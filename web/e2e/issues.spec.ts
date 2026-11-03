// ANB-27, ANB-28, ANB-29. The Issues screen on the phone: what work there is
// and who is on it, an issue in full, changing one with what you see updating
// at once, and putting an agent on one. The API is the fake in fixtures.ts;
// `src/serve/issues.rs` owns what the server really answers.
import { expect, test, type Page } from "@playwright/test";
import { pool, serve, serveIssues } from "./fixtures";

/** A picture with everything moving at rest. */
async function shot(page: Page, name: string) {
  await page.screenshot({ path: `test-results/${name}.png`, animations: "disabled" });
}

async function openIssues(page: Page) {
  await serve(page, () => pool);
  const api = await serveIssues(page);
  await page.goto("/");
  return api;
}

test("issues are a tab beside the trees, each row saying who is on it", async ({ page }) => {
  await openIssues(page);
  await expect(page.getByRole("link", { name: "New agent" })).toBeVisible();
  await expect(page.getByLabel("1 waiting on you")).toBeVisible();
  await shot(page, "issues-trees");

  await page.getByRole("link", { name: "Issues", exact: true }).click();
  await expect(page).toHaveURL(/\/issues$/);
  await expect(page.getByRole("link", { name: "New issue" })).toBeVisible();
  const doing = page.getByRole("region", { name: "In Progress" });
  const row = doing.getByRole("link", { name: /END-123/ });
  await expect(row).toContainText("17 · waiting");
  await expect(row).not.toContainText("In Progress");
  const todo = page.getByRole("region", { name: "Todo" });
  await expect(todo.getByRole("link", { name: /END-141/ })).toContainText("ready");
  await expect(page.getByRole("link", { name: /END-150/ })).toHaveCount(0);
  await shot(page, "issues-mine");

  await page.getByRole("button", { name: "all", exact: true }).click();
  await expect(todo.getByRole("link", { name: /END-150/ })).toContainText("Mia");

  // Status joins the row once the list is not grouped by it.
  await page.getByRole("button", { name: "by project" }).click();
  const login = page.getByRole("region", { name: "Login" });
  await expect(login.getByRole("link", { name: /END-123/ })).toContainText("In Progress");
  await shot(page, "issues-by-project");

  await page.getByRole("button", { name: "this cycle" }).click();
  await expect(page.getByRole("link", { name: /END-123/ })).toContainText("In Progress");
  await expect(page.getByRole("link", { name: /END-141/ })).toHaveCount(0);

  // A view from the config is a chip like the others.
  await page.getByRole("button", { name: "bugs" }).click();
  await expect(page.getByRole("link", { name: /END-123/ })).toBeVisible();
  await expect(page.getByRole("link", { name: /END-141/ })).toHaveCount(0);
});

