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
  DelegateKeyT,
  DelegateRequestType,
  InboundDelegateMsgT,
  InboundDelegateMsgType,
} from "@freenetorg/freenet-stdlib/client-request";

import type { FreenetClient } from "./freenet-api";

export interface DelegateAddress {
  /// blake3(code_hash || params) — the node's lookup key.
  keyBytes: number[];
  /// blake3(raw wasm bytes) — a DIFFERENT hash; both are required.
  codeHashBytes: number[];
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
