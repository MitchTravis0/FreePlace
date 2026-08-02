// Connection layer over FreenetWsApi. The WS URL is derived from the page
// location (never hardcoded) so the gateway-served app follows whatever
// host/port serves it; the dev harness may override the node host with a
// ?node=host:port query parameter because `vite dev` runs on its own origin.

import {
  ContractContainer,
  ContractKey,
  DelegateResponse,
  FreenetWsApi,
  GetRequest,
  GetResponse,
  HostError,
  PutRequest,
  StateUpdate,
  SubscribeRequest,
  UpdateRequest,
  UpdateResponse,
  UpdateData,
  UpdateDataType,
  UpdateNotification,
  DeltaUpdate,
} from "@freenetorg/freenet-stdlib";
import { RelatedContractsT } from "@freenetorg/freenet-stdlib/client-request";

export function wsApiUrl(): URL {
  const override = new URLSearchParams(location.search).get("node");
  const host = override ?? location.host;
  const proto = location.protocol === "https:" ? "wss" : "ws";
  // FreenetWsApi appends encodingProtocol (and auth token) itself.
  return new URL(`${proto}://${host}/v1/contract/command`);
}

export interface FreenetEvents {
  onOpen: () => void;
  onClose: (code: number, reason: string) => void;
  /// Subscription update, keyed by the hex of the contract instance id. Either
  /// or both of state/delta are present depending on what the node sent.
  onNotification?: (
    instanceHex: string,
    update: { state?: Uint8Array; delta?: Uint8Array },
  ) => void;
}

/// Thin wrapper owning the socket plus a queue of pending delegate
/// round-trips (delegate requests have no promise API in stdlib TS 0.2.0;
/// responses and host errors arrive through the shared handler). Responses
/// are matched to waiters by delegate key when the response carries one —
/// necessary because a request to an unregistered delegate gets no response
/// at all, and a plain FIFO would then hand the next response to the wrong
/// waiter. Host errors carry no key and fall back to oldest-first.
export class FreenetClient {
  readonly api: FreenetWsApi;
  private pendingDelegate: Array<{
    keyHex: string | null;
    resolve: (response: DelegateResponse) => void;
    reject: (err: Error) => void;
  }> = [];

  constructor(events: FreenetEvents) {
    this.api = new FreenetWsApi(
      wsApiUrl(),
      {
        onContractPut: () => {},
        onContractGet: (_response: GetResponse) => {},
        onContractUpdate: (_response: UpdateResponse) => {},
        onContractUpdateNotification: (notification: UpdateNotification) => {
          const handler = events.onNotification;
          if (!handler) return;
          const instanceHex = bytesToHex(notification.key.bytes());
          const data = notification.update?.updateData as {
            state?: number[];
            delta?: number[];
          } | null;
          switch (notification.update?.updateDataType) {
            case UpdateDataType.StateUpdate:
              handler(instanceHex, { state: Uint8Array.from(data?.state ?? []) });
              break;
            case UpdateDataType.DeltaUpdate:
              handler(instanceHex, { delta: Uint8Array.from(data?.delta ?? []) });
              break;
            case UpdateDataType.StateAndDeltaUpdate:
              handler(instanceHex, {
                state: Uint8Array.from(data?.state ?? []),
                delta: Uint8Array.from(data?.delta ?? []),
              });
              break;
          }
        },
        onContractNotFound: () => {
          this.failNextDelegate(new Error("contract not found"));
        },
        onDelegateResponse: (response: DelegateResponse) => {
          const responseKey = response.key?.key?.length ? bytesToHex(Uint8Array.from(response.key.key)) : null;
          let index = responseKey
            ? this.pendingDelegate.findIndex((e) => e.keyHex === null || e.keyHex === responseKey)
            : 0;
          if (index < 0) index = 0;
          const entry = this.pendingDelegate.splice(index, 1)[0];
          entry?.resolve(response);
        },
        onErr: (err: HostError) => {
          // Expected for e.g. a missing ghostkeys delegate; routed to the
          // waiting caller instead of the console.
          this.failNextDelegate(new Error(err.cause));
        },
        onOpen: events.onOpen,
        onClose: (code: number, reason: string) => {
          this.failNextDelegate(new Error(`connection closed: ${reason || code}`));
          events.onClose(code, reason);
        },
      },
      // Empty auth token: in the gateway iframe the shell owns auth, and the
      // sandbox blocks cookie reading.
      "",
    );
  }

