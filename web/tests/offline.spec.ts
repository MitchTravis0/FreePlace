// Offline tier (Phase 6, extended by Phases 8, 9, 11, and 12): the full UI
// against `vite dev` with mock data (?mock=1), no node involved. Console must
// be clean in every flow.

import { expect, Page, test } from "@playwright/test";

interface FreeplaceHooks {
  view(): { zoom: number; panX: number; panY: number };
  pixel(x: number, y: number): number | null;
  cooldownRemainingMs(): number;
  winnerAt(x: number, y: number): { author: string; ts: number } | null;
  recentArrivals(): { x: number; y: number; until: number }[];
  centerPixel(): { x: number; y: number };
  overlay(): { x: number; y: number; w: number; h: number; remaining: number } | null;
  replay(): { cutoff: number; min: number; max: number; playing: boolean } | null;
}

interface MockHooks {
  injectPlacement(x: number, y: number, color: number): void;
  injectChat(content: string): void;
}

declare global {
  interface Window {
    __freeplace: FreeplaceHooks;
    __freeplaceMock: MockHooks;
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

/// Board pixel -> viewport point, via the app's view transform.
async function boardPoint(page: Page, x: number, y: number): Promise<{ px: number; py: number }> {
  const view = await page.evaluate(() => window.__freeplace.view());
  const box = (await page.getByTestId("board").boundingBox())!;
  return {
    px: box.x + view.panX + (x + 0.5) * view.zoom,
    py: box.y + view.panY + (y + 0.5) * view.zoom,
  };
}

test("canvas renders mock state and zoom/pan work", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  // Seeded content is derived and rendered: marker pixel + strip + lone pixel.
  await expect
    .poll(() => page.evaluate(() => window.__freeplace.pixel(10, 10)))
    .toBe(5);
  expect(await page.evaluate(() => window.__freeplace.pixel(510, 300))).toBe(6);
  expect(await page.evaluate(() => window.__freeplace.pixel(513, 300))).toBe(9);
  expect(await page.evaluate(() => window.__freeplace.pixel(900, 700))).toBe(11);
  expect(await page.evaluate(() => window.__freeplace.pixel(11, 10))).toBeNull();

  // Zoom in around the board centre.
  const before = await page.evaluate(() => window.__freeplace.view());
  const box = (await page.getByTestId("board").boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.wheel(0, -400);
  await expect
    .poll(() => page.evaluate(() => window.__freeplace.view().zoom))
    .toBeGreaterThan(before.zoom);

  // Drag pans the board.
  const mid = await page.evaluate(() => window.__freeplace.view());
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2 + 120, box.y + box.height / 2 + 60, { steps: 5 });
  await page.mouse.up();
  const after = await page.evaluate(() => window.__freeplace.view());
  expect(after.panX).not.toBe(mid.panX);
  expect(after.panY).not.toBe(mid.panY);
  // A drag is not a click: no cooldown was started.
  expect(await page.evaluate(() => window.__freeplace.cooldownRemainingMs())).toBe(0);

  expect(errors).toEqual([]);
});

test("palette placement, optimistic update, and cooldown timer", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");
  await expect(page.getByTestId("cooldown")).toHaveText("ready");

  // Pick color 12 and place at (200, 100).
  await page.getByTestId("palette").locator("button").nth(12).click();
  const { px, py } = await boardPoint(page, 200, 100);
  await page.mouse.click(px, py);

  // Optimistic render + cooldown countdown.
  await expect
    .poll(() => page.evaluate(() => window.__freeplace.pixel(200, 100)))
    .toBe(12);
  await expect(page.getByTestId("cooldown")).toHaveText(/next pixel in \d+s/);

  // A second click during the cooldown places nothing.
  const { px: px2, py: py2 } = await boardPoint(page, 202, 100);
  await page.mouse.click(px2, py2);
  await page.waitForTimeout(200);
  expect(await page.evaluate(() => window.__freeplace.pixel(202, 100))).toBeNull();

  expect(errors).toEqual([]);
});

