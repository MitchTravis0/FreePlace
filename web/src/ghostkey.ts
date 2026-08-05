// Ghost key flow against the platform ghostkeys delegate (CBOR
// GhostkeyRequest/GhostkeyResponse over delegate messaging; types from
// ghostkey-common 0.2.4). The permission prompt is rendered by the delegate
// and runtime, not by this app: SignWithDefault itself shows the user a key
// picker when this app holds no grant yet and replays the request once they
// choose, so proving a key is one round trip with no RequestAnyAccess step.
//
// Every path resolves to one of three UI states, none of which is an error:
//   signature     — the user held a ghost key and approved; the proof fields
//                   feed AdmissionProof::Ghostkey in the registry admission.
//   no-identity   — nothing to sign with: the vault is empty or every key in
//                   it lost its signing key (NoIdentityAvailable), or this
//                   node has no ghostkeys delegate at all (a host error on
//                   the round-trip). Point the user at freenet.org/ghostkey.
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

/// Sign the challenge with the user's default ghost key (prompting for a
/// grant, or a key choice, as needed — the delegate handles both).
export async function proveGhostkey(
  client: FreenetClient,
  delegate: DelegateAddress | null,
  challenge: Uint8Array,
): Promise<GhostkeyOutcome> {
  if (!delegate || delegate.keyBytes.length === 0) {
    return { kind: "no-identity", detail: "no ghostkeys delegate configured for this build" };
  }
  let signPayloads: Uint8Array[];
  try {
    signPayloads = await delegateRoundTrip(
      client,
      delegate,
      cborEncode({ SignWithDefault: { message: Array.from(challenge) } }),
    );
  } catch (err) {
    // A node without the ghostkeys delegate reports a host error; that is the
    // "user cannot present a ghost key here" state, not a failure.
    return { kind: "no-identity", detail: `ghostkeys delegate unavailable: ${String(err)}` };
  }
  const signed = firstResponse(signPayloads);
  if (!signed) {
    // Seen on the real network: the node acks the request with a
    // DelegateResponse carrying no application messages. Same UI state as an
    // absent delegate.
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

export interface GhostkeyPresence {
  /// Ghost keys that can sign right now.
  usable: number;
  /// Certificates whose signing key is gone: the user needs their vault
  /// backup, not another purchase.
  unusable: number;
}

/// Ask whether the user's vault holds any ghost keys at all (HasIdentity:
/// never prompts, not permission-filtered). Returns null when the answer is
/// unknowable — no delegate on this node, a pre-0.2.4 delegate, or an
/// unusable response. Offer-only signal: callers decide what to OFFER from
/// it (a purchase link, a restore-your-backup hint), never whether the user
/// may ATTEMPT to sign — the answer goes stale the moment a key is bought
/// in another tab, and proveGhostkey is authoritative at the click.
export async function ghostkeyPresence(
  client: FreenetClient,
  delegate: DelegateAddress | null,
): Promise<GhostkeyPresence | null> {
  if (!delegate || delegate.keyBytes.length === 0) return null;
  let payloads: Uint8Array[];
  try {
    payloads = await delegateRoundTrip(client, delegate, cborEncode("HasIdentity"));
  } catch {
    return null;
  }
  const response = firstResponse(payloads);
  if (!response || response.variant !== "IdentityPresence") return null;
  const usable = mapGet(response.fields!, "usable");
  const unusable = mapGet(response.fields!, "unusable");
  if (typeof usable !== "number" || typeof unusable !== "number") return null;
  return { usable, unusable };
}

/// Purchase page for a new ghost key. When the app is served from a gateway
/// contract path, carry our contract instance id as return_to so the vault
/// can offer a one-click way back once the key lands (the vault only ever
/// builds a same-origin /v1/contract/web/<id>/ path from the id).
export function ghostkeyCreateUrl(pathname: string = location.pathname): string {
  const base = "https://freenet.org/ghostkey/create/";
  const match = /\/v1\/contract\/web\/([^/]+)/.exec(pathname);
  return match ? `${base}?return_to=${encodeURIComponent(match[1])}` : base;
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
