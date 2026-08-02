// Web Worker running the proof-of-work grind off the main thread, posting
// progress so the onboarding UI can show a live counter.

import { leadingZeroBits, powDigest } from "./pow";

export interface PowWorkerRequest {
  challenge: Uint8Array;
  difficultyBits: number;
}

export type PowWorkerMessage = { type: "progress"; tried: number } | { type: "done"; nonce: number };

const PROGRESS_CHUNK = 2048;

// The project compiles against the DOM lib; type the worker global locally so
// the one-argument postMessage form checks.
const scope = self as unknown as {
  onmessage: ((event: MessageEvent<PowWorkerRequest>) => void) | null;
  postMessage(message: PowWorkerMessage): void;
};

scope.onmessage = (event) => {
  const { challenge, difficultyBits } = event.data;
  let nonce = 0;
  for (;;) {
    for (let i = 0; i < PROGRESS_CHUNK; i++, nonce++) {
      if (leadingZeroBits(powDigest(challenge, nonce)) >= difficultyBits) {
        scope.postMessage({ type: "done", nonce });
        return;
      }
    }
    scope.postMessage({ type: "progress", tried: nonce });
  }
};
