// Regression for the baked layer going missing on full-state loads (found
// 2026-08-02): decodeTileState decodes both the live and baked logs, but the
// copy into the render state only iterated the live log, so any pixel an
// author had displaced past the K=8 live cap was invisible to every client
// that loaded the tile fresh (initial sync and full-state notifications).
// mergeTileInto folds BOTH layers.

import { expect, test } from "@playwright/test";

test("mergeTileInto carries the baked layer into the render state", async ({ page }) => {
  await page.goto("/?mock=1");
  const result = await page.evaluate(async () => {
    const backend = (await import("/src/backend.ts")) as unknown as {
      mergeTileInto(target: unknown, decoded: unknown): void;
    };
    const state = (await import("/src/state.ts")) as unknown as {
      TileStateJs: new () => {
        insert(p: unknown): void;
        placements: Map<string, Map<number, unknown>>;
        baked: Map<string, Map<number, unknown>>;
        validPlacements(tierOf: (a: string) => string | null): unknown[];
      };
      MAX_PLACEMENTS_PER_AUTHOR: number;
      POW_TILE_COOLDOWN_SECS: number;
    };
    // 9 spaced placements from one author: the 9th displaces the 1st into
    // the decoded state's baked layer (mirroring what a populated wire state
    // decodes to).
    const author = new Uint8Array(32).fill(1);
    const decoded = new state.TileStateJs();
    const spacing = state.POW_TILE_COOLDOWN_SECS + 10;
    const count = state.MAX_PLACEMENTS_PER_AUTHOR + 1;
    for (let i = 0; i < count; i++) {
      decoded.insert({
        coord: i,
        color: 3,
        ts: 1_000_000 + i * spacing,
        author,
        signature: new Uint8Array(64),
      });
    }
    const target = new state.TileStateJs();
    backend.mergeTileInto(target, decoded);
    const bakedCount = [...target.baked.values()].reduce((n, log) => n + log.size, 0);
    const liveCount = [...target.placements.values()].reduce((n, log) => n + log.size, 0);
    return {
      decodedBaked: [...decoded.baked.values()].reduce((n, log) => n + log.size, 0),
      bakedCount,
      liveCount,
      visible: target.validPlacements(() => "Pow").length,
    };
  });
  expect(result.decodedBaked).toBe(1);
  expect(result.bakedCount).toBe(1);
  expect(result.liveCount).toBe(8);
  // All 9 spaced placements render; before the fix the baked one vanished.
  expect(result.visible).toBe(9);
});
