// ANB-17. An agent's chat on the phone: the conversation as it happens,
// each tool call folded to a line, and a line you send shown at once,
// settled once the agent has it, or kept with a retry when it could not go.
import { expect, test, type Page } from "@playwright/test";
import type { ChatMessage } from "../src/api";
import { pool, serve, tree17 } from "./fixtures";

const backlog: ChatMessage[] = [
  { id: "u1", kind: "you", at: "2026-11-01T13:40:00Z", text: "Fix the login redirect loop on expired sessions." },
  {
    id: "a1:0",
    kind: "claude",
    at: "2026-11-01T13:41:00Z",
    text: "The guard redirects **before** the token refresh finishes. Moving the check:\n\n```ts\nawait refresh();\n```",
  },
  {
    id: "t1",
    kind: "tool",
    tool: { verb: "Edited", what: "src/auth/guard.ts", added: 12, removed: 4, status: "ok", output: "ok", ask: null },
  },
  {
    id: "t2",
    kind: "tool",
    tool: { verb: "Ran", what: "Run the tests", added: null, removed: null, status: "error", output: "1 failed: expects a Clerk session", ask: null },
  },
  { id: "a2:0", kind: "claude", text: "Which auth provider should the login use, Clerk or Supabase?" },
];

function events(...parts: [string, unknown][]): string {
  return "retry: 200\n\n" + parts.map(([name, data]) => `event: ${name}\ndata: ${JSON.stringify(data)}\n\n`).join("");
}

/** Answers the chat stream: the first connection with `first`, each later one with `later`. */
async function stream(page: Page, first: string, later: () => string = () => ": quiet\n\n") {
  let connections = 0;
  await page.route("**/api/trees/17/chat**", (route) => {
    connections += 1;
    return route.fulfill({
      status: 200,
      headers: { "Content-Type": "text/event-stream" },
      body: connections === 1 ? first : later(),
    });
  });
}

/** Tree 17's agent at work, so a line is a message rather than an answer to what it waits on. */
async function atWork(page: Page) {
  await page.route(
    (url) => url.pathname === "/api/trees/17",
    (route) => route.fulfill({ json: { ...tree17, agents: [{ ...tree17.agents[0]!, status: "busy" }] } }),
  );
}

test("the chat shows the conversation, each tool call a line that opens", async ({ page }) => {
  await serve(page, () => pool);
  await stream(page, events(["backlog", backlog]));
  await page.goto("/trees/17");
  await page.getByRole("link", { name: "Answer in chat" }).click();
  await expect(page).toHaveURL(/\/trees\/17\/chat$/);

  const chat = page.getByRole("main", { name: "Conversation" });
  await expect(chat.getByText("Fix the login redirect loop on expired sessions.")).toBeVisible();
  await expect(chat.locator("strong", { hasText: "before" })).toBeVisible();
  await expect(chat.locator("pre code")).toHaveText("await refresh();");
  await expect(chat.getByRole("button", { name: /Edited src\/auth\/guard\.ts \+12 −4/ })).toBeVisible();

  const run = chat.getByRole("button", { name: /Ran Run the tests/ });
  await expect(run.getByRole("img", { name: "failed" })).toBeVisible();
  await expect(chat.getByText("1 failed: expects a Clerk session")).toHaveCount(0);
  await run.click();
  await expect(chat.getByText("1 failed: expects a Clerk session")).toBeVisible();
  // The question is marked where the agent asked it, not said again below.
  await expect(chat.locator(".asks")).toContainText("Needs you");
  await expect(chat.locator(".asks")).toContainText("Which auth provider should the login use, Clerk or Supabase?");
  await expect(page.getByRole("region", { name: "Waiting on you" })).toHaveCount(0);
  await page.screenshot({ path: "test-results/chat.png" });

  await page.getByRole("button", { name: "Back to tree 17" }).click();
  await expect(page).toHaveURL(/\/trees\/17$/);
  await expect(page.getByRole("main", { name: "Conversation" })).toHaveCount(0);
});

