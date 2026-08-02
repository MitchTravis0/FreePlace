// Regression for the PUT fallback's real serialization path (found live
// 2026-08-02): `Put.relatedContracts` is a required flatbuffer field, so
// building a PutRequest without it makes pack() throw "FlatBuffers: field 8
// must be set" — a failure no fake-channel test catches. This runs the actual
// FreenetClient GET(+contract)/PUT round-trip against the dev node, so it
// exercises unpacking the node's container and re-packing it into a PUT.
// Node-gated like the other smoke-tier specs: runs under
// scripts/phase5-smoke.sh (which publishes the fixtures and sets WS_API_HOST).

import { readFileSync } from "node:fs";
import { expect, test } from "@playwright/test";

const NODE = process.env.WS_API_HOST;

test("getStateWithContract + putState round-trip against the node", async ({ page }) => {
  test.skip(!NODE, "WS_API_HOST not set (run via scripts/phase5-smoke.sh)");
  const registryId = readFileSync(".gen/registry_contract_id.txt", "utf8").trim();

  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  await page.goto(`/?mock=1&node=${NODE}`);

  const result = await page.evaluate(
    async ({ registryId }) => {
      const api = (await import("/src/freenet-api.ts")) as unknown as {
        FreenetClient: new (events: {
          onOpen: () => void;
          onClose: (code: number, reason: string) => void;
        }) => {
          api: unknown;
          getStateWithContract(key: unknown): Promise<{ state: Uint8Array; contract: unknown }>;
          putState(contract: unknown, state: Uint8Array): Promise<void>;
        };
        contractKeyFromId(id: string): unknown;
      };
      let onOpen!: () => void;
      const opened = new Promise<void>((resolve) => (onOpen = resolve));
      const client = new api.FreenetClient({
        onOpen,
        onClose: () => {},
      });
      await opened;
      const key = api.contractKeyFromId(registryId);
      const got = await client.getStateWithContract(key);
      const codeLen = (got.contract as { contract?: { data?: { data?: number[] } } }).contract
        ?.data?.data?.length;
      // Re-PUT the identical state. Packing (where the required-field bug
      // threw) happens synchronously before ws.send; the dev node does not
      // ack a re-PUT, so capture the outgoing bytes instead of awaiting the
      // response — a pack failure sends nothing.
      let sentBytes = 0;
      const ws = (client.api as { ws: WebSocket }).ws;
      ws.send = (data: ArrayBufferLike | ArrayBufferView) => {
        sentBytes += "byteLength" in data ? data.byteLength : 0;
      };
      let putError: string | null = null;
      try {
        void client.putState(got.contract, got.state).catch(() => {});
        // One microtask turn is enough: pack + send are synchronous inside
        // putState before it awaits the response.
        await Promise.resolve();
      } catch (err) {
        putError = String(err);
      }
      return { stateLen: got.state.length, codeLen, sentBytes, putError };
    },
    { registryId },
  );

  expect(result.putError).toBeNull();
  // The GET must have carried the full container (code + params + key)...
  expect(result.codeLen ?? 0).toBeGreaterThan(1000);
  // ...and the PUT must have packed and reached the socket, container
  // included (the SDK chunks payloads this large).
  expect(result.sentBytes).toBeGreaterThan(result.codeLen ?? 0);
  expect(errors).toEqual([]);
});
