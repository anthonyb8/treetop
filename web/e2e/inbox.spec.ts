// The inbox tab: what wants you in every workspace, in one place, as the
// desktop keeps it beside the list. Opening an entry goes where its
// notification would; swiping one away dismisses it on the desktop too.
import { expect, test, type Page } from "@playwright/test";
import type { Inbox, InboxEntry } from "../src/api";
import { pool, serve, shot, swipe } from "./fixtures";

function entry(tree: string, fields: Partial<InboxEntry> = {}): InboxEntry {
  return {
    key: `NeedsYou:/pool/${tree}:job@1`,
    kind: "needs_you",
    workspace: "app",
    tree,
    title: `END-${tree} work`,
    what: "waiting at a prompt in its chat",
    ago: 300,
    seen: false,
    ...fields,
  };
}

const full: Inbox = {
  workspace: "app",
  unseen: 4,
  is_reading: false,
  entries: [
    entry("17", { title: "END-123 fix-login", what: "Which auth provider should the login use, Clerk or Supabase?", ago: 720 }),
    entry("3", {
      key: "NeedsYou:/other/3:pane@9",
      workspace: "treetop",
      title: "ANB-46 inbox-everywhere",
      what: "waiting at a prompt in its pane",
      ago: null,
    }),
    entry("14", { key: "Failed:/pool/14:abc", kind: "failed", title: "END-140 rate-limits", what: "#44 ✗ e2e", ago: null }),
    entry("12", { key: "Done:/pool/12:job@2", kind: "done", title: "END-131 copy-tweak", what: "Shipped the copy change; PR #43 is up.", ago: 5400 }),
    entry("9", { key: "Merged:/pool/9:def", kind: "merged", title: "END-120 old-thing", what: "merged", ago: null, seen: true }),
  ],
};

/** Serves the pool and `inbox`, keeping what the app marks. */
async function serveInbox(page: Page, inbox: Inbox) {
  const marked: { as: string; key: string }[] = [];
  await serve(page, () => pool);
  await page.route("**/api/inbox**", (route) => {
    const url = new URL(route.request().url());
    if (route.request().method() === "POST") {
      const { key } = route.request().postDataJSON() as { key: string };
      marked.push({ as: url.pathname.split("/").pop()!, key });
      return route.fulfill({ json: {} });
    }
    return route.fulfill({ json: inbox });
  });
  return marked;
}

test("the tab counts what is new and the inbox lists it under the desktop's headings", async ({ page }) => {
  await serveInbox(page, full);
  await page.goto("/");
  const tab = page.getByRole("link", { name: /Inbox/ });
  await expect(tab.getByLabel("4 new in the inbox")).toBeVisible();
  await shot(page, "test-results/inbox-tab.png");

  await tab.click();
  await expect(page).toHaveURL(/\/inbox$/);
  await expect(page.getByRole("heading", { level: 1, name: "Inbox" })).toBeVisible();
  await expect(page.getByText("2 need you · 2 more new")).toBeVisible();
  await expect(page.locator("main:visible h2")).toHaveText(["Needs you", "Failed", "Done", "Seen"]);
  await expect(page.getByRole("region", { name: "Needs you" }).getByRole("button")).toHaveCount(2);
  await expect(page.getByRole("button", { name: "tree 3 in treetop, needs you" })).toContainText("treetop");
  await shot(page, "test-results/inbox-list.png");
});

test("an entry that needs you opens its chat and is marked seen", async ({ page }) => {
  const marked = await serveInbox(page, structuredClone(full));
  await page.goto("/inbox");
  await page.getByRole("button", { name: "tree 17, needs you" }).click();
  await expect(page).toHaveURL(/\/trees\/17\/chat$/);
  expect(marked).toEqual([{ as: "seen", key: full.entries[0]!.key }]);
  await expect(page.getByRole("link", { name: /Inbox/ }).getByLabel("3 new in the inbox")).toBeAttached();
});

test("another workspace's entry switches to that workspace before opening", async ({ page }) => {
  await serveInbox(page, structuredClone(full));
  await page.goto("/inbox");
  const asked = page.waitForRequest((r) => r.url().includes("/api/trees/14?workspace=app"));
  await page.route("**/api/inbox**", (route) =>
    route.fulfill({
      json: {
        workspace: "treetop",
        unseen: 1,
        is_reading: false,
        entries: [entry("14", { kind: "failed", workspace: "app", what: "#44 ✗ e2e", ago: null })],
      } satisfies Inbox,
    }),
  );
  await page.reload();
  await page.getByRole("button", { name: "tree 14 in app, PR failed" }).click();
  await expect(page).toHaveURL(/\/trees\/14$/);
  await asked;
});

