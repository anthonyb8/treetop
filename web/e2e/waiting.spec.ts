// The Waiting tab (ANB-62): what waits on you in every workspace, the
// oldest first, as the desktop's banner names it, each until it is dealt
// with; what finished or merged is one line at the foot. Opening one goes
// where its notification would.
import { expect, test, type Page } from "@playwright/test";
import type { Waiting, WaitingEntry } from "../src/api";
import { pool, serve, shot } from "./fixtures";

function entry(tree: string, fields: Partial<WaitingEntry> = {}): WaitingEntry {
  return {
    key: `needs_you:/pool/${tree}`,
    kind: "needs_you",
    workspace: "app",
    tree,
    title: `END-${tree} work`,
    what: "waiting at a prompt in its chat",
    ago: 300,
    ...fields,
  };
}

const full: Waiting = {
  workspace: "app",
  is_reading: false,
  entries: [
    entry("9", { key: "died:/pool/9", kind: "died", title: "END-90 port-menu", what: "its agent died", ago: 1900 }),
    entry("17", { title: "END-123 fix-login", what: "Which auth provider should the login use, Clerk or Supabase?", ago: 720 }),
    entry("3", {
      key: "needs_you:/other/3",
      workspace: "treetop",
      title: "ANB-46 inbox-everywhere",
      what: "waiting at a prompt in its pane",
      ago: 360,
    }),
    entry("14", { key: "failed:/pool/14", kind: "failed", title: "END-140 rate-limits", what: "#44 ✗ e2e", ago: null }),
  ],
  also: [
    { kind: "done", workspace: "app", tree: "12", title: "END-131 copy-tweak" },
    { kind: "merged", workspace: "app", tree: "4", title: "END-120 old-thing" },
  ],
};

async function serveWaiting(page: Page, waiting: Waiting) {
  await serve(page, () => pool);
  await page.route("**/api/waiting**", (route) => route.fulfill({ json: waiting }));
}

test("the tab counts what waits and lists it, the oldest first, with what finished below", async ({ page }) => {
  await serveWaiting(page, full);
  await page.goto("/");
  const tab = page.getByRole("link", { name: /Waiting/ });
  await expect(tab.getByLabel("4 waiting in every workspace")).toBeVisible();
  await shot(page, "test-results/waiting-tab.png");

  await tab.click();
  await expect(page).toHaveURL(/\/waiting$/);
  await expect(page.getByRole("heading", { level: 1, name: "Waiting" })).toBeVisible();
  await expect(page.getByText("4 waiting on you")).toBeVisible();
  const rows = page.locator("main:visible .entry");
  await expect(rows).toHaveCount(4);
  await expect(rows.nth(0)).toHaveAccessibleName("tree 9, agent died");
  await expect(rows.nth(1)).toHaveAccessibleName("tree 17, needs you");
  await expect(page.getByRole("button", { name: "tree 3 in treetop, needs you" })).toContainText("treetop");
  await expect(page.getByText("Also: 12 finished · 4 merged")).toBeVisible();
  await shot(page, "test-results/waiting-list.png");
});

test("an ask opens its agent's chat, and a failed PR its tree", async ({ page }) => {
  await serveWaiting(page, full);
  await page.goto("/waiting");
  await page.getByRole("button", { name: "tree 17, needs you" }).click();
  await expect(page).toHaveURL(/\/trees\/17\/chat$/);
  await page.goBack();
  await page.getByRole("button", { name: "tree 14, PR failed" }).click();
  await expect(page).toHaveURL(/\/trees\/14$/);
});

test("another workspace's entry switches to that workspace before opening", async ({ page }) => {
  await serveWaiting(page, full);
  await page.goto("/waiting");
  const asked = page.waitForRequest((r) => r.url().includes("/api/trees/3?workspace=treetop"));
  await page.getByRole("button", { name: "tree 3 in treetop, needs you" }).click();
  await expect(page).toHaveURL(/\/trees\/3\/chat$/);
  await asked;
});

test("after opening another workspace's entry, this workspace's entries still open here", async ({ page }) => {
  await serve(page, () => pool);
  await page.route("**/api/waiting**", (route) => {
    const asked = new URL(route.request().url()).searchParams.get("workspace");
    return route.fulfill({
      json:
        asked === "treetop"
          ? { workspace: "treetop", entries: [], also: [], is_reading: true }
          : {
              workspace: "app",
              is_reading: false,
              also: [],
              entries: [
                entry("17", { key: "failed:/pool/17", kind: "failed", ago: null }),
                entry("3", { key: "failed:/other/3", kind: "failed", workspace: "treetop", ago: null }),
              ],
            },
    });
  });
  await page.goto("/waiting");
  await page.getByRole("button", { name: "tree 3 in treetop, PR failed" }).click();
  await expect(page).toHaveURL(/\/trees\/3$/);
  await page.goBack();
  await expect(page).toHaveURL(/\/waiting$/);
  const seventeen = page.getByRole("button", { name: "tree 17 in app, PR failed" });
  await expect(seventeen).toBeVisible();
  const asked = page.waitForRequest((r) => r.url().includes("/api/trees/17?workspace=app"));
  await seventeen.click();
  await asked;
});

test("with nothing waiting the tab says so and counts nothing", async ({ page }) => {
  await serveWaiting(page, { workspace: "app", entries: [], also: [], is_reading: false });
  await page.goto("/waiting");
  await expect(page.getByText("Nothing is waiting on you.")).toBeVisible();
  await expect(page.getByText("All caught up")).toBeVisible();
  await expect(page.getByRole("link", { name: /Waiting/ }).locator(".badge")).toHaveCount(0);
  await expect(page.getByText(/^Also:/)).toHaveCount(0);
  await shot(page, "test-results/waiting-empty.png");
});

test("the old inbox address opens the Waiting tab", async ({ page }) => {
  await serveWaiting(page, full);
  await page.goto("/inbox");
  await expect(page.getByRole("heading", { level: 1, name: "Waiting" })).toBeVisible();
});

test("while the server is still reading a workspace the tab keeps what it showed", async ({ page }) => {
  let reading = false;
  await serve(page, () => pool);
  await page.route("**/api/waiting**", (route) =>
    route.fulfill({ json: reading ? { workspace: "app", entries: [], also: [], is_reading: true } : full }),
  );
  await page.goto("/waiting");
  await expect(page.getByRole("button", { name: "tree 17, needs you" })).toBeVisible();
  reading = true;
  await page.evaluate(() => document.dispatchEvent(new Event("visibilitychange")));
  await page.waitForResponse("**/api/waiting**");
  await expect(page.getByText("All caught up")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "tree 17, needs you" })).toBeVisible();
});

test("a first answer that is still reading shows the tab loading, not empty", async ({ page }) => {
  await serve(page, () => pool);
  await page.route("**/api/waiting**", (route) =>
    route.fulfill({ json: { workspace: "app", entries: [], also: [], is_reading: true } }),
  );
  await page.goto("/waiting");
  await page.waitForResponse("**/api/waiting**");
  await expect(page.locator("main:visible [aria-busy='true']")).toBeVisible();
  await expect(page.getByText("Nothing is waiting on you.")).toHaveCount(0);
});