test("live remote deltas update the canvas", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  expect(await page.evaluate(() => window.__freeplace.pixel(640, 640))).toBeNull();
  await page.evaluate(() => window.__freeplaceMock.injectPlacement(640, 640, 3));
  await expect
    .poll(() => page.evaluate(() => window.__freeplace.pixel(640, 640)))
    .toBe(3);

  expect(errors).toEqual([]);
});

test("chat sidebar shows history, sends, and receives", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  // Seeded history with nicknames from the registry.
  const messages = page.getByTestId("chat-messages");
  await expect(messages).toContainText("welcome to FreePlace");
  await expect(messages).toContainText("alice");

  // Send.
  await page.getByTestId("chat-input").fill("hello from the offline suite");
  await page.getByTestId("chat-send").click();
  await expect(messages).toContainText("hello from the offline suite");
  await expect(messages).toContainText("you");

  // Receive.
  await page.evaluate(() => window.__freeplaceMock.injectChat("remote peer says hi"));
  await expect(messages).toContainText("remote peer says hi");
  await expect(messages).toContainText("eve");

  expect(errors).toEqual([]);
});

test("FAQ documents the per-tile cooldown compromise and ghost key terms", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  const faq = page.getByTestId("faq");
  await expect(faq).toBeHidden();
  await page.getByTestId("btn-faq").click();
  await expect(faq).toBeVisible();

  // The per-tile cooldown compromise (plan.md risks: "documented in the FAQ").
  const compromise = page.getByTestId("faq-cooldown");
  await expect(compromise).toContainText("16 independent tile contracts");
  await expect(compromise).toContainText("up to 16 pixels");
  await expect(compromise).toContainText("known, accepted v1 compromise");

  // The ghost key disclosure is repeated here, outside onboarding.
  await expect(faq).toContainText("Ghost keys cost money");
  await expect(faq).toContainText("mint is centralized");

  // Phase 11: overlays are session-only (opaque iframe origin, no storage).
  await expect(page.getByTestId("faq-overlay")).toContainText("session-only");

  // Phase 12: replay covers only the recent window each client holds.
  await expect(page.getByTestId("faq-replay")).toContainText("last 8 placements");
  await expect(page.getByTestId("faq-replay")).toContainText("not full history");

  await page.getByTestId("btn-faq-close").click();
  await expect(faq).toBeHidden();

  expect(errors).toEqual([]);
});

test("onboarding: disclosure, nickname, and ghost key admission", async ({ page }) => {
  const errors = watchConsole(page);
  // holdpow keeps the challenge (and so the grind) parked until the test
  // releases it, making the overlay interactions deterministic.
  await page.goto("/?mock=1&holdpow=1&ghostkey=yes");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  const onboarding = page.getByTestId("onboarding");
  await expect(onboarding).toBeVisible();
  await expect(page.getByTestId("pow-progress")).toBeVisible();
  await expect(page.getByTestId("pow-status")).toContainText("preparing challenge");

  // The required ghost key disclosure copy.
  const disclosure = page.getByTestId("ghostkey-disclosure");
  await expect(disclosure).toContainText("Ghost keys cost money");
  await expect(disclosure).toContainText("funds Freenet");
  await expect(disclosure).toContainText("mint is centralized");
  await expect(disclosure).toContainText("proof-of-work path is always sufficient");

  await page.getByTestId("nickname-input").fill("tester");
  await page.getByTestId("btn-ghostkey").click();
  await expect(page.getByTestId("ghostkey-status")).toContainText("asking the ghost key delegate");

  // Release: the ghost key path (instant) admits before the 12-bit grind can.
  await page.evaluate(() => window.__freeplaceMock.releasePow());
  await expect(onboarding).toBeHidden({ timeout: 15_000 });
  await expect(page.getByTestId("identity-chip")).toContainText("tester");
  await expect(page.getByTestId("identity-chip")).toContainText("ghost key");
  await expect(page.getByTestId("cooldown")).toHaveText("ready");

  expect(errors).toEqual([]);
});

