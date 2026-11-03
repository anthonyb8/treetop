// ANB-21. An agent waiting on a question or a permission is answered in its
// chat on the phone: a question's options as buttons, a command with Allow
// and Deny, or your own words; there is no terminal to read.
import { expect, test, type Page } from "@playwright/test";
import type { Answer, ChatMessage } from "../src/api";
import { pool, serve } from "./fixtures";

const asked: ChatMessage = {
  id: "q1",
  kind: "tool",
  tool: {
    verb: "Asked you",
    what: "Which auth provider should the login use?",
    added: null,
    removed: null,
    status: "running",
    output: null,
    ask: [
      {
        question: "Which auth provider should the login use?",
        header: "Auth",
        multi: false,
        options: [
          { label: "Clerk", description: "Keep the current SDK." },
          { label: "Supabase", description: "Swap the session check." },
        ],
      },
      {
        question: "Which sign-in methods?",
        header: "Methods",
        multi: true,
        options: [
          { label: "Email", description: "" },
          { label: "Google", description: "" },
          { label: "GitHub", description: "" },
        ],
      },
    ],
  },
};

const command: ChatMessage = {
  id: "p1",
  kind: "tool",
  tool: {
    verb: "Ran",
    what: "Run the tests",
    added: null,
    removed: null,
    status: "running",
    output: null,
    ask: null,
  },
};

/** Opens tree 17's chat with `waiting` as its newest message; answers are kept in `given`. */
async function chat(page: Page, waiting: ChatMessage, refuse?: string) {
  await serve(page, () => pool);
  await page.route("**/api/trees/17/chat**", (route) =>
    route.fulfill({
      headers: { "Content-Type": "text/event-stream" },
      body: `retry: 60000\n\nevent: backlog\ndata: ${JSON.stringify([
        { id: "u1", kind: "you", text: "Fix the login loop." },
        waiting,
      ])}\n\n`,
    }),
  );
  const given: Answer[] = [];
  await page.route("**/api/trees/17/answer**", (route) => {
    given.push(route.request().postDataJSON() as Answer);
    return refuse
      ? route.fulfill({ status: 409, json: { error: refuse } })
      : route.fulfill({ json: {} });
  });
  await page.goto("/trees/17/chat");
  return given;
}

test("a question's options are answered with a tap", async ({ page }) => {
  const given = await chat(page, asked);
  const prompt = page.getByRole("region", { name: "Waiting on you" });
  await expect(prompt).toContainText("Which auth provider should the login use?");
  const answer = prompt.getByRole("button", { name: "Answer", exact: true });
  await expect(answer).toBeDisabled();
  await prompt.getByRole("button", { name: /Supabase/ }).click();
  await prompt.getByRole("button", { name: /Email/ }).click();
  await prompt.getByRole("button", { name: /GitHub/ }).click();
  await expect(prompt.getByRole("button", { name: /Supabase/ })).toHaveAttribute("aria-pressed", "true");
  await expect(answer).toBeInViewport();
  await page.screenshot({ path: "test-results/prompt-question.png" });
  await answer.click();
  await expect.poll(() => given).toEqual([{ action: "say", text: "Auth: Supabase\nMethods: Email, GitHub" }]);
  await expect(page.getByRole("region", { name: "Waiting on you" })).toHaveCount(0);
});

for (const [button, action] of [
  ["Allow", "allow"],
  ["Deny", "deny"],
] as const) {
  test(`a command waiting on permission is answered with ${button}`, async ({ page }) => {
    const given = await chat(page, command);
    const prompt = page.getByRole("region", { name: "Waiting on you" });
    await expect(prompt).toContainText("Wants to: Run the tests");
    await page.screenshot({ path: `test-results/prompt-permission-${action}.png` });
    await prompt.getByRole("button", { name: button }).click();
    await expect.poll(() => given).toEqual([{ action }]);
    await expect(page.getByRole("region", { name: "Waiting on you" })).toHaveCount(0);
  });
}

test("a command is answered in your own words", async ({ page }) => {
  const given = await chat(page, command);
  await expect(page.getByRole("textbox", { name: "Message 17" })).toHaveAttribute(
    "placeholder",
    "Answer in your own words…",
  );
  await page.getByRole("textbox", { name: "Message 17" }).fill("Run only the login tests.");
  await page.getByRole("button", { name: "Send" }).click();
  await expect.poll(() => given).toEqual([{ action: "say", text: "Run only the login tests." }]);
  await expect(page.getByText("Run only the login tests.")).toBeVisible();
});

// ANB-30. Claude Code writes a question to the transcript only once it is
// answered, so while the agent waits the chat has nothing but the agent's
// "needs you" words to go on.
const ran: ChatMessage = {
  id: "r1",
  kind: "tool",
  tool: { verb: "Ran", what: "Run the tests", added: null, removed: null, status: "ok", output: "ok", ask: null },
};

test("a question not yet in the transcript is read off the agent's screen and answered with a tap", async ({ page }) => {
  const given = await chat(page, ran);
  await page.route("**/api/trees/17/prompt**", (route) =>
    route.fulfill({ json: { ask: [asked.tool!.ask![0]] } }),
  );
  const prompt = page.getByRole("region", { name: "Waiting on you" });
  await expect(prompt).toContainText("Which auth provider should the login use?");
  await prompt.getByRole("button", { name: /Supabase/ }).click();
  await page.screenshot({ path: "test-results/prompt-from-screen.png" });
  await prompt.getByRole("button", { name: "Answer", exact: true }).click();
  await expect.poll(() => given).toEqual([{ action: "say", text: "Supabase" }]);
  await expect(page.getByRole("region", { name: "Waiting on you" })).toHaveCount(0);
});

test("a reply while the agent waits on you answers it, though no question was found", async ({ page }) => {
  const given = await chat(page, ran);
  const sent: string[] = [];
  await page.route("**/api/trees/17/send**", (route) => {
    sent.push(route.request().postData() ?? "");
    return route.fulfill({ json: {} });
  });
  await expect(page.getByRole("region", { name: "Waiting on you" })).toContainText("Clerk or Supabase?");
  await page.getByRole("textbox", { name: "Message 17" }).fill("Supabase, and keep Clerk's tables.");
  await page.getByRole("button", { name: "Send" }).click();
  await expect.poll(() => given).toEqual([{ action: "say", text: "Supabase, and keep Clerk's tables." }]);
  expect(sent).toEqual([]);
});

test("an answer the agent's screen would not take says why and keeps the prompt", async ({ page }) => {
  await chat(page, command, "nothing is waiting on an answer: the agent's input box is open");
  await page.getByRole("region", { name: "Waiting on you" }).getByRole("button", { name: "Deny" }).click();
  await expect(page.getByRole("alert")).toHaveText("nothing is waiting on an answer: the agent's input box is open");
  await expect(page.getByRole("region", { name: "Waiting on you" })).toBeVisible();
});