test("a line you send shows at once, then settles once the agent has it", async ({ page }) => {
  await serve(page, () => pool);
  await atWork(page);
  let isSent = false;
  await stream(page, events(["backlog", backlog]), () =>
    isSent ? events(["message", { id: "u9", kind: "you", text: "Use Clerk." }]) : ": quiet\n\n",
  );
  let release: () => void = () => {};
  const held = new Promise<void>((resolve) => (release = resolve));
  await page.route("**/api/trees/17/send**", async (route) => {
    expect(route.request().postDataJSON()).toEqual({ text: "Use Clerk." });
    await held;
    isSent = true;
    await route.fulfill({ json: { said: "sent to login in tree 17" } });
  });
  await page.goto("/trees/17/chat");
  await expect(page.getByText("Fix the login redirect loop on expired sessions.")).toBeVisible();

  await page.getByRole("textbox", { name: "Message 17" }).fill("Use Clerk.");
  await page.getByRole("button", { name: "Send" }).click();
  await expect(page.getByText("Sending…")).toBeVisible();
  await expect(page.getByRole("textbox", { name: "Message 17" })).toHaveValue("");
  await page.screenshot({ path: "test-results/chat-sending.png" });
  release();
  await expect(page.getByText("Sending…")).toHaveCount(0);
  await expect(page.getByText("Use Clerk.")).toHaveCount(1);
  await expect(page.getByText("Sent")).toHaveCount(0);
});

test("a line that could not go says why and goes on a retry", async ({ page }) => {
  await serve(page, () => pool);
  await atWork(page);
  await stream(page, events(["backlog", backlog]));
  let tries = 0;
  await page.route("**/api/trees/17/send**", (route) => {
    tries += 1;
    return tries === 1
      ? route.fulfill({ status: 502, json: { error: "no agent with a chat to type into at 17" } })
      : route.fulfill({ json: { said: "sent" } });
  });
  await page.goto("/trees/17/chat");
  await page.getByRole("textbox", { name: "Message 17" }).fill("Use Clerk.");
  await page.getByRole("button", { name: "Send" }).click();
  await expect(page.getByText("no agent with a chat to type into at 17 ·")).toBeVisible();
  await page.screenshot({ path: "test-results/chat-failed.png" });
  await page.getByRole("button", { name: "Retry" }).click();
  await expect(page.getByText("Sent")).toBeVisible();
});

test("scrolled up, a new message waits behind a pill instead of moving the chat", async ({ page }) => {
  await serve(page, () => pool);
  const long: ChatMessage[] = Array.from({ length: 30 }, (_, i) => ({
    id: `c${i}`,
    kind: "claude" as const,
    text: `Step ${i + 1}: a paragraph long enough to take up a few lines on a phone's narrow screen.`,
  }));
  let isScrolled = false;
  await stream(page, events(["backlog", long]), () =>
    isScrolled ? events(["message", { id: "new", kind: "claude", text: "Tests pass now." }]) : ": quiet\n\n",
  );
  await page.goto("/trees/17/chat");
  const chat = page.getByRole("main", { name: "Conversation" });
  await expect(chat.getByText("Step 30:", { exact: false })).toBeInViewport();

  await chat.evaluate((el) => (el.scrollTop = 0));
  isScrolled = true;
  const pill = page.getByRole("button", { name: "1 new message" });
  await expect(pill).toBeVisible();
  await expect(chat.getByText("Step 1:", { exact: false })).toBeInViewport();
  await page.screenshot({ path: "test-results/chat-pill.png" });
  await pill.click();
  await expect(chat.getByText("Tests pass now.")).toBeInViewport();
});

test("what you were typing is still there when you come back", async ({ page }) => {
  await serve(page, () => pool);
  await stream(page, events(["backlog", backlog]), () => events(["backlog", backlog]));
  await page.goto("/trees/17/chat");
  await page.getByRole("textbox", { name: "Message 17" }).fill("Half a thought");
  await page.getByRole("button", { name: "Back to tree 17" }).click();
  await page.getByRole("link", { name: "Answer in chat" }).click();
  await expect(page.getByRole("textbox", { name: "Message 17" })).toHaveValue("Half a thought");
});
