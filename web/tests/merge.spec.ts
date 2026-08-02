// Cross-language lock for the PUT-fallback state merge (web/src/merge.ts):
// every fixture case was merged by the Rust CRDT implementations
// (`release_tool merge-fixtures`), and the TS merge must reproduce the merged
// state byte-for-byte — the merged bytes are re-PUT to the network, where the
// contract re-validates them. Regenerate the fixtures with
//   cargo run -p common --example release_tool -- merge-fixtures \
//     web/tests/fixtures/merge-fixtures.json

import { readFileSync } from "node:fs";
import { expect, test } from "@playwright/test";

interface MergeCase {
  name: string;
  state: string;
  delta: string;
  merged: string;
}

const fixtures = JSON.parse(readFileSync("tests/fixtures/merge-fixtures.json", "utf8")) as Record<
  string,
  MergeCase[]
>;

test("TS state merges match the Rust merges byte-for-byte", async ({ page }) => {
  await page.goto("/?mock=1");
  const results = await page.evaluate(async (cases) => {
    const merge = (await import("/src/merge.ts")) as unknown as Record<
      string,
      (state: Uint8Array, delta: Uint8Array) => Uint8Array
    >;
    const mergeFns: Record<string, (state: Uint8Array, delta: Uint8Array) => Uint8Array> = {
      registry: merge.mergeRegistryState,
      tile: merge.mergeTileState,
      chat: merge.mergeChatState,
    };
    const fromHex = (h: string) =>
      Uint8Array.from(h.match(/../g) ?? [], (byte) => parseInt(byte, 16));
    const toHex = (bytes: Uint8Array) =>
      Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
    const out: { label: string; expected: string; actual: string }[] = [];
    for (const [kind, kindCases] of Object.entries(cases)) {
      for (const c of kindCases) {
        let actual: string;
        try {
          actual = toHex(mergeFns[kind](fromHex(c.state), fromHex(c.delta)));
        } catch (err) {
          actual = `threw: ${String(err)}`;
        }
        out.push({ label: `${kind}/${c.name}`, expected: c.merged, actual });
      }
    }
    return out;
  }, fixtures);
  expect(results.length).toBeGreaterThan(0);
  for (const result of results) {
    expect(result.actual, result.label).toBe(result.expected);
  }
});
