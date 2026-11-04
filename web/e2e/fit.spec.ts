// Every screen fits the phone's width, whatever a tree or an issue holds:
// a branch, a path or a URL has no spaces to wrap at, and a tree can serve
// many ports, but none of it may run off the side of the screen.
import { expect, test, type Page } from "@playwright/test";
import type { IssueDetail, Pool, Tree } from "../src/api";
import { card, details, pool, serve, serveIssues, shot, tree17, waiting } from "./fixtures";

const word = "test_billing_reconciliation_for_multi_currency_invoices";

const crowded: Pool = {
  ...pool,
  needs_you: [
    card("17", {
      ...pool.needs_you[0],
      says: `Should ${word} keep its own table?`,
      ports: [5173, 8000, 8001, 8002, 9000, 9001],
    }),
  ],
  groups: [
    {
      ...pool.groups[0]!,
      base: `feature/${word}`,
      cards: [card("14", { title: `END-140 ${word}`, ports: [8000, 8001, 8002, 8003] })],
    },
  ],
};

const tree: Tree = {
  ...tree17,
  card: crowded.needs_you[0]!,
  path: `/home/me/.treehouse/${word}/17/app`,
  ports: [{ port: 5173, process: `node /home/me/${word}/server.js`, pid: 4242, public: null }],
  pr: { ...tree17.pr!, detail: [`test/${word}.spec.ts: 2 of 3 passed`] },
  report: { ...tree17.report!, says: `Should ${word} keep its own table?` },
};

const issue: IssueDetail = {
  ...details["END-123"]!,
  issue: { ...waiting, title: word, project: word },
  comments: [{ author: word, body: `See https://example.com/${word}/${word}`, created_at: "2026-10-30T10:00:00.000Z" }],
};

/** What on screen reaches past its right edge, itself or the text in it,
 *  but for a code block, which scrolls on its own. */
function overflowing(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const width = document.documentElement.clientWidth;
    return Array.from(document.querySelectorAll<HTMLElement>("body *"))
      .filter((el) => {
        const box = el.getBoundingClientRect();
        if (box.width === 0 || el.closest("[inert], pre")) return false;
        // A box that clips what it holds, as a title cut short with "…" does, reaches no further than itself.
        const clips = getComputedStyle(el).overflowX !== "visible";
        return (clips ? box.right : Math.max(box.right, box.left + el.scrollWidth)) > width + 0.5;
      })
      .map((el) => `${el.tagName.toLowerCase()}.${el.className} "${(el.textContent ?? "").trim().slice(0, 40)}"`);
  });
}

test("long names and many ports stay inside the screen", async ({ page }) => {
  await serve(page, () => crowded);
  await page.route("**/api/trees/17?**", (route) => route.fulfill({ json: tree }));
  await page.route("**/api/trees/17/chat**", (route) =>
    route.fulfill({ status: 200, headers: { "Content-Type": "text/event-stream" }, body: ": quiet\n\n" }),
  );
  await serveIssues(page);
  // Answered here rather than by changing the shared fixture, which later
  // tests in the same worker read.
  await page.route("**/api/issues/END-123?**", (route) => route.fulfill({ json: issue }));

  for (const [path, landmark] of [
    ["/", page.getByRole("region", { name: "Needs you" })],
    ["/trees/17", page.getByRole("region", { name: "Tree" })],
    ["/trees/17/chat", page.getByRole("main", { name: "Conversation" })],
    ["/issues/END-123", page.getByRole("heading", { name: word })],
  ] as const) {
    await page.goto(path);
    await expect(landmark).toBeVisible();
    await shot(page, `test-results/fit${path.replaceAll("/", "-")}.png`);
    expect(await overflowing(page), path).toEqual([]);
  }
});
