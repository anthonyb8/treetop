// ANB-19. From the phone you start an agent in a fresh tree or the tree you
// are on, start a stopped one again, and give a finished tree back after
// seeing what it holds; a refusal says why.
import { expect, test } from "@playwright/test";
import type { Tree } from "../src/api";
import { pool, serve, shot, tree17 } from "./fixtures";

test("an agent started from the pool lands on its tree", async ({ page }) => {
  await serve(page, () => pool);
  let asked: unknown = null;
  await page.route("**/api/dispatch**", async (route) => {
    asked = route.request().postDataJSON();
    await new Promise((resolve) => setTimeout(resolve, 300));
    await route.fulfill({ json: { tree: "17" } });
  });
  await page.goto("/");
  await page.getByRole("link", { name: "New agent" }).click();
  await expect(page).toHaveURL(/\/new$/);
  await page.getByRole("textbox", { name: "Task" }).fill("Add rate limiting to the public API.");
  await expect(page.getByRole("radio", { name: /A fresh tree/ })).toBeChecked();
  await page.getByRole("combobox", { name: "Base" }).selectOption("feature/billing");
  await shot(page, "test-results/new-agent.png");
  await page.getByRole("button", { name: "Start agent" }).click();
  await expect(page.getByRole("button", { name: "Starting…" })).toBeDisabled();
  await expect(page).toHaveURL(/\/trees\/17$/);
  expect(asked).toEqual({ task: "Add rate limiting to the public API.", tree: null, base: "feature/billing" });
});

test("an agent started on a tree keeps that tree", async ({ page }) => {
  await serve(page, () => pool);
  let asked: unknown = null;
  await page.route("**/api/dispatch**", (route) => {
    asked = route.request().postDataJSON();
    return route.fulfill({ json: { tree: "14" } });
  });
  await page.route("**/api/trees/14", (route) =>
    route.fulfill({ json: { ...tree17, card: pool.groups[0]!.cards[0]!, report: null } }),
  );
  await page.goto("/trees/14");
  await page.getByRole("link", { name: "New agent here" }).click();
  await expect(page.getByRole("radio", { name: /Tree 14/ })).toBeChecked();
  await expect(page.getByRole("combobox", { name: "Base" })).toHaveCount(0);
  await page.getByRole("textbox", { name: "Task" }).fill("Write the docs.");
  await page.getByRole("button", { name: "Start agent" }).click();
  await expect(page).toHaveURL(/\/trees\/14$/);
  expect(asked).toEqual({ task: "Write the docs.", tree: "14", base: null });
});

test("a tree is returned after its sheet says what it holds", async ({ page }) => {
  await serve(page, () => pool);
  let returned = false;
  await page.route("**/api/trees/17/return**", (route) => {
    returned = true;
    return route.fulfill({ json: {} });
  });
  await page.goto("/trees/17");
  await page.getByRole("button", { name: "More" }).click();
  await page.getByRole("button", { name: "Return…" }).click();
  const sheet = page.getByRole("dialog", { name: "Return tree 17?" });
  await expect(sheet).toContainText("2 commits on no remote");
  await expect(sheet).toContainText("3 uncommitted files");
  await expect(sheet).toContainText("Its agent is stopped first.");
  await shot(page, "test-results/return-sheet.png");
  await sheet.getByRole("button", { name: "Cancel" }).click();
  await expect(sheet).toHaveCount(0);
  expect(returned).toBe(false);

  await page.getByRole("button", { name: "More" }).click();
  await page.getByRole("button", { name: "Return…" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Return", exact: true }).click();
  await expect(page).toHaveURL(/\/$/);
  expect(returned).toBe(true);
});

test("a return the server refuses says why and leaves the tree", async ({ page }) => {
  await serve(page, () => pool);
  await page.route("**/api/trees/17/return**", (route) =>
    route.fulfill({
      status: 409,
      json: { error: "tree 17 has vite (4242) running in it; stop it, or return the tree from treetop" },
    }),
  );
  await page.goto("/trees/17");
  await page.getByRole("button", { name: "More" }).click();
  await page.getByRole("button", { name: "Return…" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Return", exact: true }).click();
  await expect(page.getByRole("alert")).toHaveText(
    "tree 17 has vite (4242) running in it; stop it, or return the tree from treetop",
  );
  await expect(page).toHaveURL(/\/trees\/17$/);
  await shot(page, "test-results/return-refused.png");
});

test("a stopped agent is started again from its tree", async ({ page }) => {
  await serve(page, () => pool);
  const cold: Tree = { ...tree17, agents: [{ ...tree17.agents[0]!, status: "cold" }] };
  let isResumed = false;
  await page.route("**/api/trees/17", (route) =>
    route.fulfill({
      json: isResumed ? { ...tree17, agents: [{ ...tree17.agents[0]!, status: "busy" }] } : cold,
    }),
  );
  await page.route("**/api/trees/17/resume**", (route) => {
    isResumed = true;
    return route.fulfill({ json: {} });
  });
  await page.goto("/trees/17");
  await page.getByRole("button", { name: "Resume" }).click();
  await expect(page.getByRole("button", { name: "Resume" })).toHaveCount(0);
  await expect(page.getByRole("link", { name: "New agent here" })).toBeVisible();
  expect(isResumed).toBe(true);
});
