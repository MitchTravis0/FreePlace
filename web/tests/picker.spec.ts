// Extended palette + wheel picker (255 contract colors): the offline mock
// tier. The classic 16 stay frozen at indices 0..15; the picker reaches the
// rest via the hue wheel, shade grid, gray ramp, and a snapping hex input.
// Console must be clean in every flow.

import { expect, Page, test } from "@playwright/test";

interface FreeplaceHooks {
  pixel(x: number, y: number): number | null;
  view(): { zoom: number; panX: number; panY: number };
  cooldownRemainingMs(): number;
  selectedColor(): number;
}

declare global {
  interface Window {
    __freeplace: FreeplaceHooks;
  }
}

function watchConsole(page: Page): string[] {
  const errors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("pageerror", (error) => errors.push(String(error)));
  return errors;
}

test("the palette holds 255 colors and the classic 16 are frozen", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1");
  const palette = await page.evaluate(async () => {
    const module = (await import("/src/canvas.ts")) as unknown as {
      PALETTE: string[];
      CLASSIC_PALETTE: string[];
    };
    return { all: module.PALETTE, classic: module.CLASSIC_PALETTE };
  });
  expect(palette.all).toHaveLength(255);
  expect(palette.classic).toHaveLength(16);
  expect(palette.all.slice(0, 16)).toEqual(palette.classic);
  expect(palette.all[15]).toBe("#820080");
  for (const color of palette.all) expect(color).toMatch(/^#[0-9a-f]{6}$/);
  expect(errors).toEqual([]);
});

test("wheel and shade grid select an extended color that places on the board", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  // Closed by default: a merely-transparent panel would swallow board input.
  await expect(page.getByTestId("picker-panel")).toBeHidden();
  await page.getByTestId("btn-picker").click();
  await expect(page.getByTestId("picker-panel")).toBeVisible();

  // Click the wheel ring at 12 o'clock: hue 0, vivid variant -> index 41.
  const wheel = (await page.getByTestId("picker-wheel").boundingBox())!;
  await page.mouse.click(wheel.x + wheel.width / 2, wheel.y + wheel.height / 2 - 69);
  expect(await page.evaluate(() => window.__freeplace.selectedColor())).toBe(41);

  // The darkest vivid shade of that hue (variant 4) -> index 39 + 4 = 43.
  await page.getByTestId("picker-shades").locator("button").nth(4).click();
  expect(await page.evaluate(() => window.__freeplace.selectedColor())).toBe(43);

  // Place it: extended colors are ordinary contract colors end to end.
  const view = await page.evaluate(() => window.__freeplace.view());
  const box = (await page.getByTestId("board").boundingBox())!;
  await page.mouse.click(
    box.x + view.panX + 200.5 * view.zoom,
    box.y + view.panY + 100.5 * view.zoom,
  );
  await expect
    .poll(() => page.evaluate(() => window.__freeplace.pixel(200, 100)))
    .toBe(43);
  expect(await page.evaluate(() => window.__freeplace.cooldownRemainingMs())).toBeGreaterThan(0);
  expect(errors).toEqual([]);
});

test("the hex input snaps to the nearest palette color", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  await page.getByTestId("btn-picker").click();
  const hexInput = page.getByTestId("picker-hex");

  // Pure red exists exactly in the wheel (hue 0, s100 l50 = index 41).
  await hexInput.fill("#ff0000");
  await hexInput.press("Enter");
  expect(await page.evaluate(() => window.__freeplace.selectedColor())).toBe(41);
  await expect(hexInput).toHaveValue("#ff0000");

  // Mid gray snaps into the gray ramp, and the input shows the snapped hex.
  await hexInput.fill("808080");
  await hexInput.press("Enter");
  const snapped = await page.evaluate(() => window.__freeplace.selectedColor());
  expect(snapped).toBeGreaterThanOrEqual(16);
  expect(snapped).toBeLessThan(39);
  await expect(hexInput).toHaveValue(/^#[0-9a-f]{6}$/);
  expect(errors).toEqual([]);
});

test("classic swatches and the picker stay in sync", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  const classic = page.getByTestId("palette").locator("button[data-color]");
  await classic.nth(15).click();
  await expect(classic.nth(15)).toHaveClass(/selected/);
  await expect(page.getByTestId("btn-picker")).not.toHaveClass(/selected/);
  await page.getByTestId("btn-picker").click();
  await expect(page.getByTestId("picker-hex")).toHaveValue("#820080");

  // A gray-ramp pick deselects the classic bar; the toggle (whose centre
  // shows the current color) takes the selected ring instead.
  await page.getByTestId("picker-grays").locator("button").first().click();
  expect(await page.evaluate(() => window.__freeplace.selectedColor())).toBe(16);
  await expect(page.getByTestId("palette").locator("button[data-color].selected")).toHaveCount(0);
  await expect(page.getByTestId("btn-picker")).toHaveClass(/selected/);
  expect(errors).toEqual([]);
});