  private failNextDelegate(err: Error): void {
    this.pendingDelegate.shift()?.reject(err);
  }

  /// Register a waiter for the next delegate response before sending the
  /// request, so the response cannot race the registration. `keyBytes` (the
  /// target delegate's key) scopes the waiter to that delegate's responses.
  awaitDelegateResponse(keyBytes?: number[], timeoutMs = 15_000): Promise<DelegateResponse> {
    return new Promise((resolve, reject) => {
      const entry = {
        keyHex: keyBytes?.length ? bytesToHex(Uint8Array.from(keyBytes)) : null,
        resolve: (r: DelegateResponse) => {
          clearTimeout(timer);
          resolve(r);
        },
        reject: (e: Error) => {
          clearTimeout(timer);
          reject(e);
        },
      };
      const timer = setTimeout(() => {
        const i = this.pendingDelegate.indexOf(entry);
        if (i >= 0) this.pendingDelegate.splice(i, 1);
        reject(new Error("delegate request timed out"));
      }, timeoutMs);
      this.pendingDelegate.push(entry);
    });
  }

  /// The SDK matches GET responses to requests FIFO with no correlation
  /// (freenet-core#5048); when responses can interleave, a mis-routed one
  /// must fail loudly instead of feeding another contract's state to the
  /// caller.
  private checkResponseKey(requested: ContractKey, response: GetResponse): void {
    const got = response.key?.bytes() ?? new Uint8Array();
    if (got.length > 0 && bytesToHex(got) !== bytesToHex(requested.bytes())) {
      throw new Error("mis-routed GET response (freenet-core#5048); retrying");
    }
  }

  async getState(key: ContractKey): Promise<Uint8Array> {
    const response = await this.api.get(new GetRequest(key, false));
    this.checkResponseKey(key, response);
    return Uint8Array.from(response.state);
  }

  /// GET that also fetches the contract container (code + params), needed to
  /// re-PUT a contract the node has dropped from the update mesh.
  async getStateWithContract(
    key: ContractKey,
  ): Promise<{ state: Uint8Array; contract: ContractContainer }> {
    const response = await this.api.get(new GetRequest(key, true));
    this.checkResponseKey(key, response);
    if (!response.contract) {
      throw new Error("node returned no contract code with the state");
    }
    return { state: Uint8Array.from(response.state), contract: response.contract };
  }

  /// Full-state PUT (upsert): peers holding state re-validate and merge it.
  /// `relatedContracts` is a required field in the Put flatbuffer — leaving
  /// it null makes pack() throw "field 8 must be set" (found live 2026-08-02);
  /// an empty list is the correct "none" value.
  async putState(contract: ContractContainer, state: Uint8Array): Promise<void> {
    await this.api.put(new PutRequest(contract, Array.from(state), new RelatedContractsT([])));
  }

  async updateWithDelta(key: ContractKey, delta: Uint8Array): Promise<void> {
    const update = new UpdateData(UpdateDataType.DeltaUpdate, new DeltaUpdate(Array.from(delta)));
    await this.api.update(new UpdateRequest(key, update));
  }

  /// Full-state merge update; the contract's update path re-validates every
  /// byte. Used by the migration probe to fold a previous release's state
  /// into the current instance.
  async updateWithState(key: ContractKey, state: Uint8Array): Promise<void> {
    const update = new UpdateData(UpdateDataType.StateUpdate, new StateUpdate(Array.from(state)));
    await this.api.update(new UpdateRequest(key, update));
  }

  async subscribe(key: ContractKey): Promise<void> {
    await this.api.subscribe(new SubscribeRequest(key, []));
  }
}

function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

/// Build a ContractKey from a base58 contract instance id (as printed by
/// `fdev get-contract-id`).
export function contractKeyFromId(instanceId: string): ContractKey {
  const instanceBytes = ContractKey.fromInstanceId(instanceId).bytes();
  return new ContractKey(instanceBytes, instanceBytes);
}
