// Phase 7 exit checks in the browser, against the local dev node (published
// by scripts/phase7-smoke.sh):
//  - the stable facade URL reaches the running app through the loader hop;
//  - after a tile contract re-key, the app's backward migration probe carries
//    the previous instance's placements to the new key.

import { expect, FrameLocator, Page, test } from "@playwright/test";

const FACADE_URL = process.env.FREEPLACE_FACADE_URL;
const APP_URL = process.env.FREEPLACE_APP_URL;
// "x,y,color": a board pixel expected to be visible after migration.
const EXPECT_PIXEL = process.env.FREEPLACE_EXPECT_PIXEL;

function watchConsole(page: Page): string[] {
  const errors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("pageerror", (error) => errors.push(String(error)));
  return errors;
}

async function pixelAt(app: FrameLocator, x: number, y: number): Promise<number | null> {
  return app.locator("body").evaluate(
    (_el, [px, py]) =>
      (
        window as unknown as { __freeplace: { pixel(x: number, y: number): number | null } }
      ).__freeplace.pixel(px, py),
    [x, y],
  );
}

test.describe("phase 7: facade", () => {
  test.skip(!FACADE_URL, "FREEPLACE_FACADE_URL not set");
  test.setTimeout(180_000);

  test("the stable facade URL reaches the running app", async ({ page }) => {
    const errors = watchConsole(page);
    await page.goto(FACADE_URL!);
    // The loader hops to the current web container (via the shell's navigate
    // message or the in-iframe sandbox fallback); either way the app ends up
    // inside iframe#app and connects to the node.
    const app = page.frameLocator("iframe#app");
    await expect(app.getByTestId("conn-status")).toHaveText("connected", { timeout: 60_000 });
    expect(errors, `console errors:\n${errors.join("\n")}`).toEqual([]);
  });
});

test.describe("phase 7: migration probe", () => {
  test.skip(!APP_URL || !EXPECT_PIXEL, "FREEPLACE_APP_URL / FREEPLACE_EXPECT_PIXEL not set");
  test.setTimeout(180_000);

  test("the probe carries previous-release tile state to the new key", async ({ page }) => {
    const [x, y, color] = EXPECT_PIXEL!.split(",").map(Number);
    const errors = watchConsole(page);
    await page.goto(APP_URL!);
    await expect(page.locator("iframe#app")).toHaveAttribute("src", /__sandbox=1/, {
      timeout: 15_000,
    });
    const app = page.frameLocator("iframe#app");
    await expect(app.getByTestId("conn-status")).toHaveText("connected", { timeout: 60_000 });
    // The seeded placement lived only in the previous tile instance; seeing it
    // means the probe fetched the legacy state and folded it forward.
    await expect.poll(() => pixelAt(app, x, y), { timeout: 30_000 }).toBe(color);
    expect(errors, `console errors:\n${errors.join("\n")}`).toEqual([]);
  });
});
