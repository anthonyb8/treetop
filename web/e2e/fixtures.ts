import type { Page } from "@playwright/test";
import type { Card, Pool, Tree } from "../src/api";

export function card(tree: string, fields: Partial<Card> = {}): Card {
  return {
    tree,
    title: `END-${tree} work`,
    branch: `me/END-${tree}-work`,
    ticket: `END-${tree}`,
    mark: "busy",
    agent: "claude",
    more: 0,
    says: null,
    needs_you: false,
    git: { unpushed: 0, staged: 0, unstaged: 0, untracked: 0 },
    ports: [],
    pr: null,
    active_ago: 120,
    ...fields,
  };
}

/** A pool with a tree waiting on you, two working, one parked, three free. */
export const pool: Pool = {
  workspace: "app",
  is_stale: false,
  error: null,
  needs_you: [
    card("17", {
      title: "END-123 fix-login",
      mark: "waiting",
      needs_you: true,
      says: "Which auth provider should the login use, Clerk or Supabase?",
      git: { unpushed: 2, staged: 0, unstaged: 3, untracked: 0 },
      ports: [5173],
      pr: { label: "#41 ✗ ci", stage: "ci_failed", url: "https://github.com/me/app/pull/41" },
      active_ago: 720,
    }),
  ],
  groups: [
    {
      base: "main",
      checkout: { ports: [7777], git: { unpushed: 0, staged: 0, unstaged: 0, untracked: 0 } },
      cards: [
        card("14", { title: "END-140 rate-limits", says: "running the test suite", ports: [8000] }),
        card("12", { title: "END-131 copy-tweak", mark: "idle", pr: { label: "merged", stage: "merged", url: null } }),
      ],
    },
  ],
  free: 3,
  parked: [card("6", { title: "END-152 invoices", mark: "cold", active_ago: 172800 })],
};

/** Tree 17 as its screen sees it: blocked on a question, its PR failing. */
export const tree17: Tree = {
  card: pool.needs_you[0]!,
  path: "/home/me/.treehouse/app/17/app",
  holder: "me",
  base: "main",
  task: "Fix the login redirect loop on expired sessions.",
  agents: [{ name: "login", status: "waiting", has_chat: true, active_ago: 720 }],
  ports: [{ port: 5173, process: "vite", pid: 4242 }],
  pr: {
    label: "#41 ✗ ci",
    stage: "ci_failed",
    url: "https://github.com/me/app/pull/41",
    detail: ["test: 2 of 3 passed"],
  },
  pr_unknown: null,
  report: {
    state: "blocked",
    says: "Which auth provider should the login use, Clerk or Supabase?",
    steps: [
      { at: 1793500860, state: "working", detail: "read the login flow" },
      { at: 1793502120, state: "working", detail: "rewrote the redirect guard" },
      { at: 1793503200, state: "blocked", detail: "asked about the auth provider" },
    ],
  },
};

/** Answers the app's API calls with `answer` for the pool. */
export async function serve(page: Page, answer: () => Pool | "down") {
  await page.route("**/api/workspaces", (route) =>
    route.fulfill({ json: { default: "app", workspaces: ["app", "treetop"] } }),
  );
  await page.route("**/api/pool**", (route) => {
    const body = answer();
    return body === "down" ? route.abort("connectionrefused") : route.fulfill({ json: body });
  });
  await page.route("**/api/trees/**", (route) => {
    const name = decodeURIComponent(new URL(route.request().url()).pathname.split("/").pop() ?? "");
    return name === "17"
      ? route.fulfill({ json: tree17 })
      : route.fulfill({ status: 404, json: { error: `no tree ${name} in app` } });
  });
}