// ---------------------------------------------------------------------------
// Phase 8: canvas information + feedback layer
// ---------------------------------------------------------------------------

test("hovering a painted pixel at high zoom shows the author tooltip", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");
  await expect
    .poll(() => page.evaluate(() => window.__freeplace.pixel(10, 10)))
    .toBe(5);

  // The winner map backs the tooltip: the marker pixel belongs to alice.
  const winner = await page.evaluate(() => window.__freeplace.winnerAt(10, 10));
  expect(winner?.author).toBe("01".repeat(32));

  // Zoom in on the marker pixel until the pixel-cursor threshold is reached
  // (the zoom keeps the board point under the cursor fixed).
  const tooltip = page.getByTestId("pixel-tooltip");
  await expect(tooltip).toBeHidden();
  const { px, py } = await boardPoint(page, 10, 10);
  await page.mouse.move(px, py);
  while ((await page.evaluate(() => window.__freeplace.view().zoom)) < 4) {
    await page.mouse.wheel(0, -400);
  }
  // Mouse coordinates are rounded to integers, which at low zoom can drift the
  // hover a pixel off target; re-aim at the pixel centre now that one board
  // pixel spans >= 4 screen px.
  const zoomed = await boardPoint(page, 10, 10);
  await page.mouse.move(zoomed.px, zoomed.py);
  await expect(tooltip).toBeVisible();
  await expect(tooltip).toContainText("alice");
  await expect(tooltip).toContainText("color 5");
  await expect(tooltip).toContainText("ago");

  // An empty neighbour pixel hides it again.
  const empty = await boardPoint(page, 14, 10);
  await page.mouse.move(empty.px, empty.py);
  await expect(tooltip).toBeHidden();

  expect(errors).toEqual([]);
});

test("the coords chip tracks the hovered board coordinate", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  const coords = page.getByTestId("coords");
  const a = await boardPoint(page, 200, 100);
  await page.mouse.move(a.px, a.py);
  await expect(coords).toHaveText("(200, 100)");

  const b = await boardPoint(page, 400, 300);
  await page.mouse.move(b.px, b.py);
  await expect(coords).toHaveText("(400, 300)");

  expect(errors).toEqual([]);
});

test("clicking during cooldown raises a toast and does not place", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");
  await expect(page.getByTestId("cooldown")).toHaveText("ready");

  const first = await boardPoint(page, 300, 200);
  await page.mouse.click(first.px, first.py);
  await expect(page.getByTestId("cooldown")).toHaveText(/next pixel in \d+s/);

  const second = await boardPoint(page, 302, 200);
  await page.mouse.click(second.px, second.py);
  const toast = page.getByTestId("error-toast");
  await expect(toast).toBeVisible();
  await expect(toast).toHaveText(/next pixel in \d+s/);
  await expect(page.getByTestId("cooldown")).toHaveClass(/shake/);
  expect(await page.evaluate(() => window.__freeplace.pixel(302, 200))).toBeNull();

  expect(errors).toEqual([]);
});

test("a remote arrival gets a highlight entry that expires", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  // Seed placements are not arrivals; the list starts empty.
  expect(await page.evaluate(() => window.__freeplace.recentArrivals())).toEqual([]);

  await page.evaluate(() => window.__freeplaceMock.injectPlacement(640, 640, 3));
  const arrivals = await page.evaluate(() => window.__freeplace.recentArrivals());
  expect(arrivals).toHaveLength(1);
  expect(arrivals[0]).toMatchObject({ x: 640, y: 640 });
  // The pixel itself landed with its real palette color (overlay-only ring).
  await expect
    .poll(() => page.evaluate(() => window.__freeplace.pixel(640, 640)))
    .toBe(3);

  // The ~1.2 s ring expires on its own.
  await expect
    .poll(() => page.evaluate(() => window.__freeplace.recentArrivals().length), {
      timeout: 5_000,
    })
    .toBe(0);

  expect(errors).toEqual([]);
});

