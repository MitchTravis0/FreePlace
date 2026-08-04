// Phase 10, deferred half (touch + pinch): the offline mock tier in a
// phone-sized touch context, driving the board with synthesized PointerEvent
// pairs (Playwright has no native multi-touch gesture API). Console must be
// clean in every flow.

import { expect, Page, test } from "@playwright/test";

test.use({ viewport: { width: 390, height: 844 }, hasTouch: true });

interface FreeplaceHooks {
  view(): { zoom: number; panX: number; panY: number };
  pixel(x: number, y: number): number | null;
  cooldownRemainingMs(): number;
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

/// Dispatch a synthesized touch-type PointerEvent on the board canvas.
/// clientX/clientY are viewport coordinates.
async function touchEvent(
  page: Page,
  type: "pointerdown" | "pointermove" | "pointerup",
  pointerId: number,
  clientX: number,
  clientY: number,
): Promise<void> {
  await page.evaluate(
    (e) => {
      document.querySelector('[data-testid="board"]')!.dispatchEvent(
        new PointerEvent(e.type, {
          pointerId: e.pointerId,
          pointerType: "touch",
          isPrimary: e.pointerId === 1,
          clientX: e.clientX,
          clientY: e.clientY,
          bubbles: true,
        }),
      );
    },
    { type, pointerId, clientX, clientY },
  );
}

/// The viewport point at the centre of the board canvas.
async function boardCenter(page: Page): Promise<{ cx: number; cy: number }> {
  const box = (await page.getByTestId("board").boundingBox())!;
  return { cx: box.x + box.width / 2, cy: box.y + box.height / 2 };
}

/// The board coordinate currently under a viewport point.
async function boardCoordAt(page: Page, vx: number, vy: number): Promise<{ x: number; y: number }> {
  const box = (await page.getByTestId("board").boundingBox())!;
  const view = await page.evaluate(() => window.__freeplace.view());
  return {
    x: (vx - box.x - view.panX) / view.zoom,
    y: (vy - box.y - view.panY) / view.zoom,
  };
}

test("pinch zooms around the midpoint and its release never places", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  const { cx, cy } = await boardCenter(page);
  const before = await page.evaluate(() => window.__freeplace.view());
  const anchor = await boardCoordAt(page, cx, cy);

  // Two fingers 80px apart spread symmetrically to 240px: a 3x zoom.
  await touchEvent(page, "pointerdown", 1, cx - 40, cy);
  await touchEvent(page, "pointerdown", 2, cx + 40, cy);
  await touchEvent(page, "pointermove", 1, cx - 120, cy);
  await touchEvent(page, "pointermove", 2, cx + 120, cy);

  const zoomed = await page.evaluate(() => window.__freeplace.view());
  expect(zoomed.zoom).toBeCloseTo(before.zoom * 3, 5);
  // The board point under the pinch midpoint stayed fixed.
  const anchorAfter = await boardCoordAt(page, cx, cy);
  expect(anchorAfter.x).toBeCloseTo(anchor.x, 3);
  expect(anchorAfter.y).toBeCloseTo(anchor.y, 3);

  // Lifting one finger degrades to a pan with the survivor, still no placing.
  await touchEvent(page, "pointerup", 1, cx - 120, cy);
  await touchEvent(page, "pointermove", 2, cx + 150, cy);
  const panned = await page.evaluate(() => window.__freeplace.view());
  expect(panned.panX).toBeCloseTo(zoomed.panX + 30, 3);
  expect(panned.zoom).toBeCloseTo(zoomed.zoom, 5);
  await touchEvent(page, "pointerup", 2, cx + 150, cy);

  // A placement would have started the cooldown.
  expect(await page.evaluate(() => window.__freeplace.cooldownRemainingMs())).toBe(0);
  expect(errors).toEqual([]);
});

test("two-finger drag pans without zooming or placing", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  const { cx, cy } = await boardCenter(page);
  const before = await page.evaluate(() => window.__freeplace.view());

  // Vertical finger arrangement: the sequential moves then only wobble the
  // inter-pointer distance mildly (~0.9x), far from the zoom clamps.
  await touchEvent(page, "pointerdown", 1, cx, cy - 40);
  await touchEvent(page, "pointerdown", 2, cx, cy + 40);
  await touchEvent(page, "pointermove", 1, cx + 60, cy);
  await touchEvent(page, "pointermove", 2, cx + 60, cy + 80);
  await touchEvent(page, "pointerup", 1, cx + 60, cy);
  await touchEvent(page, "pointerup", 2, cx + 60, cy + 80);

  const after = await page.evaluate(() => window.__freeplace.view());
  expect(after.panX).toBeCloseTo(before.panX + 60, 3);
  expect(after.panY).toBeCloseTo(before.panY + 40, 3);
  expect(after.zoom).toBeCloseTo(before.zoom, 5);
  expect(await page.evaluate(() => window.__freeplace.cooldownRemainingMs())).toBe(0);
  expect(errors).toEqual([]);
});

test("single-finger drag pans without placing", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  const { cx, cy } = await boardCenter(page);
  const before = await page.evaluate(() => window.__freeplace.view());

  await touchEvent(page, "pointerdown", 1, cx, cy);
  await touchEvent(page, "pointermove", 1, cx + 80, cy - 25);
  await touchEvent(page, "pointerup", 1, cx + 80, cy - 25);

  const after = await page.evaluate(() => window.__freeplace.view());
  expect(after.panX).toBeCloseTo(before.panX + 80, 3);
  expect(after.panY).toBeCloseTo(before.panY - 25, 3);
  expect(after.zoom).toBeCloseTo(before.zoom, 5);
  expect(await page.evaluate(() => window.__freeplace.cooldownRemainingMs())).toBe(0);
  expect(errors).toEqual([]);
});

test("a tap places a pixel even with finger wobble", async ({ page }) => {
  const errors = watchConsole(page);
  // Boot centred and zoomed so an 8px wobble stays inside one board pixel.
  await page.goto("/?mock=1&admitted=1&x=512&y=512&zoom=8");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");
  expect(await page.evaluate(() => window.__freeplace.pixel(512, 512))).toBeNull();

  const box = (await page.getByTestId("board").boundingBox())!;
  const view = await page.evaluate(() => window.__freeplace.view());
  const px = box.x + view.panX + 512.5 * view.zoom;
  const py = box.y + view.panY + 512.5 * view.zoom;

  // 8px of travel is past the mouse click slop (4) but inside the touch
  // slop (12): it must still read as a tap, not a pan.
  await touchEvent(page, "pointerdown", 1, px, py);
  await touchEvent(page, "pointermove", 1, px + 8, py);
  await touchEvent(page, "pointermove", 1, px, py);
  await touchEvent(page, "pointerup", 1, px, py);

  await expect
    .poll(() => page.evaluate(() => window.__freeplace.pixel(512, 512)))
    .toBe(5);
  expect(
    await page.evaluate(() => window.__freeplace.cooldownRemainingMs()),
  ).toBeGreaterThan(0);
  expect(errors).toEqual([]);
});
