// Client for the identity delegate: CBOR requests/responses over delegate
// messaging (protocol pinned in common/src/delegate_protocol.rs). The private
// key never reaches the page — responses carry the verifying key and
// ready-to-send contract delta bytes.

import { cborDecode, cborEncode, CborValue, enumVariant, mapGet } from "./cbor";
import type { DelegateAddress } from "./delegate-api";
import { delegateRoundTrip } from "./delegate-api";
import type { FreenetClient } from "./freenet-api";
import { contractKeyFromId } from "./freenet-api";

export interface SignedDelta {
  verifyingKey: Uint8Array;
  delta: Uint8Array;
}

export interface AdmissionChallenge {
  bytes: Uint8Array;
  difficultyBits: number;
}

export class IdentityClient {
  constructor(
    private readonly client: FreenetClient,
    private readonly delegate: DelegateAddress,
  ) {}

  private async request(request: CborValue): Promise<{ variant: string; fields: CborValue | null }> {
    const payloads = await delegateRoundTrip(this.client, this.delegate, cborEncode(request));
    if (payloads.length === 0) throw new Error("identity delegate returned no response");
    const response = enumVariant(cborDecode(payloads[0]));
    if (response.variant === "Error") {
      throw new Error(`identity delegate: ${asString(mapGet(response.fields!, "message"))}`);
    }
    return response;
  }

  async getIdentity(): Promise<Uint8Array> {
    const response = await this.request("GetIdentity");
    expectVariant(response.variant, "Identity");
    return asByteArray(mapGet(response.fields!, "verifying_key"));
  }

  /// Resolve this origin's identity, first trying to adopt one left under a
  /// previous release's web-container id (newest first), so identities
  /// survive the per-release container re-key. Falls back to GetIdentity,
  /// which creates a fresh key.
  async adoptOrGetIdentity(legacyWebappIds: string[]): Promise<Uint8Array> {
    for (const id of legacyWebappIds) {
      try {
        const response = await this.request({
          AdoptLegacyOrigin: { old_webapp_id: Array.from(contractKeyFromId(id).bytes()) },
        });
        expectVariant(response.variant, "AdoptResult");
        const vk = mapGet(response.fields!, "verifying_key");
        if (vk !== null && vk !== undefined) {
          if (mapGet(response.fields!, "adopted") === true) {
            console.info(`freeplace identity: adopted from previous release container ${id}`);
          }
          return asByteArray(vk);
        }
      } catch (err) {
        console.info(`freeplace identity: adoption probe for ${id} failed: ${err}`);
      }
    }
    return this.getIdentity();
  }

  async admissionChallenge(registryParams: Uint8Array): Promise<AdmissionChallenge> {
    const response = await this.request({
      AdmissionChallenge: { registry_params: Array.from(registryParams) },
    });
    expectVariant(response.variant, "Challenge");
    return {
      bytes: asByteArray(mapGet(response.fields!, "bytes")),
      difficultyBits: asNumber(mapGet(response.fields!, "difficulty_bits")),
    };
  }

  async signPowAdmission(
    registryParams: Uint8Array,
    nonce: number,
    admittedTs: number,
    nickname: string | null,
  ): Promise<SignedDelta> {
    const response = await this.request({
      SignAdmission: {
        registry_params: Array.from(registryParams),
        proof: { Work: { nonce } },
        admitted_ts: admittedTs,
        nickname,
      },
    });
    return signedDelta(response, "RegistryUpdate");
  }

  async signGhostkeyAdmission(
    registryParams: Uint8Array,
    proof: { scopedPayload: Uint8Array; signature: Uint8Array; certificatePem: string },
    admittedTs: number,
    nickname: string | null,
  ): Promise<SignedDelta> {
    const response = await this.request({
      SignAdmission: {
        registry_params: Array.from(registryParams),
        proof: {
          Ghostkey: {
            scoped_payload: Array.from(proof.scopedPayload),
            signature: Array.from(proof.signature),
            certificate_pem: proof.certificatePem,
          },
        },
        admitted_ts: admittedTs,
        nickname,
      },
    });
    return signedDelta(response, "RegistryUpdate");
  }

  async signPlacement(
    tileParams: Uint8Array,
    coord: number,
    color: number,
    ts: number,
  ): Promise<SignedDelta> {
    const response = await this.request({
      SignPlacement: { tile_params: Array.from(tileParams), coord, color, ts },
    });
    return signedDelta(response, "TileUpdate");
  }

  async signChatMessage(
    chatParams: Uint8Array,
    content: string,
    ts: number,
    seq: number,
  ): Promise<SignedDelta> {
    const response = await this.request({
      SignChatMessage: { chat_params: Array.from(chatParams), content, ts, seq },
    });
    return signedDelta(response, "ChatUpdate");
  }

  async signNickname(registryParams: Uint8Array, name: string, version: number): Promise<SignedDelta> {
    const response = await this.request({
      SignNickname: { registry_params: Array.from(registryParams), name, version },
    });
    return signedDelta(response, "RegistryUpdate");
  }
}

function signedDelta(
  response: { variant: string; fields: CborValue | null },
  expected: string,
): SignedDelta {
  expectVariant(response.variant, expected);
  return {
    verifyingKey: asByteArray(mapGet(response.fields!, "verifying_key")),
    delta: asByteArray(mapGet(response.fields!, "delta")),
  };
}

function expectVariant(got: string, expected: string): void {
  if (got !== expected) throw new Error(`expected ${expected} response, got ${got}`);
}

function asString(value: CborValue | undefined): string {
  if (typeof value !== "string") throw new Error("expected a string field");
  return value;
}

function asNumber(value: CborValue | undefined): number {
  if (typeof value !== "number") throw new Error("expected a numeric field");
  return value;
}

/// Byte payloads arrive either as a CBOR byte string or as an array of ints
/// (serde's Vec<u8>/[u8; N] encode as arrays under ciborium).
export function asByteArray(value: CborValue | undefined): Uint8Array {
  if (value instanceof Uint8Array) return value;
  if (Array.isArray(value) && value.every((v) => typeof v === "number")) {
    return Uint8Array.from(value as number[]);
  }
  throw new Error("expected a byte-array field");
}