test("search reaches closed issues too, and a closed group opens by hand", async ({ page }) => {
  await openIssues(page);
  await page.goto("/issues");
  await page.getByRole("button", { name: "Search issues" }).click();
  await page.getByLabel("Search issues by id or title").fill("invoice");
  await expect(page.getByRole("link", { name: /END-141/ })).toBeVisible();
  await expect(page.getByRole("link", { name: /END-127/ })).toContainText("Done");
  await expect(page.getByRole("link", { name: /END-123/ })).toHaveCount(0);
  await shot(page, "issues-search");
  await page.getByLabel("Search issues by id or title").fill("zebra");
  await expect(page.getByText("No issues match.")).toBeVisible();
  await page.getByRole("button", { name: "Cancel" }).click();

  await page.getByRole("button", { name: "Filters" }).click();
  await page.getByRole("dialog").getByRole("button", { name: /^Status/ }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Any, closed too" }).click();
  await shot(page, "issues-filters");
  await page.getByRole("dialog").getByRole("button", { name: /^Show/ }).click();
  await expect(page.getByText("state:any")).toBeVisible();
  const done = page.getByRole("region", { name: "Done" });
  await expect(done.getByRole("link")).toHaveCount(0);
  await done.getByRole("button", { name: /Done/ }).click();
  await expect(done.getByRole("link", { name: /END-127/ })).toBeVisible();
  await page.getByRole("banner").getByRole("button", { name: "Clear" }).click();
  await expect(page.getByText("state:any")).toHaveCount(0);
});

test("a filter that leaves nothing says so and clears", async ({ page }) => {
  await openIssues(page);
  await page.goto("/issues");
  await expect(page.getByRole("link", { name: /END-123/ })).toBeVisible();
  const sheet = page.getByRole("dialog");
  await page.getByRole("button", { name: "Filters" }).click();
  await sheet.getByRole("button", { name: /^Label/ }).click();
  await sheet.getByRole("button", { name: "Billing" }).click();
  await sheet.getByRole("button", { name: /^Status/ }).click();
  await sheet.getByRole("button", { name: "In Review" }).click();
  await shot(page, "issues-filters-set");
  await sheet.getByRole("button", { name: /^Show/ }).click();
  await expect(page.getByText("No issues match.")).toBeVisible();
  await expect(page.getByText("state:\"In Review\" label:Billing")).toBeVisible();
  await shot(page, "issues-no-match");
  await page.getByRole("button", { name: "Clear filters" }).click();
  await expect(page.getByRole("link", { name: /END-123/ })).toBeVisible();
});

test("another source shows its own issues, and a repository with none says what to add", async ({ page }) => {
  const api = await openIssues(page);
  await page.goto("/issues");
  await page.getByLabel("Issue source").selectOption("personal");
  await expect(page.getByRole("link", { name: /PER-1/ })).toBeVisible();
  await expect(page.getByRole("link", { name: /END-123/ })).toHaveCount(0);

  await page.evaluate(() => localStorage.clear());
  api.sources = { ...api.sources, own: null, why: 'app has no issue source: add [repos."app"] with issues = "work"' };
  await page.reload();
  await expect(page.getByText("app has no issue source")).toBeVisible();
  await shot(page, "issues-no-source");
  await page.getByLabel("Issue source").selectOption("work");
  await expect(page.getByRole("link", { name: /END-123/ })).toBeVisible();
});

test("with Linear out of reach the last list stays and says so", async ({ page }) => {
  const api = await openIssues(page);
  await page.goto("/issues");
  await expect(page.getByRole("link", { name: /END-123/ })).toBeVisible();
  api.isDown = true;
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Linear did not answer - showing the list as it last was.");
  await expect(page.getByRole("link", { name: /END-123/ })).toBeVisible();
  await shot(page, "issues-down");
});

test("an issue reads in full, with its agent's question", async ({ page }) => {
  await openIssues(page);
  await page.goto("/issues");
  await page.getByRole("link", { name: /END-123/ }).click();
  await expect(page).toHaveURL(/\/issues\/END-123$/);
  await expect(page.getByRole("heading", { name: /Fix the login redirect/ })).toBeVisible();
  const agent = page.getByRole("region", { name: "Agent" });
  await expect(agent).toContainText("Tree 17");
  await expect(agent).toContainText("Which auth provider should the login use, Clerk or Supabase?");
  const text = page.getByRole("region", { name: "Description" });
  await expect(text.locator("strong")).toHaveText("straight back");
  await expect(text.locator("li")).toHaveCount(2);
  await expect(page.getByRole("region", { name: "Comments" })).toContainText("Seen on Safari too.");
  await shot(page, "issue-waiting");

  await page.getByRole("link", { name: "Open tree 17" }).click();
  await expect(page).toHaveURL(/\/trees\/17$/);
  await page.getByRole("button", { name: "Trees" }).click();
  await expect(page).toHaveURL(/\/issues\/END-123$/);
});

test("a ready issue says why, and picking it up lands on its tree", async ({ page }) => {
  const api = await openIssues(page);
  await page.goto("/issues/END-141");
  await expect(page.getByRole("region", { name: "Ready" })).toHaveText("Ready. END-127 is done, so nothing blocks this.");
  await expect(page.getByRole("region", { name: "Links" })).toContainText("blocked by");
  await shot(page, "issue-ready");

  // A refusal stays on the sheet and says why.
  api.refuse = (path) => (path.endsWith("/pick") ? "no free tree" : null);
  await page.getByRole("button", { name: "Pick up" }).click();
  await page.getByLabel("Base").selectOption("feature/billing");
  await shot(page, "issue-pick-up");
  await page.getByRole("dialog").getByRole("button", { name: "Pick up" }).click();
  await expect(page.getByRole("alert")).toHaveText("no free tree");

  api.refuse = () => null;
  await page.getByRole("dialog").getByRole("button", { name: "Pick up" }).click();
  await expect(page).toHaveURL(/\/trees\/17$/);
  expect(api.posts.at(-1)).toEqual({ path: "/END-141/pick", source: "work", body: { base: "feature/billing" } });
});

test("an issue from the other source is picked up in that source", async ({ page }) => {
  const api = await openIssues(page);
  await page.goto("/issues");
  await page.getByLabel("Issue source").selectOption("personal");
  await page.getByRole("link", { name: /PER-1/ }).click();
  await page.getByRole("button", { name: "Pick up" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Pick up" }).click();
  await expect(page).toHaveURL(/\/trees\/17$/);
  expect(api.posts.at(-1)?.source).toBe("personal");
});

test("status, assignee and labels change, say what they sent, and the list shows it", async ({ page }) => {
  const api = await openIssues(page);
  await page.goto("/issues");
  await page.getByRole("region", { name: "Todo" }).getByRole("link", { name: /END-141/ }).click();

  await page.getByRole("button", { name: "Status: Todo" }).click();
  await shot(page, "issue-status");
  await page.getByRole("dialog").getByRole("button", { name: "In Review" }).click();
  await expect(page.getByRole("button", { name: "Status: In Review" })).toBeVisible();
  await expect.poll(() => api.posts.at(-1)).toEqual({ path: "/END-141", source: "work", body: { state_id: "s-review" } });

  await page.getByRole("button", { name: "Issues", exact: true }).click();
  await page.getByRole("region", { name: "In Review" }).getByRole("link", { name: /END-141/ }).click();

  await page.getByRole("button", { name: "Assignee: Sam" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Nobody" }).click();
  await expect(page.getByRole("button", { name: "Assignee: Nobody" })).toBeVisible();
  await expect.poll(() => api.posts.at(-1)?.body).toEqual({ assignee_id: null });

  await page.getByRole("button", { name: "Labels: Billing" }).click();
  const sheet = page.getByRole("dialog", { name: "Labels" });
  await sheet.getByRole("button", { name: "Bug" }).click();
  await sheet.getByRole("button", { name: "Billing" }).click();
  await sheet.getByRole("button", { name: "Auth" }).click();
  await shot(page, "issue-labels");
  await sheet.getByRole("button", { name: "Apply" }).click();
  await expect(page.getByRole("button", { name: "Labels: Bug, Auth" })).toBeVisible();
  await expect.poll(() => api.posts.at(-1)?.body).toEqual({ label_ids: ["l-bug", "l-auth"] });
});

test("a change shows at once, and goes back saying why if Linear refuses it", async ({ page }) => {
  const api = await openIssues(page);
  api.refuse = () => "Linear said no";
  let answer = () => {};
  api.hold = new Promise((resolve) => (answer = resolve));
  await page.goto("/issues/END-141");
  await page.getByRole("button", { name: "Status: Todo" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Done" }).click();
  await expect(page.getByRole("button", { name: "Status: Done" })).toBeVisible();
  answer();
  await expect(page.getByRole("alert")).toHaveText("The status did not change: Linear said no.");
  await expect(page.getByRole("button", { name: "Status: Todo" })).toBeVisible();
  await shot(page, "issue-refused");
});

test("a comment is added to the issue", async ({ page }) => {
  const api = await openIssues(page);
  await page.goto("/issues/END-141");
  await page.getByRole("button", { name: "Comment", exact: true }).click();
  await page.getByLabel("On END-141, in Markdown").fill("A plain preview it is.");
  await shot(page, "issue-comment");
  await page.getByRole("button", { name: "Send" }).click();
  await expect(page.getByRole("region", { name: "Comments" })).toContainText("A plain preview it is.");
  expect(api.posts.at(-1)).toEqual({ path: "/END-141/comment", source: "work", body: { body: "A plain preview it is." } });
});

test("a new issue is written in one form and opens once made", async ({ page }) => {
  const api = await openIssues(page);
  await page.goto("/issues");
  await page.getByRole("link", { name: "New issue" }).click();
  // The button also waits on the source, so only a title that takes it away
  // again shows that it is the title it wants.
  await page.getByLabel("Title").fill("Email a receipt when a payment fails");
  await expect(page.getByRole("button", { name: "Create issue" })).toBeEnabled();
  await page.getByLabel("Title").fill("  ");
  await expect(page.getByRole("button", { name: "Create issue" })).toBeDisabled();
  await page.getByLabel("Title").fill("Email a receipt when a payment fails");
  await page.getByRole("button", { name: /^Priority/ }).click();
  await page.getByRole("dialog").getByRole("button", { name: "High" }).click();
  await page.getByRole("button", { name: /^Labels/ }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Billing" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Done" }).click();
  await page.getByLabel("Description").fill("Customers only find out from the billing page.");
  await shot(page, "issue-new");
  await page.getByRole("button", { name: "Create issue" }).click();
  await expect(page).toHaveURL(/\/issues\/END-161$/);
  await expect(page.getByRole("heading", { name: "Email a receipt when a payment fails" })).toBeVisible();
  expect(api.posts.at(-1)).toEqual({
    path: "",
    source: "work",
    body: {
      title: "Email a receipt when a payment fails",
      description: "Customers only find out from the billing page.",
      state_id: null,
      assignee_id: null,
      label_ids: ["l-billing"],
      project_id: null,
      priority: 2,
    },
  });
});

test("editing sends only what changed", async ({ page }) => {
  const api = await openIssues(page);
  await page.goto("/issues/END-141");
  await page.getByRole("link", { name: "Edit the issue" }).click();
  await expect(page.getByLabel("Title")).toHaveValue("Show the invoice PDF on the billing page");
  await page.getByLabel("Title").fill("Show the invoice on the billing page");
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page).toHaveURL(/\/issues\/END-141$/);
  expect(api.posts.at(-1)?.body).toEqual({ title: "Show the invoice on the billing page" });
  await expect(page.getByRole("heading", { name: "Show the invoice on the billing page" })).toBeVisible();
});

test("a source that cannot change its issues offers only what it can do", async ({ page }) => {
  const api = await openIssues(page);
  api.readOnly = true;
  await page.goto("/issues");
  await expect(page.getByRole("link", { name: /END-123/ })).toBeVisible();
  await expect(page.getByRole("link", { name: "New issue" })).toHaveCount(0);
  // The controls show only once the source has said it can change issues, so
  // that they are absent means something only once it has answered.
  const answered = page.waitForResponse(/\/api\/issues\/team/);
  await page.getByRole("link", { name: /END-141/ }).click();
  await (await answered).finished();
  await page.evaluate(() => new Promise(requestAnimationFrame));
  await expect(page.getByRole("heading", { name: /Show the invoice/ })).toBeVisible();
  await expect(page.getByText("Todo").last()).toBeVisible();
  await expect(page.getByRole("button", { name: /^Status/ })).toHaveCount(0);
  await expect(page.getByRole("link", { name: "Edit the issue" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Comment", exact: true })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Pick up" })).toBeVisible();
  await shot(page, "issue-read-only");
});
