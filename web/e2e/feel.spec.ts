// ANB-22. The phone's chat reads like a chat with Claude: dots while the
// agent works, naming what it is doing; a new reply written out word by
// word; what was there on opening shown at once; no motion for a reader
// who asked for less.
import { expect, test, type Page } from "@playwright/test";
import type { ChatMessage, Tree } from "../src/api";
import { pool, serve, tree17 } from "./fixtures";

const reply =
  "The guard redirects before the token refresh finishes, so an expired session bounces between the login page and the app. Moving the check after the refresh fixes it.";

/** Tree 17 at work rather than waiting on you. */
const busy: Tree = {
  ...tree17,
  card: { ...tree17.card, mark: "busy", needs_you: false },
  agents: [{ ...tree17.agents[0]!, status: "busy" }],
  report: { state: "working", says: "fixing the redirect", steps: [] },
};

function events(...parts: [string, unknown][]): string {
  return "retry: 200\n\n" + parts.map(([name, data]) => `event: ${name}\ndata: ${JSON.stringify(data)}\n\n`).join("");
}

/** Opens tree 17's chat: `first` on connecting, `later` on each reconnect. */
async function chat(page: Page, tree: Tree, first: string, later: () => string = () => ": quiet\n\n") {
  await serve(page, () => pool);
  await page.route("**/api/trees/17", (route) => route.fulfill({ json: tree }));
  let connections = 0;
  await page.route("**/api/trees/17/chat**", (route) => {
    connections += 1;
    return route.fulfill({
      headers: { "Content-Type": "text/event-stream" },
      body: connections === 1 ? first : later(),
    });
  });
  await page.goto("/trees/17/chat");
}

/** Every length the newest reply's text had, as it was drawn, from the
 * page's first paint; call it before the chat opens. */
async function watchReplies(page: Page) {
  await page.addInitScript(() => {
    const seen: number[] = [];
    (window as unknown as { lengths: number[] }).lengths = seen;
    new MutationObserver(() => {
      const replies = document.querySelectorAll(".claude");
      const last = replies[replies.length - 1];
      if (last) seen.push(last.textContent?.length ?? 0);
    }).observe(document, { subtree: true, childList: true, characterData: true });
  });
}

const lengths = (page: Page) => page.evaluate(() => (window as unknown as { lengths: number[] }).lengths);
const forget = (page: Page) => page.evaluate(() => void ((window as unknown as { lengths: number[] }).lengths.length = 0));

/** A command under way: running, or waiting on your permission to run. */
const running: ChatMessage = {
  id: "t1",
  kind: "tool",
  tool: { verb: "Ran", what: "Run the tests", added: null, removed: null, status: "running", output: null, ask: null },
};

test("while the agent works, the dots say what it is doing", async ({ page }) => {
  await chat(page, busy, events(["backlog", [{ id: "u1", kind: "you", text: "Fix it." }, running]]));
  await expect(page.getByRole("status", { name: "Claude is working: Run the tests" })).toBeVisible();
  await page.screenshot({ path: "test-results/chat-working.png" });
});

test("an agent waiting on you shows no dots, though its last report says it is working", async ({ page }) => {
  // As a session shows a permission prompt: its live status waiting, its
  // report still the working one it wrote before.
  const waiting: Tree = {
    ...busy,
    card: { ...busy.card, needs_you: true },
    agents: [{ ...busy.agents[0]!, status: "waiting" }],
  };
  await chat(page, waiting, events(["backlog", [{ id: "u1", kind: "you", text: "Fix it." }, running]]));
  await expect(page.getByRole("region", { name: "Waiting on you" })).toBeVisible();
  await expect(page.getByRole("status", { name: /Claude is working/ })).toHaveCount(0);
});

test("a new reply is written out, while what was there on opening is not", async ({ page }) => {
  let isWritten = false;
  await watchReplies(page);
  await chat(page, busy, events(["backlog", [{ id: "a0", kind: "claude", text: reply }]]), () =>
    isWritten ? events(["message", { id: "a1", kind: "claude", text: reply }]) : ": quiet\n\n",
  );
  await expect(page.locator(".claude")).toHaveText(reply);
  expect((await lengths(page)).filter((length) => length > 0 && length < reply.length)).toEqual([]);
  await forget(page);
  isWritten = true;
  await expect(page.locator(".claude")).toHaveCount(2);
  await expect(page.locator(".claude").nth(1)).toHaveText(reply);
  const drawn = await lengths(page);
  const growing = drawn.filter((length) => length > 0 && length < reply.length);
  expect(growing.length).toBeGreaterThan(3);
});

test("with less motion asked for, a new reply is there at once", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  let isWritten = false;
  await watchReplies(page);
  await chat(page, busy, events(["backlog", []]), () =>
    isWritten ? events(["message", { id: "a1", kind: "claude", text: reply }]) : ": quiet\n\n",
  );
  await expect(page.getByRole("status", { name: /Claude is working/ })).toBeVisible();
  isWritten = true;
  await expect(page.locator(".claude")).toHaveText(reply);
  const drawn = await lengths(page);
  expect(drawn.filter((length) => length > 0 && length < reply.length)).toEqual([]);
});

test("an agent back at work after an answer shows dots, not its stale question", async ({ page }) => {
  // Its report still says it waits on you; its live status says it is busy.
  const answered: Tree = {
    ...busy,
    card: { ...busy.card, mark: "waiting", needs_you: true },
    report: { state: "blocked", says: "Which auth provider?", steps: [] },
  };
  await chat(page, answered, events(["backlog", [{ id: "u1", kind: "you", text: "Use Clerk." }]]));
  await expect(page.getByRole("status", { name: /Claude is working/ })).toBeVisible();
  await expect(page.getByRole("region", { name: "Waiting on you" })).toHaveCount(0);
});
