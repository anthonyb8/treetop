// ANB-16. Tapping a tree on the pool opens what the terminal's detail pane
// says about it: its agent and what it asks, git, ports, PR and timeline;
// back lands on the pool where it was left.
import { expect, test } from "@playwright/test";
import { pool, serve, shot, tree17 } from "./fixtures";

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
  await shot(page, "test-results/tree.png", { fullPage: true });

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
  await shot(page, "test-results/tree-gone.png");
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

// A keel session's tree: its run on the card and the screen, its agent
// labelled, and the return sheet warning that the run is not finished.
test("a tree with a keel run shows the run and warns before it is returned", async ({ page }) => {
  const run = {
    playbook: "bug-fix",
    step: "2/6",
    step_title: "Narrow the cause",
    state: "active" as const,
    goal: "users get two notifications after a retry",
    attention: 2,
    stale: false,
    holds: true,
  };
  const card = { ...pool.needs_you[0]!, run };
  const withRun = { ...pool, needs_you: [card] };
  await serve(page, () => withRun);
  await page.route("**/api/trees/17", (route) =>
    route.fulfill({
      json: { ...tree17, card, agents: [{ ...tree17.agents[0]!, label: "keel" }] },
    }),
  );
  await page.goto("/");
  await expect(page.getByRole("link", { name: "tree 17" })).toContainText("bug-fix 2/6 · 2 need attention");
  await page.screenshot({ path: "test-results/pool-run.png" });
  await page.goto("/trees/17");

  await expect(page.getByRole("region", { name: "Agent" })).toContainText("keel");
  const shown = page.getByRole("region", { name: "Run" });
  await expect(shown).toContainText("bug-fix");
  await expect(shown).toContainText("step 2/6 · Narrow the cause");
  await expect(shown).toContainText("users get two notifications after a retry");
  await expect(shown).toContainText("2 need attention");
  await page.screenshot({ path: "test-results/tree-run.png", fullPage: true });

  await page.getByRole("button", { name: "More" }).click();
  await page.getByRole("button", { name: "Return…" }).click();
  const sheet = page.getByRole("dialog", { name: "Return tree 17?" });
  await expect(sheet).toContainText("keel run bug-fix is active at step 2/6");
  await expect(sheet).toContainText("keel run stop");
  await page.screenshot({ path: "test-results/return-sheet-run.png" });
});
