// Phase 6 iso tier: the same flows as the offline suite, against the
// gateway-served app on the local dev node (published by
// scripts/phase6-smoke.sh). The gateway wraps the app in an iframe shell, so
// everything goes through frameLocator and an absolute-URL goto.

import { expect, FrameLocator, Page, test } from "@playwright/test";

const BASE_URL = process.env.FREENET_BASE_URL;
const SKIP = !BASE_URL || !/\/v1\/contract\/web\//.test(BASE_URL);

function watchConsole(page: Page): string[] {
  const errors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("pageerror", (error) => errors.push(String(error)));
  return errors;
}

/// Board pixel -> viewport point, using the app's view transform inside the
/// iframe (boundingBox already accounts for the iframe offset).
async function boardPoint(
  app: FrameLocator,
  x: number,
  y: number,
): Promise<{ px: number; py: number }> {
  const view = await app
    .locator("body")
    .evaluate(() =>
      (
        window as unknown as {
          __freeplace: { view(): { zoom: number; panX: number; panY: number } };
        }
      ).__freeplace.view(),
    );
  const box = (await app.getByTestId("board").boundingBox())!;
  return {
    px: box.x + view.panX + (x + 0.5) * view.zoom,
    py: box.y + view.panY + (y + 0.5) * view.zoom,
  };
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

test.describe("phase 6 gateway (iso) tier", () => {
  test.skip(SKIP, "FREENET_BASE_URL not set to a /v1/contract/web/... URL");
  test.setTimeout(300_000);

  test("onboarding, placement, chat, and persistence through the gateway", async ({ page }) => {
    const errors = watchConsole(page);

    await page.goto(BASE_URL!);
    // Shell bridge ran and armed the sandboxed iframe.
    await expect(page.locator("iframe#app")).toHaveAttribute("src", /__sandbox=1/, {
      timeout: 15_000,
    });
    const app = page.frameLocator("iframe#app");

    await expect(app.getByTestId("conn-status")).toHaveText("connected", { timeout: 30_000 });

    // Fresh registry instance per smoke run: onboarding opens with the grind
    // and the ghost key offer + disclosure.
    const onboarding = app.getByTestId("onboarding");
    await expect(onboarding).toBeVisible({ timeout: 15_000 });
    await expect(app.getByTestId("ghostkey-disclosure")).toContainText("Ghost keys cost money");
    await expect(app.getByTestId("pow-progress")).toBeVisible();

    // No ghostkeys delegate on the dev node: the request must resolve to the
    // no-key state (via the delegate round-trip timeout), never an error.
    await app.getByTestId("btn-ghostkey").click();

    // The 18-bit grind admits us (usually a few seconds in the worker).
    await expect(onboarding).toBeHidden({ timeout: 180_000 });
    await expect(app.getByTestId("identity-chip")).toContainText("proof of work");
    await expect(app.getByTestId("cooldown")).toHaveText("ready", { timeout: 15_000 });

    // The parked ghost key request from before the grind finished must have
    // landed in its no-key state (delegate missing on this node).
    await expect(app.getByTestId("ghostkey-status")).toContainText("no ghost key available", {
      timeout: 60_000,
    });

    // Place a pixel with palette color 12 at board (200, 100).
    await app.getByTestId("palette").locator("button").nth(12).click();
    const { px, py } = await boardPoint(app, 200, 100);
    await page.mouse.click(px, py);
    await expect.poll(() => pixelAt(app, 200, 100)).toBe(12);
    await expect(app.getByTestId("cooldown")).toHaveText(/next pixel in \d+s/);

    // Chat round-trip.
    const chatLine = `iso smoke ${Date.now()}`;
    await app.getByTestId("chat-input").fill(chatLine);
    await app.getByTestId("chat-send").click();
    await expect(app.getByTestId("chat-messages")).toContainText(chatLine);

    // Give the node a moment to run the validate passes, then reload: state
    // must come back from the contracts, not from optimistic memory.
    await page.waitForTimeout(5_000);
    await page.goto(BASE_URL!);
    const app2 = page.frameLocator("iframe#app");
    await expect(app2.getByTestId("conn-status")).toHaveText("connected", { timeout: 30_000 });
    // Already admitted: no onboarding this time.
    await expect(app2.getByTestId("identity-chip")).toContainText("proof of work", {
      timeout: 15_000,
    });
    await expect(app2.getByTestId("onboarding")).toBeHidden();
    await expect.poll(() => pixelAt(app2, 200, 100), { timeout: 30_000 }).toBe(12);
    await expect(app2.getByTestId("chat-messages")).toContainText(chatLine, { timeout: 30_000 });

    expect(errors, `console errors:\n${errors.join("\n")}`).toEqual([]);
  });
});
