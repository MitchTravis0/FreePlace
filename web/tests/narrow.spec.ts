// Phase 10: responsive layout. The same offline mock tier as offline.spec.ts
// but at a phone-sized viewport (390x844), run as a sibling spec in the
// existing desktop project — the offline spec itself is unchanged at the
// default viewport. Console must be clean in every flow.

import { expect, Page, test } from "@playwright/test";

test.use({ viewport: { width: 390, height: 844 } });

function watchConsole(page: Page): string[] {
  const errors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("pageerror", (error) => errors.push(String(error)));
  return errors;
}

async function injectChat(page: Page, content: string): Promise<void> {
  await page.evaluate(
    (text) =>
      (
        window as unknown as { __freeplaceMock: { injectChat(c: string): void } }
      ).__freeplaceMock.injectChat(text),
    content,
  );
}

/// The page itself must never scroll horizontally at 390px.
async function assertNoHorizontalScroll(page: Page): Promise<void> {
  const overflow = await page.evaluate(() => ({
    doc: document.documentElement.scrollWidth - document.documentElement.clientWidth,
    body: document.body.scrollWidth - document.body.clientWidth,
  }));
  expect(overflow.doc).toBeLessThanOrEqual(0);
  expect(overflow.body).toBeLessThanOrEqual(0);
}

test("chat is a toggled overlay with an unread badge", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  const panel = page.getByTestId("chat-panel");
  const toggle = page.getByTestId("btn-chat-toggle");
  const badge = page.getByTestId("chat-unread");
  await expect(panel).toBeHidden();
  await expect(toggle).toBeVisible();
  // The two seeded messages arrived while the panel was closed.
  await expect(badge).toHaveText("2");

  await injectChat(page, "remote peer says hi");
  await expect(badge).toHaveText("3");

  await toggle.click();
  await expect(panel).toBeVisible();
  await expect(page.getByTestId("chat-messages")).toContainText("remote peer says hi");
  await expect(badge).toBeHidden();

  // Messages arriving while the panel is open are already seen. (A second
  // remote inject would be dropped by eve's per-author chat rate limit, so
  // send as ourselves.)
  await page.getByTestId("chat-input").fill("mine while open");
  await page.getByTestId("chat-send").click();
  await expect(page.getByTestId("chat-messages")).toContainText("mine while open");
  await expect(badge).toBeHidden();

  await toggle.click();
  await expect(panel).toBeHidden();

  expect(errors).toEqual([]);
});

test("palette targets are at least 44px and selectable", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  // 16 classic swatches plus the wheel-picker toggle, all >= 44px targets.
  const buttons = page.getByTestId("palette").locator("button[data-color]");
  await expect(buttons).toHaveCount(16);
  const first = (await buttons.first().boundingBox())!;
  expect(first.width).toBeGreaterThanOrEqual(44);
  expect(first.height).toBeGreaterThanOrEqual(44);
  const toggle = (await page.getByTestId("btn-picker").boundingBox())!;
  expect(toggle.width).toBeGreaterThanOrEqual(44);
  expect(toggle.height).toBeGreaterThanOrEqual(44);

  // The strip scrolls horizontally instead of overflowing the page.
  const palette = (await page.getByTestId("palette").boundingBox())!;
  expect(palette.width).toBeLessThanOrEqual(390);
  await buttons.nth(15).scrollIntoViewIfNeeded();
  await buttons.nth(15).click();
  await expect(buttons.nth(15)).toHaveClass(/selected/);

  await assertNoHorizontalScroll(page);
  expect(errors).toEqual([]);
});

test("the color picker fits a 390px viewport", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");

  await page.getByTestId("btn-picker").scrollIntoViewIfNeeded();
  await page.getByTestId("btn-picker").click();
  const panel = (await page.getByTestId("picker-panel").boundingBox())!;
  expect(panel.x).toBeGreaterThanOrEqual(0);
  expect(panel.x + panel.width).toBeLessThanOrEqual(390);
  await assertNoHorizontalScroll(page);

  expect(errors).toEqual([]);
});

test("the onboarding card fits a 390px viewport", async ({ page }) => {
  const errors = watchConsole(page);
  // holdpow parks the grind so the overlay stays up deterministically.
  await page.goto("/?mock=1&holdpow=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");
  await expect(page.getByTestId("onboarding")).toBeVisible();

  const card = (await page.getByTestId("onboarding").locator(".onboarding-card").boundingBox())!;
  expect(card.x).toBeGreaterThanOrEqual(0);
  expect(card.x + card.width).toBeLessThanOrEqual(390);
  await assertNoHorizontalScroll(page);

  expect(errors).toEqual([]);
});

test("the FAQ card fits and the main view has no horizontal scroll", async ({ page }) => {
  const errors = watchConsole(page);
  await page.goto("/?mock=1&admitted=1");
  await expect(page.getByTestId("conn-status")).toHaveText("connected");
  await assertNoHorizontalScroll(page);

  await page.getByTestId("btn-faq").click();
  await expect(page.getByTestId("faq")).toBeVisible();
  const card = (await page.getByTestId("faq").locator(".onboarding-card").boundingBox())!;
  expect(card.x).toBeGreaterThanOrEqual(0);
  expect(card.x + card.width).toBeLessThanOrEqual(390);
  await assertNoHorizontalScroll(page);

  expect(errors).toEqual([]);
});