// ---------------------------------------------------------------------------
// Phase 9: social coordination + identity
// ---------------------------------------------------------------------------

test("chat coordinate links recenter the viewport", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  await page.evaluate(() =>
    window.__freeplaceMock.injectChat("meet at (512, 300) or 700,800 but not (5000, 12)"),
  );
  const messages = page.getByTestId("chat-messages");

  // Both in-range forms become buttons; the out-of-range pair stays text.
  const parenLink = messages.getByRole("button", { name: "(512, 300)" });
  await expect(parenLink).toBeVisible();
  await expect(messages.getByRole("button", { name: "700,800" })).toBeVisible();
  await expect(messages.getByRole("button", { name: "(5000, 12)" })).toHaveCount(0);
  await expect(messages).toContainText("(5000, 12)");

  // Clicking recentres the viewport on the pixel at zoom >= 8.
  const before = await page.evaluate(() => window.__freeplace.view().zoom);
  expect(before).toBeLessThan(8);
  await parenLink.click();
  await expect
    .poll(() => page.evaluate(() => window.__freeplace.view().zoom))
    .toBeGreaterThanOrEqual(8);
  expect(await page.evaluate(() => window.__freeplace.centerPixel())).toEqual({ x: 512, y: 300 });

  expect(errors).toEqual([]);
});

test("share-my-spot inserts the viewport-centre coordinate into the input", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  const center = await page.evaluate(() => window.__freeplace.centerPixel());
  await page.getByTestId("chat-input").fill("meet at");
  await page.getByTestId("btn-share-spot").click();
  await expect(page.getByTestId("chat-input")).toHaveValue(`meet at (${center.x}, ${center.y})`);

  expect(errors).toEqual([]);
});

test("chat messages carry timestamps and own-message styling", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  const messages = page.getByTestId("chat-messages");
  const seeded = messages.locator(".chat-message", { hasText: "welcome to FreePlace" });
  await expect(seeded.locator(".msg-time")).toHaveText(/\d+[smhd] ago/);
  expect(await seeded.locator(".msg-time").getAttribute("title")).toBeTruthy();
  await expect(seeded).not.toHaveClass(/mine/);

  await page.getByTestId("chat-input").fill("that is my pixel");
  await page.getByTestId("chat-send").click();
  const mine = messages.locator(".chat-message", { hasText: "that is my pixel" });
  await expect(mine).toHaveClass(/mine/);
  await expect(mine.locator(".msg-time")).toHaveText(/\d+[smhd] ago/);

  expect(errors).toEqual([]);
});

test("ghost key upgrade flips the tier and keeps the nickname", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1&ghostkey=yes");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  const chip = page.getByTestId("identity-chip");
  const upgrade = page.getByTestId("btn-upgrade");
  await expect(chip).toHaveText("you · proof of work");
  await expect(upgrade).toBeVisible();

  await upgrade.click();
  // The upgrade record carries no nickname; the merge must keep "you".
  await expect(chip).toHaveText("you · ghost key");
  await expect(upgrade).toBeHidden();

  expect(errors).toEqual([]);
});

test("ghost key upgrade without a key shows the no-key state and stays PoW", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  await page.getByTestId("btn-upgrade").click();
  await expect(page.getByTestId("error-toast")).toContainText("no ghost key available");
  await expect(page.getByTestId("identity-chip")).toHaveText("you · proof of work");
  await expect(page.getByTestId("btn-upgrade")).toBeVisible();
  await expect(page.getByTestId("btn-upgrade")).toBeEnabled();

  expect(errors).toEqual([]);
});

