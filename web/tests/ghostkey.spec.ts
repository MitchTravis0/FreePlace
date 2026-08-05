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

// ghostkey-common 0.2.4: SignWithDefault handles the no-grant case itself
// (key picker + replay inside the delegate), so proving a key is ONE round
// trip with no RequestAnyAccess step. Pinned by capturing every request the
// stub client is asked to send.
test("proveGhostkey is a single SignWithDefault round trip", async ({ page }) => {
  await page.goto("/?mock=1");
  const result = await page.evaluate(async () => {
    const ghost = (await import("/src/ghostkey.ts")) as unknown as {
      proveGhostkey(
        client: unknown,
        delegate: { keyBytes: number[]; codeHashBytes: number[] },
        challenge: Uint8Array,
      ): Promise<{ kind: string; certificatePem?: string }>;
    };
    const cbor = (await import("/src/cbor.ts")) as unknown as {
      cborEncode(value: unknown): Uint8Array;
      cborDecode(bytes: Uint8Array): unknown;
      enumVariant(value: unknown): { variant: string; fields: Map<string, unknown> | null };
    };

    // Collect the CBOR payloads of every delegate request sent, by walking
    // the FlatBuffers table object graph for ApplicationMessage payloads.
    const sentPayloads: number[][] = [];
    const seen = new Set<object>();
    const walk = (value: unknown): void => {
      if (!value || typeof value !== "object" || seen.has(value)) return;
      seen.add(value);
      const record = value as Record<string, unknown>;
      if (
        Array.isArray(record.payload) &&
        record.payload.length > 0 &&
        record.payload.every((b) => typeof b === "number")
      ) {
        sentPayloads.push(record.payload as number[]);
      }
      for (const child of Object.values(record)) {
        if (Array.isArray(child)) child.forEach(walk);
        else walk(child);
      }
    };

    const responseBytes = cbor.cborEncode({
      SignResult: {
        scoped_payload: new Uint8Array([9, 9]),
        signature: new Uint8Array([7, 7, 7]),
        certificate_pem: "stub pem",
      },
    });
    const stubClient = {
      awaitDelegateResponse: () =>
        Promise.resolve({
          values: [{ inboundType: 1, inbound: { payload: Array.from(responseBytes) } }],
        }),
      api: {
        sendRequest(req: unknown) {
          walk(req);
        },
      },
    };

    const challenge = new Uint8Array([1, 2, 3, 4]);
    const outcome = await ghost.proveGhostkey(
      stubClient,
      { keyBytes: [1, 2], codeHashBytes: [3, 4] },
      challenge,
    );
    const decoded = sentPayloads.map((payload) =>
      cbor.enumVariant(cbor.cborDecode(Uint8Array.from(payload))),
    );
    return {
      outcome,
      requestVariants: decoded.map((d) => d.variant),
      firstMessage: decoded[0]?.fields?.get("message"),
    };
  });
  expect(result.requestVariants).toEqual(["SignWithDefault"]);
  expect(result.firstMessage).toEqual([1, 2, 3, 4]);
  expect(result.outcome.kind).toBe("signature");
  expect(result.outcome.certificatePem).toBe("stub pem");
});

test("ghostkeyPresence parses IdentityPresence and maps unknowns to null", async ({ page }) => {
  await page.goto("/?mock=1");
  const result = await page.evaluate(async () => {
    const ghost = (await import("/src/ghostkey.ts")) as unknown as {
      ghostkeyPresence(
        client: unknown,
        delegate: { keyBytes: number[]; codeHashBytes: number[] },
      ): Promise<{ usable: number; unusable: number } | null>;
    };
    const cbor = (await import("/src/cbor.ts")) as unknown as {
      cborEncode(value: unknown): Uint8Array;
    };
    const address = { keyBytes: [1, 2], codeHashBytes: [3, 4] };
    const clientAnswering = (values: unknown[]) => ({
      awaitDelegateResponse: () => Promise.resolve({ values }),
      api: { sendRequest() {} },
    });

    const presenceBytes = cbor.cborEncode(
      new Map([["IdentityPresence", { usable: 2, unusable: 1 }]]),
    );
    const answered = await ghost.ghostkeyPresence(
      clientAnswering([{ inboundType: 1, inbound: { payload: Array.from(presenceBytes) } }]),
      address,
    );
    // The real-network node acks an absent/old delegate with an empty
    // DelegateResponse: that must read as "unknowable", never as "no keys".
    const emptyAck = await ghost.ghostkeyPresence(clientAnswering([]), address);
    return { answered, emptyAck };
  });
  expect(result.answered).toEqual({ usable: 2, unusable: 1 });
  expect(result.emptyAck).toBeNull();
});

test("ghostkeyCreateUrl carries return_to only under a gateway contract path", async ({
  page,
}) => {
  await page.goto("/?mock=1");
  const result = await page.evaluate(async () => {
    const ghost = (await import("/src/ghostkey.ts")) as unknown as {
      ghostkeyCreateUrl(pathname?: string): string;
    };
    return {
      gateway: ghost.ghostkeyCreateUrl("/v1/contract/web/AbCd123xyz/index.html"),
      standalone: ghost.ghostkeyCreateUrl("/"),
    };
  });
  expect(result.gateway).toBe("https://freenet.org/ghostkey/create/?return_to=AbCd123xyz");
  expect(result.standalone).toBe("https://freenet.org/ghostkey/create/");
});