test("swiping an entry away dismisses it", async ({ page }) => {
  const marked = await serveInbox(page, structuredClone(full));
  await page.goto("/inbox");
  await swipe(page, ".swipe:has([aria-label='tree 12, done'])", 160);
  await expect(page.getByRole("button", { name: "tree 12, done" })).toHaveCount(0);
  expect(marked).toEqual([{ as: "dismiss", key: full.entries[3]!.key }]);
  await expect(page.getByRole("link", { name: /Inbox/ }).getByLabel("3 new in the inbox")).toBeAttached();
});

test("with nothing waiting the inbox says so", async ({ page }) => {
  await serveInbox(page, { workspace: "app", entries: [], unseen: 0, is_reading: false });
  await page.goto("/inbox");
  await expect(page.getByText("Nothing needs you.")).toBeVisible();
  await expect(page.getByText("All caught up")).toBeVisible();
  await expect(page.getByRole("link", { name: /Inbox/ }).locator(".badge")).toHaveCount(0);
  await shot(page, "test-results/inbox-empty.png");
});

test("while the server is still reading a workspace the inbox keeps what it showed", async ({ page }) => {
  let reading = false;
  await serve(page, () => pool);
  await page.route("**/api/inbox**", (route) =>
    route.fulfill({ json: reading ? { workspace: "app", entries: [], unseen: 0, is_reading: true } : full }),
  );
  await page.goto("/inbox");
  await expect(page.getByRole("button", { name: "tree 17, needs you" })).toBeVisible();
  reading = true;
  await page.evaluate(() => document.dispatchEvent(new Event("visibilitychange")));
  await page.waitForResponse("**/api/inbox**");
  await expect(page.getByText("All caught up")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "tree 17, needs you" })).toBeVisible();
  await expect(page.getByRole("link", { name: /Inbox/ }).getByLabel("4 new in the inbox")).toBeAttached();
});

test("a first answer that is still reading shows the inbox loading, not empty", async ({ page }) => {
  await serve(page, () => pool);
  await page.route("**/api/inbox**", (route) => route.fulfill({ json: { workspace: "app", entries: [], unseen: 0, is_reading: true } }));
  await page.goto("/inbox");
  await page.waitForResponse("**/api/inbox**");
  await expect(page.locator("main:visible [aria-busy='true']")).toBeVisible();
  await expect(page.getByText("Nothing needs you.")).toHaveCount(0);
});

test("after opening another workspace's entry, this workspace's entries still open here", async ({ page }) => {
  await serve(page, () => pool);
  await page.route("**/api/inbox**", (route) => {
    const asked = new URL(route.request().url()).searchParams.get("workspace");
    return route.fulfill({
      json:
        asked === "treetop"
          ? { workspace: "treetop", entries: [], unseen: 0, is_reading: true }
          : {
              workspace: "app",
              unseen: 2,
              is_reading: false,
              entries: [
                entry("17", { kind: "failed", ago: null }),
                entry("3", { key: "Failed:/other/3:x", kind: "failed", workspace: "treetop", ago: null }),
              ],
            },
    });
  });
  await page.goto("/inbox");
  await page.getByRole("button", { name: "tree 3 in treetop, PR failed" }).click();
  await expect(page).toHaveURL(/\/trees\/3$/);
  await page.goBack();
  await expect(page).toHaveURL(/\/inbox$/);
  await expect(page.getByRole("button", { name: "tree 17 in app, PR failed" })).toBeVisible();
  const asked = page.waitForRequest((r) => r.url().includes("/api/trees/17?workspace=app"));
  await page.getByRole("button", { name: "tree 17 in app, PR failed" }).click();
  await asked;
});

test("a poll answered before a swipe does not bring the swiped entry back", async ({ page }) => {
  await serve(page, () => pool);
  let release: () => void = () => undefined;
  let hold = false;
  await page.route("**/api/inbox**", async (route) => {
    if (route.request().method() === "POST") return route.fulfill({ json: {} });
    if (hold) await new Promise<void>((done) => (release = done));
    return route.fulfill({ json: full });
  });
  await page.goto("/inbox");
  await expect(page.getByRole("button", { name: "tree 12, done" })).toBeVisible();
  hold = true;
  const held = page.waitForRequest((r) => r.url().includes("/api/inbox") && r.method() === "GET");
  await page.evaluate(() => document.dispatchEvent(new Event("visibilitychange")));
  await held;
  await swipe(page, ".swipe:has([aria-label='tree 12, done'])", 160);
  await expect(page.getByRole("button", { name: "tree 12, done" })).toHaveCount(0);
  hold = false;
  const answered = page.waitForResponse((r) => r.url().includes("/api/inbox") && r.request().method() === "GET");
  release();
  await answered;
  await expect(page.getByRole("button", { name: "tree 12, done" })).toHaveCount(0);
  await expect(page.getByRole("link", { name: /Inbox/ }).getByLabel("3 new in the inbox")).toBeAttached();
});
