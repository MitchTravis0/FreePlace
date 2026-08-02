// Regression for "identity delegate returned no response" on every node
// except the publisher's (found live 2026-08-02): delegates never propagate
// over the network, so the UI must hand the node the delegate WASM itself
// via RegisterDelegate. This drives the real registerDelegate() against the
// dev node and asserts the node acks with the delegate key computed at build
// time (the node recomputes it from the WASM bytes, so a matching ack proves
// the registration actually installed our delegate), then that an identity
// round-trip works. Node-gated like the other smoke-tier specs: runs under
// scripts/phase5-smoke.sh (which also fdev-publishes the delegate, so this
// exercises idempotent re-registration; the fresh-node path is
// scripts/phase6-smoke.sh, which no longer publishes the delegate at all).

import { readFileSync } from "node:fs";
import { expect, test } from "@playwright/test";

const NODE = process.env.WS_API_HOST;

test("UI-side RegisterDelegate is acked with the build-time delegate key", async ({ page }) => {
  test.skip(!NODE, "WS_API_HOST not set (run via scripts/phase5-smoke.sh)");
  const keyBytes = JSON.parse(readFileSync(".gen/identity_delegate_key_bytes.json", "utf8"));
  const codeHashBytes = JSON.parse(
    readFileSync(".gen/identity_delegate_code_hash_bytes.json", "utf8"),
  );
  const wasmB64 = readFileSync(".gen/identity_delegate.wasm").toString("base64");

  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  await page.goto(`/?mock=1&node=${NODE}`);

  const result = await page.evaluate(
    async ({ keyBytes, codeHashBytes, wasmB64 }) => {
      const freenet = (await import("/src/freenet-api.ts")) as unknown as {
        FreenetClient: new (events: {
          onOpen: () => void;
          onClose: (code: number, reason: string) => void;
        }) => object;
      };
      const delegates = (await import("/src/delegate-api.ts")) as unknown as {
        base64ToBytes(b64: string): Uint8Array;
        registerDelegate(
          client: object,
          address: { keyBytes: number[]; codeHashBytes: number[] },
          wasm: Uint8Array,
        ): Promise<Uint8Array>;
      };
      const identityMod = (await import("/src/identity.ts")) as unknown as {
        IdentityClient: new (
          client: object,
          address: { keyBytes: number[]; codeHashBytes: number[] },
        ) => { getIdentity(): Promise<Uint8Array> };
      };
      let onOpen!: () => void;
      const opened = new Promise<void>((resolve) => (onOpen = resolve));
      const client = new freenet.FreenetClient({ onOpen, onClose: () => {} });
      await opened;
      const address = { keyBytes, codeHashBytes };
      const ackKey = await delegates.registerDelegate(
        client,
        address,
        delegates.base64ToBytes(wasmB64),
      );
      const vk = await new identityMod.IdentityClient(client, address).getIdentity();
      return { ackKey: Array.from(ackKey), vkLen: vk.length };
    },
    { keyBytes, codeHashBytes, wasmB64 },
  );

  expect(result.ackKey).toEqual(keyBytes);
  expect(result.vkLen).toBe(32);
  expect(errors).toEqual([]);
});
