// Delegate messaging over the raw FlatBuffers table types. This module is
// the ONLY place allowed to touch `(api as any).sendRequest` — a private SDK
// method used because stdlib TS 0.2.0 has no public delegate-request builder.
// Known-unstable seam: may break on any minor SDK bump; track freenet-stdlib
// for a public equivalent.

import { DelegateRequest, DelegateResponse } from "@freenetorg/freenet-stdlib";
import { ApplicationMessageT } from "@freenetorg/freenet-stdlib/common";
import {
  ApplicationMessagesT,
  ClientRequestT,
  ClientRequestType,
  DelegateCodeT,
  DelegateContainerT,
  DelegateKeyT,
  DelegateRequestType,
  DelegateType,
  InboundDelegateMsgT,
  InboundDelegateMsgType,
  RegisterDelegateT,
  WasmDelegateV1T,
} from "@freenetorg/freenet-stdlib/client-request";

import type { FreenetClient } from "./freenet-api";

export interface DelegateAddress {
  /// blake3(code_hash || params) — the node's lookup key.
  keyBytes: number[];
  /// blake3(raw wasm bytes) — a DIFFERENT hash; both are required.
  codeHashBytes: number[];
}

/// Decode the base64 the build embeds delegate WASM as (atob: the gateway
/// iframe's opaque origin makes fetching asset URLs unreliable, so binaries
/// ship inline).
export function base64ToBytes(b64: string): Uint8Array {
  const raw = atob(b64);
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
  return bytes;
}

/// Register a delegate's WASM on the connected node. Delegates never
/// propagate over the network, so every user's node must be handed the WASM
/// by the UI (River's pattern); registration is idempotent on the node side
/// (re-registering the same bytes is a no-op success), and the node acks
/// with an empty DelegateResponse carrying the delegate key. The node
/// recomputes the key from (code, params); the key fields sent here are
/// required by the schema but not trusted. Cipher/nonce are required 32/24
/// byte fields the node has ignored since freenet-core#4140. Resolves with
/// the delegate key bytes from the ack.
export async function registerDelegate(
  client: FreenetClient,
  address: DelegateAddress,
  wasm: Uint8Array,
): Promise<Uint8Array> {
  const code = new DelegateCodeT(Array.from(wasm), address.codeHashBytes);
  const key = new DelegateKeyT(address.keyBytes, address.codeHashBytes);
  const container = new DelegateContainerT(
    DelegateType.WasmDelegateV1,
    new WasmDelegateV1T([], code, key),
  );
  const cipher = new Uint8Array(32);
  const nonce = new Uint8Array(24);
  crypto.getRandomValues(cipher);
  crypto.getRandomValues(nonce);
  const register = new RegisterDelegateT(container, Array.from(cipher), Array.from(nonce));
  const delegateReq = new DelegateRequest(DelegateRequestType.RegisterDelegate, register);
  const clientReq = new ClientRequestT(ClientRequestType.DelegateRequest, delegateReq);

  const pending = client.awaitDelegateResponse(address.keyBytes);
  // Private SDK method — see the module header before touching this.
  (client.api as unknown as { sendRequest(req: ClientRequestT): void }).sendRequest(clientReq);
  const ack = await pending;
  return Uint8Array.from(ack.key?.key ?? []);
}

/// Send one ApplicationMessage payload to a delegate and resolve with the
/// payloads of the ApplicationMessages it responds with.
export async function delegateRoundTrip(
  client: FreenetClient,
  address: DelegateAddress,
  payload: Uint8Array,
): Promise<Uint8Array[]> {
  const appMsg = new ApplicationMessageT(Array.from(payload), [], false);
  const inbound = new InboundDelegateMsgT(InboundDelegateMsgType.common_ApplicationMessage, appMsg);
  const delegateKey = new DelegateKeyT(address.keyBytes, address.codeHashBytes);
  const appMessages = new ApplicationMessagesT(delegateKey, [], [inbound]);
  const delegateReq = new DelegateRequest(DelegateRequestType.ApplicationMessages, appMessages);
  const clientReq = new ClientRequestT(ClientRequestType.DelegateRequest, delegateReq);

  const pending = client.awaitDelegateResponse(address.keyBytes);
  // Private SDK method — see the module header before touching this.
  (client.api as unknown as { sendRequest(req: ClientRequestT): void }).sendRequest(clientReq);
  return applicationMessagePayloads(await pending);
}

const OUTBOUND_APPLICATION_MESSAGE = 1; // OutboundDelegateMsgType.common_ApplicationMessage

function applicationMessagePayloads(response: DelegateResponse): Uint8Array[] {
  const payloads: Uint8Array[] = [];
  for (const outbound of response.values ?? []) {
    if (outbound.inboundType !== OUTBOUND_APPLICATION_MESSAGE) continue;
    const msg = outbound.inbound as { payload?: number[] } | null;
    if (msg?.payload?.length) payloads.push(Uint8Array.from(msg.payload));
  }
  return payloads;
}
