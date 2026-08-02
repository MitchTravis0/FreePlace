// Phase 13 production liveness: a minimal post-publish check against the
// published gateway URL (the stable facade URL, or a container URL directly).
// Run it right after a real-network publish:
//   FREEPLACE_LIVE_URL=http://127.0.0.1:7509/v1/contract/web/<facade-id>/ \
//     npx playwright test tests/liveness.spec.ts
// Skipped when the env var is unset, so offline runs are unaffected.

import { expect, test } from "@playwright/test";

const LIVE_URL = process.env.FREEPLACE_LIVE_URL;
const SKIP = !LIVE_URL || !/\/v1\/contract\/web\//.test(LIVE_URL);

test.describe("phase 13: production liveness", () => {
  test.skip(SKIP, "FREEPLACE_LIVE_URL not set to a /v1/contract/web/... URL");
  test.setTimeout(180_000);

  test("published app mounts, syncs, and renders with vendored CSS", async ({ page }) => {
    const errors: string[] = [];
    page.on("console", (message) => {
      if (message.type() === "error") errors.push(message.text());
    });
    page.on("pageerror", (error) => errors.push(String(error)));

    await page.goto(LIVE_URL!);

    // Shell bridge ran and armed the sandboxed iframe. When LIVE_URL is the
    // facade, the loader then hops to the current web container via the
    // shell's top-level navigate; the watchers above survive the navigation.
    await expect(page.locator("iframe#app")).toHaveAttribute("src", /__sandbox=1/, {
      timeout: 15_000,
    });
    const app = page.frameLocator("iframe#app");

    // The backend reports "connected" only after syncAll() has GET-ed the
    // registry, the chat room, and all 16 tiles, so this one assertion also
    // proves the published contract instances answer GETs.
    await expect(app.getByTestId("conn-status")).toHaveText("connected", { timeout: 90_000 });

    // Phase 8+ marker: the coords chip exists, so a stale pre-feature web
    // container fails here even if it still connects.
    const coords = app.getByTestId("coords");
    await expect(coords).toBeAttached();

    // Vendored CSS applied: style.css positions the coords chip absolutely
    // (UA default is "static"), so this flips iff the stylesheet loaded
    // through the gateway CSP.
    const position = await coords.evaluate((el) => getComputedStyle(el).position);
    expect(position, "vendored CSS did not load - check CSP / asset paths").toBe("absolute");

    expect(errors, `console errors:\n${errors.join("\n")}`).toEqual([]);
  });
});
