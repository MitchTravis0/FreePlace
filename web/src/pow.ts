// Proof-of-work grind: find a nonce whose blake3(challenge || nonce_le)
// digest has at least `difficultyBits` leading zero bits. Matches
// common::registry::pow_digest / pow_nonce_meets_target. Phase 6 moves this
// into a Web Worker with progress reporting; the algorithm stays the same.

import { blake3 } from "@noble/hashes/blake3.js";

export function leadingZeroBits(digest: Uint8Array): number {
  let bits = 0;
  for (const byte of digest) {
    if (byte === 0) {
      bits += 8;
    } else {
      bits += Math.clz32(byte) - 24;
      break;
    }
  }
  return bits;
}

export function powDigest(challenge: Uint8Array, nonce: number): Uint8Array {
  const input = new Uint8Array(challenge.length + 8);
  input.set(challenge);
  // Nonce as u64 little-endian; the grind stays far below 2^53.
  new DataView(input.buffer).setBigUint64(challenge.length, BigInt(nonce), true);
  return blake3(input);
}

export function grindPowNonce(challenge: Uint8Array, difficultyBits: number): number {
  for (let nonce = 0; ; nonce++) {
    if (leadingZeroBits(powDigest(challenge, nonce)) >= difficultyBits) {
      return nonce;
    }
  }
}
