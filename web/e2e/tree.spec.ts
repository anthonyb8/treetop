// ANB-16. Tapping a tree on the pool opens what the terminal's detail pane
// says about it: its agent and what it asks, git, ports, PR and timeline;
// back lands on the pool where it was left.
import { expect, test } from "@playwright/test";
import { pool, serve, tree17 } from "./fixtures";

test("a tree's screen shows its agent, git, ports, PR and timeline", async ({ page }) => {
  await serve(page, () => pool);
  await page.goto("/");
  await page.getByRole("link", { name: "tree 17" }).click();

  await expect(page).toHaveURL(/\/trees\/17$/);
  await expect(page.getByRole("heading", { name: "END-123 fix-login" })).toBeVisible();
  const agent = page.getByRole("region", { name: "Agent" });
  await expect(agent).toContainText("login");
  await expect(agent).toContainText("waiting on you");
  await expect(agent).toContainText("Which auth provider should the login use, Clerk or Supabase?");

  const facts = page.getByRole("region", { name: "Tree" });
  await expect(facts).toContainText("⇡2");
  await expect(facts).toContainText(":5173 vite");
  await expect(facts).toContainText("#41 ✗ ci");
  await expect(facts).toContainText("test: 2 of 3 passed");
  await expect(facts.getByRole("link", { name: "Open" })).toHaveAttribute("href", "https://github.com/me/app/pull/41");

  const steps = page.getByRole("region", { name: "Timeline" }).getByRole("listitem");
  await expect(steps).toHaveCount(3);
  await expect(steps.first()).toContainText("asked about the auth provider");
  await page.screenshot({ path: "test-results/tree.png", fullPage: true });

  await page.getByRole("button", { name: "Trees" }).click();
  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByRole("region", { name: "Agent" })).toHaveCount(0);
});

test("the phone's back gesture returns to the pool", async ({ page }) => {
  await serve(page, () => pool);
  await page.goto("/");
  await page.getByRole("link", { name: "tree 17" }).click();
  await expect(page.getByRole("region", { name: "Agent" })).toBeVisible();
  await page.goBack();
  await expect(page.getByRole("region", { name: "Agent" })).toHaveCount(0);
  await expect(page.getByRole("region", { name: "Needs you" })).toBeVisible();
});

test("a tree opened from its address goes back to the pool, not out of the app", async ({ page }) => {
  await serve(page, () => pool);
  await page.goto("/trees/17");
  await expect(page.getByRole("region", { name: "Agent" })).toBeVisible();
  await page.getByRole("button", { name: "Trees" }).click();
  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByRole("region", { name: "Agent" })).toHaveCount(0);
  await expect(page.getByRole("region", { name: "Needs you" })).toBeVisible();
});

test("a tree that has left the pool says so", async ({ page }) => {
  await serve(page, () => pool);
  await page.goto("/trees/99");
  await expect(page.getByText("Tree 99 is not in this pool any more.")).toBeVisible();
  await page.screenshot({ path: "test-results/tree-gone.png" });
});

test("a long timeline scrolls below the tree's facts rather than squeezing them", async ({ page }) => {
  await serve(page, () => pool);
  const steps = Array.from({ length: 12 }, (_, i) => ({
    at: 1793500000 + i * 60,
    state: "working" as const,
    detail: `step ${i + 1} of a long run, with enough words to wrap onto a second line`,
  }));
  await page.route("**/api/trees/17", (route) =>
    route.fulfill({ json: { ...tree17, report: { ...tree17.report!, steps } } }),
  );
  await page.goto("/trees/17");
  const facts = page.getByRole("region", { name: "Tree" });
  await expect(facts).toBeVisible();
  await expect(facts.getByText(":5173 vite")).toBeInViewport();
  const box = await facts.boundingBox();
  expect(box?.height ?? 0).toBeGreaterThan(150);
});
