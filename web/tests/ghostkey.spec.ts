// Regression test (found live at launch): the real-network node answers the
// ghost key request with a DelegateResponse that carries no application
// messages, and proveGhostkey threw "returned no response" instead of
// resolving to the "no-identity" UI state its contract promises (every path
// is a UI state, never an error). The dev node never hit this: an absent
// delegate there produces no response at all, which the round-trip timeout
// already maps to no-identity.
//
// ghostkey.ts is imported directly from the vite dev server and driven with
// a stub client, since MockBackend fakes requestGhostkey above this seam.

import { expect, test } from "@playwright/test";

test("an empty ghostkeys delegate response resolves to no-identity, not an error", async ({
  page,
}) => {
  await page.goto("/?mock=1");
  const outcome = await page.evaluate(async () => {
    const module = (await import("/src/ghostkey.ts")) as unknown as {
      proveGhostkey(
        client: unknown,
        delegate: { keyBytes: number[]; codeHashBytes: number[] },
        challenge: Uint8Array,
      ): Promise<{ kind: string; detail?: string }>;
    };
    const stubClient = {
      awaitDelegateResponse: () => Promise.resolve({ values: [] }),
      api: { sendRequest() {} },
    };
    return module.proveGhostkey(
      stubClient,
      { keyBytes: [1, 2], codeHashBytes: [3, 4] },
      new Uint8Array(32),
    );
  });
  expect(outcome.kind).toBe("no-identity");
  expect(outcome.detail).toContain("no usable response");
});