test("nickname edit updates the chip and chat authorship", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  await page.getByTestId("chat-input").fill("signed by me");
  await page.getByTestId("chat-send").click();
  const messages = page.getByTestId("chat-messages");
  const mine = messages.locator(".chat-message", { hasText: "signed by me" });
  await expect(mine.locator(".nick")).toHaveText("you");

  // The inline editor opens prefilled from the registry record.
  const form = page.getByTestId("nickname-form");
  await expect(form).toBeHidden();
  await page.getByTestId("identity-chip").click();
  await expect(form).toBeVisible();
  await expect(page.getByTestId("nickname-edit")).toHaveValue("you");

  await page.getByTestId("nickname-edit").fill("picasso");
  await page.getByTestId("nickname-save").click();
  await expect(form).toBeHidden();
  await expect(page.getByTestId("identity-chip")).toContainText("picasso");
  // Chat authorship re-renders from the registry.
  await expect(mine.locator(".nick")).toHaveText("picasso");

  expect(errors).toEqual([]);
});

test("activity chip shows seeded hourly counts and tracks new placements", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  // The mock seeds exactly two authors with placements in the last hour.
  const activity = page.getByTestId("activity");
  await expect(activity).toHaveText("2 painters · 2 pixels / hour");

  // Our own placement joins the window.
  const { px, py } = await boardPoint(page, 200, 200);
  await page.mouse.click(px, py);
  await expect(activity).toHaveText("3 painters · 3 pixels / hour");

  expect(errors).toEqual([]);
});

test("x/y/zoom query params centre the view at boot", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1&x=512&y=300&zoom=10");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  expect(await page.evaluate(() => window.__freeplace.view().zoom)).toBe(10);
  expect(await page.evaluate(() => window.__freeplace.centerPixel())).toEqual({ x: 512, y: 300 });

  expect(errors).toEqual([]);
});

// ---------------------------------------------------------------------------
// Phase 11: template overlays
// ---------------------------------------------------------------------------

/// Loads the 4x4 fixture (15 red target pixels, one transparent corner) and
/// anchors it at (600, 600), an empty area of the seeded board.
async function loadTemplateFixture(page: Page): Promise<void> {
  await page.getByTestId("btn-overlay-toggle").click();
  await page.getByTestId("overlay-file").setInputFiles("tests/fixtures/template-4x4.png");
  await expect.poll(() => page.evaluate(() => window.__freeplace.overlay())).not.toBeNull();
  await page.getByTestId("overlay-x").fill("600");
  await page.getByTestId("overlay-y").fill("600");
}

/// Reads the rendered RGB of a board pixel from the visible canvas, via the
/// app's view transform.
async function renderedRgb(page: Page, x: number, y: number): Promise<[number, number, number]> {
  return page.evaluate(([bx, by]) => {
    const view = window.__freeplace.view();
    const canvas = document.querySelector<HTMLCanvasElement>('[data-testid="board"]')!;
    const data = canvas
      .getContext("2d")!
      .getImageData(
        Math.round(view.panX + (bx + 0.5) * view.zoom),
        Math.round(view.panY + (by + 0.5) * view.zoom),
        1,
        1,
      ).data;
    return [data[0], data[1], data[2]] as [number, number, number];
  }, [x, y]);
}

test("loading the template fixture shows the overlay at its anchor", async ({ page }) => {
  const errors = watchConsole(page);
  // Boot zoomed in on the anchor area so one board pixel spans 8 screen px.
  await page.goto("/?mock=1&admitted=1&x=601&y=601&zoom=8");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  await loadTemplateFixture(page);
  // The panel is honest about persistence (opaque iframe origin, no storage).
  await expect(page.getByTestId("overlay-note")).toContainText("session-only");

  // 15 opaque target pixels over an empty board area: all remaining.
  expect(await page.evaluate(() => window.__freeplace.overlay())).toEqual({
    x: 600,
    y: 600,
    w: 4,
    h: 4,
    remaining: 15,
  });

  // Rendered above the (white, empty) board at the default 0.5 opacity: a red
  // target pixel reads as the 50/50 blend, the transparent corner stays white.
  await expect
    .poll(async () => {
      const [r, g, b] = await renderedRgb(page, 600, 600);
      return Math.abs(r - 242) <= 6 && Math.abs(g - 128) <= 6 && Math.abs(b - 128) <= 6;
    })
    .toBe(true);
  expect(await renderedRgb(page, 603, 603)).toEqual([255, 255, 255]);

  // Drag-to-place drops the overlay centred on the clicked pixel and never
  // places a board pixel (no cooldown starts).
  await page.getByTestId("btn-overlay-drag").click();
  const target = await boardPoint(page, 610, 604);
  await page.mouse.click(target.px, target.py);
  await expect
    .poll(() => page.evaluate(() => window.__freeplace.overlay()))
    .toMatchObject({ x: 608, y: 602 });
  expect(await page.evaluate(() => window.__freeplace.cooldownRemainingMs())).toBe(0);

  expect(errors).toEqual([]);
});

