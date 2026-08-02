// Phase 5 exit check, from a browser page: create an identity, persist it
// across reload, sign a placement the tile contract accepts, and drive the
// ghost key flow to a signature or one of its two expected non-error states.
// Requires the fixtures published by scripts/phase5-smoke.sh.

import { expect, test } from "@playwright/test";

const NODE = process.env.WS_API_HOST ?? "127.0.0.1:7510";

test("identity, admission, placement, and ghost key flow", async ({ page }) => {
  const consoleErrors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") consoleErrors.push(message.text());
  });
  page.on("pageerror", (error) => consoleErrors.push(String(error)));

  await page.goto(`/phase5.html?node=${NODE}`);
  await expect(page.getByTestId("conn-status")).toHaveText("connected", { timeout: 15_000 });

  // Identity is created by the delegate on first request...
  await expect(page.getByTestId("identity-vk")).toHaveText(/^[0-9a-f]{64}$/, { timeout: 15_000 });
  const verifyingKey = await page.getByTestId("identity-vk").textContent();

  // ...and persists across a reload (the key lives in the node's secret
  // store, not in the page).
  await page.reload();
  await expect(page.getByTestId("conn-status")).toHaveText("connected", { timeout: 15_000 });
  await expect(page.getByTestId("identity-vk")).toHaveText(verifyingKey!, { timeout: 15_000 });

  // PoW admission: challenge from the delegate, grind in the page, sign in
  // the delegate, update the registry contract.
  await page.getByTestId("btn-admit").click();
  await expect(page.getByTestId("admit-status")).toHaveText("admitted", { timeout: 90_000 });

  // Placement signed by the delegate and accepted by the tile contract
  // (confirmed by reading the placement back out of the stored tile state).
  await page.getByTestId("btn-place").click();
  await expect(page.getByTestId("place-status")).toHaveText("accepted", { timeout: 30_000 });

  // Ghost key flow ends in one of its three expected states, never an error.
  await page.getByTestId("btn-ghostkey").click();
  await expect(page.getByTestId("ghostkey-status")).toHaveText(
    /^(signature|no-identity|access-denied)/,
    { timeout: 30_000 },
  );

  expect(consoleErrors).toEqual([]);
});
