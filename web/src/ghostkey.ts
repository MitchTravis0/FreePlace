// Ghost key flow against the platform ghostkeys delegate (CBOR
// GhostkeyRequest/GhostkeyResponse over delegate messaging; types from
// ghostkey-common 0.2.3). The permission prompt is rendered by the delegate
// and runtime, not by this app.
//
// Every path resolves to one of three UI states, none of which is an error:
//   signature     — the user held a ghost key and approved; the proof fields
//                   feed AdmissionProof::Ghostkey in the registry admission.
//   no-identity   — the user has no ghost key (NoIdentityAvailable), or this
//                   node has no ghostkeys delegate at all (a host error on the
//                   round-trip). Point the user at freenet.org/ghostkey.
//   access-denied — the user declined the delegate's prompt.

import { cborDecode, cborEncode, enumVariant, mapGet } from "./cbor";
import type { DelegateAddress } from "./delegate-api";
import { delegateRoundTrip } from "./delegate-api";
import type { FreenetClient } from "./freenet-api";
import { asByteArray } from "./identity";

export type GhostkeyOutcome =
  | {
      kind: "signature";
      scopedPayload: Uint8Array;
      signature: Uint8Array;
      certificatePem: string;
    }
  | { kind: "no-identity"; detail: string }
  | { kind: "access-denied" };

/// Ask for access to any of the user's ghost keys, then sign the challenge
/// with the default key.
export async function proveGhostkey(
  client: FreenetClient,
  delegate: DelegateAddress | null,
  challenge: Uint8Array,
): Promise<GhostkeyOutcome> {
  if (!delegate || delegate.keyBytes.length === 0) {
    return { kind: "no-identity", detail: "no ghostkeys delegate configured for this build" };
  }
  let payloads: Uint8Array[];
  try {
    payloads = await delegateRoundTrip(client, delegate, cborEncode("RequestAnyAccess"));
  } catch (err) {
    // A node without the ghostkeys delegate reports a host error; that is the
    // "user cannot present a ghost key here" state, not a failure.
    return { kind: "no-identity", detail: `ghostkeys delegate unavailable: ${String(err)}` };
  }
  const access = firstResponse(payloads);
  if (!access) {
    // Seen on the real network: the node acks the request with a
    // DelegateResponse carrying no application messages. Same UI state as an
    // absent delegate.
    return { kind: "no-identity", detail: "ghostkeys delegate sent no usable response" };
  }
  switch (access.variant) {
    case "GhostKeyList":
      break; // access granted to at least one key
    case "NoIdentityAvailable":
      return { kind: "no-identity", detail: "no ghost key stored; see freenet.org/ghostkey" };
    case "AccessDenied":
      return { kind: "access-denied" };
    default:
      return { kind: "no-identity", detail: `unexpected access response: ${access.variant}` };
  }

  let signPayloads: Uint8Array[];
  try {
    signPayloads = await delegateRoundTrip(
      client,
      delegate,
      cborEncode({ SignWithDefault: { message: Array.from(challenge) } }),
    );
  } catch (err) {
    return { kind: "no-identity", detail: `ghost key signing failed: ${String(err)}` };
  }
  const signed = firstResponse(signPayloads);
  if (!signed) {
    return { kind: "no-identity", detail: "ghostkeys delegate sent no usable response" };
  }
  switch (signed.variant) {
    case "SignResult":
      return {
        kind: "signature",
        scopedPayload: asByteArray(mapGet(signed.fields!, "scoped_payload")),
        signature: asByteArray(mapGet(signed.fields!, "signature")),
        certificatePem: String(mapGet(signed.fields!, "certificate_pem")),
      };
    case "AccessDenied":
    case "PermissionDenied":
      return { kind: "access-denied" };
    case "NoIdentityAvailable":
      return { kind: "no-identity", detail: "no ghost key stored; see freenet.org/ghostkey" };
    default:
      return { kind: "no-identity", detail: `unexpected sign response: ${signed.variant}` };
  }
}

function firstResponse(
  payloads: Uint8Array[],
): { variant: string; fields: ReturnType<typeof cborDecode> | null } | null {
  if (payloads.length === 0) return null;
  try {
    return enumVariant(cborDecode(payloads[0]));
  } catch {
    return null;
  }
}