test("diff mode: remaining count drops after placing a matching pixel", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1&x=601&y=601&zoom=8");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");
  await loadTemplateFixture(page);

  await page.getByTestId("overlay-diff").check();
  await expect(page.getByTestId("overlay-remaining")).toHaveText("remaining: 15");

  // Paint the top-left target with its matching color (palette 5, red).
  await page.getByTestId("palette").locator("button").nth(5).click();
  const { px, py } = await boardPoint(page, 600, 600);
  await page.mouse.click(px, py);
  await expect
    .poll(() => page.evaluate(() => window.__freeplace.pixel(600, 600)))
    .toBe(5);

  // The matching pixel leaves the work queue.
  await expect(page.getByTestId("overlay-remaining")).toHaveText("remaining: 14");
  expect((await page.evaluate(() => window.__freeplace.overlay()))!.remaining).toBe(14);

  expect(errors).toEqual([]);
});

test("an oversized template image is rejected with a toast", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  await page.getByTestId("btn-overlay-toggle").click();
  await page.getByTestId("overlay-file").setInputFiles("tests/fixtures/template-too-big.png");
  const toast = page.getByTestId("error-toast");
  await expect(toast).toBeVisible();
  await expect(toast).toContainText("256x256");
  expect(await page.evaluate(() => window.__freeplace.overlay())).toBeNull();

  expect(errors).toEqual([]);
});

// ---------------------------------------------------------------------------
// Phase 12: timelapse replay
// ---------------------------------------------------------------------------

