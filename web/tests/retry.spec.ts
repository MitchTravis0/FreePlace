// Regression test (found live at launch): the TS SDK's request/response
// matching is FIFO with no correlation (freenet-core#5048), so an update
// whose response goes missing leaves its promise pending forever and the UI
// froze at "nonce found; admitting". Every update attempt now runs under
// withTimeout so retry loops keep moving and the failure surfaces.

import { expect, test } from "@playwright/test";

test("withTimeout rejects a never-settling promise instead of hanging", async ({ page }) => {
  await page.goto("/?mock=1");
  const result = await page.evaluate(async () => {
    const module = (await import("/src/backend.ts")) as unknown as {
      withTimeout<T>(promise: Promise<T>, ms: number, label: string): Promise<T>;
    };
    const hung = new Promise(() => {});
    try {
      await module.withTimeout(hung, 50, "contract update");
      return "resolved";
    } catch (err) {
      return String(err);
    }
  });
  expect(result).toContain("contract update timed out after 50ms");
});

// Second live regression (2026-08-01): on the real network a low-traffic
// contract falls out of the update mesh — every UPDATE fails with "missing
// contract" (freenet-core#5069) while GET and PUT keep working, so no amount
// of UPDATE retries can land an admission. updateOrPut falls back to
// GET (+ contract code) -> merge -> re-PUT, and goes PUT-first once a key is
// known broken.
test("updateOrPut falls back to a merged re-PUT when UPDATE fails", async ({ page }) => {
  await page.goto("/?mock=1");
  const result = await page.evaluate(async () => {
    const module = (await import("/src/backend.ts")) as unknown as {
      updateOrPut(
        channel: unknown,
        key: unknown,
        delta: Uint8Array,
        merge: (state: Uint8Array, delta: Uint8Array) => Uint8Array,
        caches: { containers: Map<string, unknown>; putFirst: Set<string> },
      ): Promise<void>;
    };
    const key = { bytes: () => Uint8Array.of(1, 2, 3) };
    const calls: string[] = [];
    let putBytes: number[] = [];
    const channel = {
      updateWithDelta: async () => {
        calls.push("update");
        throw new Error("UPDATE failed: missing contract");
      },
      getStateWithContract: async () => {
        calls.push("getWithContract");
        return { state: Uint8Array.of(10), contract: "container" };
      },
      getState: async () => {
        calls.push("get");
        return Uint8Array.of(10);
      },
      putState: async (_contract: unknown, state: Uint8Array) => {
        calls.push("put");
        putBytes = Array.from(state);
      },
    };
    const caches = { containers: new Map<string, unknown>(), putFirst: new Set<string>() };
    const merge = (state: Uint8Array, delta: Uint8Array) => Uint8Array.of(...state, ...delta);
    await module.updateOrPut(channel, key, Uint8Array.of(20), merge, caches);
    const first = [...calls];
    calls.length = 0;
    // Key is now known broken: the next attempt goes PUT-first (no 20s
    // UPDATE wait) and reuses the cached container (plain GET, no code).
    await module.updateOrPut(channel, key, Uint8Array.of(21), merge, caches);
    return { first, second: calls, putBytes, putFirstSize: caches.putFirst.size };
  });
  expect(result.first).toEqual(["update", "getWithContract", "put"]);
  expect(result.putBytes).toEqual([10, 21]);
  expect(result.second).toEqual(["get", "put"]);
  expect(result.putFirstSize).toBe(1);
});

test("updateOrPut does not PUT when UPDATE succeeds", async ({ page }) => {
  await page.goto("/?mock=1");
  const calls = await page.evaluate(async () => {
    const module = (await import("/src/backend.ts")) as unknown as {
      updateOrPut(
        channel: unknown,
        key: unknown,
        delta: Uint8Array,
        merge: (state: Uint8Array, delta: Uint8Array) => Uint8Array,
        caches: { containers: Map<string, unknown>; putFirst: Set<string> },
      ): Promise<void>;
    };
    const key = { bytes: () => Uint8Array.of(4) };
    const log: string[] = [];
    const channel = {
      updateWithDelta: async () => {
        log.push("update");
      },
      getStateWithContract: async () => {
        log.push("getWithContract");
        return { state: new Uint8Array(), contract: "container" };
      },
      getState: async () => {
        log.push("get");
        return new Uint8Array();
      },
      putState: async () => {
        log.push("put");
      },
    };
    await module.updateOrPut(
      channel,
      key,
      new Uint8Array(),
      (state) => state,
      { containers: new Map(), putFirst: new Set() },
    );
    return log;
  });
  expect(calls).toEqual(["update"]);
});
