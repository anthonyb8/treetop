// ANB-15. Opening treetop on the phone shows every held tree the way the
// terminal's list does, with what waits on you lifted to the top, and keeps
// showing the last pool, said to be old, while the server is out of reach.
import { expect, test } from "@playwright/test";
import { pool, serve } from "./fixtures";

test("the pool shows what needs you first, then each base's trees", async ({ page }) => {
  await serve(page, () => pool);
  await page.goto("/");

  const needsYou = page.getByRole("region", { name: "Needs you" });
  await expect(needsYou.getByRole("link", { name: "tree 17" })).toContainText(
    "Which auth provider should the login use, Clerk or Supabase?",
  );
  await expect(needsYou.getByRole("img", { name: "waiting on you" })).toBeVisible();
  await expect(needsYou).toContainText("#41 ✗ ci");

  const main = page.getByRole("region", { name: "main" });
  const trees = main.getByRole("link");
  await expect(trees).toHaveText([/14\s*END-140 rate-limits/, /12\s*END-131 copy-tweak/]);
  await expect(main).toContainText(":7777");
  await expect(page.getByText("3 free trees")).toBeVisible();
  await expect(page.getByText("1 needs you · 3 free")).toBeVisible();
  await page.screenshot({ path: "test-results/pool.png" });

  await expect(page.getByRole("link", { name: "tree 6" })).toHaveCount(0);
  await page.getByRole("button", { name: "Parked · 1" }).click();
  await expect(page.getByRole("link", { name: "tree 6" })).toContainText("END-152 invoices");
  await page.screenshot({ path: "test-results/pool-parked.png", fullPage: true });
});

test("with the server out of reach, the last pool still shows and says so", async ({ page }) => {
  let isDown = false;
  await serve(page, () => (isDown ? "down" : pool));
  await page.goto("/");
  await expect(page.getByRole("link", { name: "tree 17" })).toBeVisible();

  isDown = true;
  await page.reload();
  await expect(page.getByRole("status")).toHaveText(
    "Can't reach the server - showing the pool as it last was.",
  );
  await expect(page.getByRole("link", { name: "tree 17" })).toBeVisible();
  await page.screenshot({ path: "test-results/pool-offline.png" });
});

test("a first open with the server out of reach says so and offers a retry", async ({ page }) => {
  // The screen asks again every few seconds on its own; with the clock held,
  // only the button can bring the pool back.
  await page.clock.install({ time: new Date("2026-11-01T09:00:00Z") });
  await page.clock.pauseAt(new Date("2026-11-01T09:00:01Z"));
  let isDown = true;
  await serve(page, () => (isDown ? "down" : pool));
  await page.goto("/");
  await expect(page.getByText("Can't reach the server.")).toBeVisible();
  await page.screenshot({ path: "test-results/pool-unreachable.png" });

  isDown = false;
  await page.getByRole("button", { name: "Try again" }).click();
  await expect(page.getByRole("link", { name: "tree 17" })).toBeVisible();
});