/// Sets the replay scrubber (Playwright's fill() rejects range inputs).
async function scrubTo(page: Page, ts: number): Promise<void> {
  await page.getByTestId("replay-scrubber").evaluate((element, value) => {
    (element as HTMLInputElement).value = String(value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  }, ts);
}

test("replay scrubbing shows placements in ts order and exiting restores the live board", async ({
  page,
}) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");
  await expect
    .poll(() => page.evaluate(() => window.__freeplace.pixel(10, 10)))
    .toBe(5);

  // Bob's strip: 8 placements at 130 s spacing; capture their timestamps from
  // the live winner map before entering replay.
  const stripTs = await page.evaluate(() =>
    Array.from({ length: 8 }, (_, i) => window.__freeplace.winnerAt(508 + i, 300)!.ts),
  );

  const bar = page.getByTestId("replay-bar");
  await expect(bar).toBeHidden();
  expect(await page.evaluate(() => window.__freeplace.replay())).toBeNull();
  await page.getByTestId("btn-replay").click();
  await expect(bar).toBeVisible();
  await expect(page.getByTestId("replay-banner")).toHaveText("viewing history");

  // Replay opens at the oldest valid placement: the day-old seeds are there,
  // everything later (the strip, the recent activity pixels) is not.
  const entry = (await page.evaluate(() => window.__freeplace.replay()))!;
  expect(entry.cutoff).toBe(entry.min);
  expect(entry.cutoff).toBeLessThan(stripTs[0]);
  expect(await page.evaluate(() => window.__freeplace.pixel(10, 10))).toBe(5);
  expect(await page.evaluate(() => window.__freeplace.pixel(900, 700))).toBe(11);
  expect(await page.evaluate(() => window.__freeplace.pixel(508, 300))).toBeNull();
  expect(await page.evaluate(() => window.__freeplace.pixel(40, 40))).toBeNull();

  // Scrub to the 4th strip placement: exactly the first four pixels exist.
  await scrubTo(page, stripTs[3]);
  for (let i = 0; i < 8; i++) {
    const color = await page.evaluate((x) => window.__freeplace.pixel(x, 300), 508 + i);
    expect(color).toBe(i <= 3 ? i + 4 : null);
  }

  // Scrub forward: the whole strip is back, in ts order.
  await scrubTo(page, stripTs[7]);
  for (let i = 0; i < 8; i++) {
    const color = await page.evaluate((x) => window.__freeplace.pixel(x, 300), 508 + i);
    expect(color).toBe(i + 4);
  }

  // Play from the start: the cutoff advances on its own and reaches the end
  // of the recorded window (11 unique timestamps at ~20/s), then pauses.
  await scrubTo(page, entry.min);
  await page.getByTestId("btn-replay-play").click();
  await expect
    .poll(() => page.evaluate(() => window.__freeplace.replay()!.cutoff))
    .toBe(entry.max);
  expect(await page.evaluate(() => window.__freeplace.replay()!.playing)).toBe(false);

  // Exit: the live derived board is restored exactly (state was untouched).
  await page.getByTestId("btn-replay").click();
  await expect(bar).toBeHidden();
  expect(await page.evaluate(() => window.__freeplace.replay())).toBeNull();
  expect(await page.evaluate(() => window.__freeplace.pixel(10, 10))).toBe(5);
  expect(await page.evaluate(() => window.__freeplace.pixel(510, 300))).toBe(6);
  expect(await page.evaluate(() => window.__freeplace.pixel(513, 300))).toBe(9);
  expect(await page.evaluate(() => window.__freeplace.pixel(900, 700))).toBe(11);
  expect(await page.evaluate(() => window.__freeplace.pixel(40, 40))).toBe(2);
  expect(await page.evaluate(() => window.__freeplace.pixel(60, 40))).toBe(7);

  expect(errors).toEqual([]);
});

test("placement is refused while replaying", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");
  await expect(page.getByTestId("cooldown")).toHaveText("ready");

  await page.getByTestId("btn-replay").click();
  await expect(page.getByTestId("replay-bar")).toBeVisible();

  const { px, py } = await boardPoint(page, 200, 100);
  await page.mouse.click(px, py);
  const toast = page.getByTestId("error-toast");
  await expect(toast).toBeVisible();
  await expect(toast).toContainText("history");
  expect(await page.evaluate(() => window.__freeplace.cooldownRemainingMs())).toBe(0);

  // Exiting replay: nothing was placed, and placing works again (default
  // selected color is 5).
  await page.getByTestId("btn-replay").click();
  await expect(page.getByTestId("replay-bar")).toBeHidden();
  expect(await page.evaluate(() => window.__freeplace.pixel(200, 100))).toBeNull();
  await page.mouse.click(px, py);
  await expect
    .poll(() => page.evaluate(() => window.__freeplace.pixel(200, 100)))
    .toBe(5);

  expect(errors).toEqual([]);
});

test("onboarding: PoW grind admits when no ghost key is available", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&holdpow=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  const onboarding = page.getByTestId("onboarding");
  await expect(onboarding).toBeVisible();
  await page.getByTestId("btn-ghostkey").click();
  await expect(page.getByTestId("ghostkey-status")).toContainText("asking the ghost key delegate");

  await page.evaluate(() => window.__freeplaceMock.releasePow());
  // Ghost key resolves to its no-key state; the grind carries the admission.
  await expect(page.getByTestId("ghostkey-status")).toContainText("no ghost key available");
  await expect(onboarding).toBeHidden({ timeout: 30_000 });
  await expect(page.getByTestId("identity-chip")).toContainText("proof of work");
  await expect(page.getByTestId("cooldown")).toHaveText("ready");

  expect(errors).toEqual([]);
});
