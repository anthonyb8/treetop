// ANB-18. An agent blocked on a prompt the chat cannot answer, such as a
// question with options, is answered from the phone on the agent's own
// screen: its lines as the terminal draws them, and keys that go in order.
import { expect, test, type Page } from "@playwright/test";
import type { TerminalScreen } from "../src/api";
import { pool, serve, tree17 } from "./fixtures";

const prompt: TerminalScreen = {
  rows: 8,
  cols: 44,
  cursor: null,
  lines: [
    [{ t: "Which auth provider should the login use?", b: true }],
    [],
    [{ t: "❯ 1. Clerk", fg: "#d8a657" }],
    [{ t: "  2. Supabase" }],
    [{ t: "  3. Type something." }],
    [],
    [{ t: "Enter to select · ↑/↓ to navigate", fg: "#a89984" }],
    [{ t: "error", fg: "#1d2021", bg: "#ea6962" }],
  ],
};

const answered: TerminalScreen = {
  ...prompt,
  lines: [[{ t: "● Using Supabase for the login." }], ...prompt.lines.slice(1).map(() => [])],
};

/** Answers the terminal stream with `screen()` on each connection. */
async function terminal(page: Page, screen: () => TerminalScreen) {
  await page.route("**/api/trees/17/terminal**", (route) =>
    route.fulfill({
      headers: { "Content-Type": "text/event-stream" },
      body: `retry: 200\n\nevent: screen\ndata: ${JSON.stringify(screen())}\n\n`,
    }),
  );
}

test("a question with options is answered with keys on the agent's screen", async ({ page }) => {
  await serve(page, () => pool);
  const pressed: { keys: string[]; text?: string }[] = [];
  // The first press is held, as a slow server would, to show the next ones
  // wait for it rather than racing it to the agent.
  let release = () => {};
  const held = new Promise<void>((resolve) => (release = resolve));
  await page.route("**/api/trees/17/keys**", async (route) => {
    pressed.push(route.request().postDataJSON() as { keys: string[]; text?: string });
    if (pressed.length === 1) await held;
    await route.fulfill({ json: {} });
  });
  await terminal(page, () => (pressed.some((p) => p.keys.includes("Enter")) ? answered : prompt));
  await page.route("**/api/trees/17/chat**", (route) =>
    route.fulfill({ headers: { "Content-Type": "text/event-stream" }, body: "event: backlog\ndata: []\n\n" }),
  );
  await page.goto("/trees/17/chat");
  await page.getByRole("status").getByRole("link", { name: "Answer" }).click();
  await expect(page).toHaveURL(/\/trees\/17\/terminal$/);

  const screen = page.getByRole("main", { name: "Terminal" });
  await expect(screen).toContainText("❯ 1. Clerk");
  await expect(screen.getByText("❯ 1. Clerk")).toHaveCSS("color", "rgb(216, 166, 87)");
  await expect(screen.getByText("error")).toHaveCSS("background-color", "rgb(234, 105, 98)");
  await page.screenshot({ path: "test-results/terminal.png" });

  await page.getByRole("button", { name: "Down" }).click();
  await page.getByRole("button", { name: "2" }).click();
  await page.getByRole("button", { name: "Enter" }).click();
  // Long enough for an unqueued press to reach the server; a queued one never does.
  await page.waitForTimeout(300);
  expect(pressed.map((p) => p.keys)).toEqual([["Down"]]);
  release();
  await expect(screen).toContainText("Using Supabase for the login.");
  expect(pressed.map((p) => p.keys)).toEqual([["Down"], ["2"], ["Enter"]]);

  await page.getByRole("textbox", { name: "Type into the terminal" }).fill("use the EU region");
  await page.getByRole("button", { name: "Type" }).click();
  await expect.poll(() => pressed.at(-1)).toEqual({ keys: [], text: "use the EU region" });
  await page.screenshot({ path: "test-results/terminal-answered.png" });

  await page.getByRole("button", { name: "Back", exact: true }).click();
  await expect(page).toHaveURL(/\/trees\/17\/chat$/);
});

test("the tree's screen offers the terminal when its agent waits on you", async ({ page }) => {
  await serve(page, () => pool);
  await terminal(page, () => prompt);
  await page.goto("/trees/17");
  await expect(page.getByRole("region", { name: "Agent" })).toContainText(tree17.report!.says);
  await page.getByRole("link", { name: "Answer" }).click();
  await expect(page.getByRole("main", { name: "Terminal" })).toContainText("2. Supabase");
});

test("a key the server turns down says why", async ({ page }) => {
  await serve(page, () => pool);
  await terminal(page, () => prompt);
  await page.route("**/api/trees/17/keys**", (route) =>
    route.fulfill({ status: 409, json: { error: "the agent's terminal is not open" } }),
  );
  await page.goto("/trees/17/terminal");
  await expect(page.getByRole("main", { name: "Terminal" })).toContainText("2. Supabase");
  await page.getByRole("button", { name: "Enter" }).click();
  await expect(page.getByText("the agent's terminal is not open")).toBeVisible();
});
