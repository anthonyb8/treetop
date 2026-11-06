import type { Page } from "@playwright/test";
import type { Card, Inbox, IssueDetail, IssueRow, IssueSources, IssueTeam, IssueView, Pool, Tree } from "../src/api";

/** What `shot` takes besides the file it writes. */
type ShotOptions = Omit<NonNullable<Parameters<Page["screenshot"]>[0]>, "path">;

/**
 * A screenshot for a person to look at afterwards, not an assertion: the
 * suite writes one at each state worth reviewing. Capturing one can fail on
 * its own -- Chromium answers `Unable to capture screenshot` on a loaded CI
 * runner -- and a picture nobody has looked at yet must never fail a run, or
 * the release that run gates, so a capture that fails says so and is passed
 * over.
 */
export async function shot(page: Page, path: string, options: ShotOptions = {}) {
  try {
    await page.screenshot({ path, ...options });
  } catch (error) {
    console.warn(`could not capture ${path}: ${error}`);
  }
}

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
    parked: false,
    backburner: false,
    git: { unpushed: 0, staged: 0, unstaged: 0, untracked: 0 },
    ports: [],
    pr: null,
    active_ago: 120,
    run: null,
    ...fields,
  };
}

/** A pool with a tree waiting on you, two working, one on the backburner, one parked, three free. */
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
  backburner: [card("9", { title: "END-147 exports", mark: "idle", backburner: true, active_ago: 7200 })],
  free: 3,
  bases: ["main", "feature/billing"],
  parked: [card("6", { title: "END-152 invoices", mark: "cold", parked: true, active_ago: 172800 })],
};

/** Tree 17 as its screen sees it: blocked on a question, its PR failing. */
export const tree17: Tree = {
  card: pool.needs_you[0]!,
  path: "/home/me/.treehouse/app/17/app",
  holder: "me",
  base: "main",
  task: "Fix the login redirect loop on expired sessions.",
  agents: [{ name: "login", status: "waiting", has_chat: true, active_ago: 720, label: null }],
  ports: [{ port: 5173, process: "vite", pid: 4242, public: null }],
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

/** Answers the app's API calls with `answer` for the pool, and an empty
 * inbox, which a test can answer otherwise by routing it after. */
export async function serve(page: Page, answer: () => Pool | "down") {
  await page.route("**/api/inbox**", (route) => route.fulfill({ json: { entries: [], unseen: 0 } satisfies Inbox }));
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

/** An issue as a list row, on nobody's tree. */
export function row(id: string, fields: Partial<IssueRow> = {}): IssueRow {
  return {
    id,
    title: `${id} title`,
    state: "Todo",
    state_id: "s-todo",
    priority: 0,
    labels: [],
    label_ids: [],
    assignee: "Sam",
    assignee_id: "me",
    project: null,
    project_id: null,
    cycle: null,
    url: `https://linear.app/x/issue/${id}`,
    card: null,
    is_ready: false,
    ...fields,
  };
}

export const team: IssueTeam = {
  states: [
    { id: "s-todo", name: "Todo", is_closed: false },
    { id: "s-doing", name: "In Progress", is_closed: false },
    { id: "s-review", name: "In Review", is_closed: false },
    { id: "s-done", name: "Done", is_closed: true },
  ],
  labels: [
    { id: "l-bug", name: "Bug" },
    { id: "l-auth", name: "Auth" },
    { id: "l-billing", name: "Billing" },
  ],
  members: [
    { id: "me", name: "Sam" },
    { id: "mia", name: "Mia" },
  ],
  projects: [{ id: "p-login", name: "Login" }],
  cycles: [{ number: 14, name: null, is_active: true }],
  me: { id: "me", name: "Sam" },
};

const views: IssueView[] = [
  { name: "mine", line: "mine", group: "status" },
  { name: "all", line: "", group: "status" },
  { name: "by project", line: "", group: "project" },
  { name: "this cycle", line: "cycle:current", group: "none" },
  { name: "bugs", line: "label:Bug", group: "none" },
];

export const waiting = row("END-123", {
  title: "Fix the login redirect loop on expired sessions",
  state: "In Progress",
  state_id: "s-doing",
  priority: 1,
  labels: ["Bug", "Auth"],
  label_ids: ["l-bug", "l-auth"],
  project: "Login",
  project_id: "p-login",
  cycle: "Cycle 14",
  card: pool.needs_you[0]!,
});

export const ready = row("END-141", {
  title: "Show the invoice PDF on the billing page",
  priority: 2,
  labels: ["Billing"],
  label_ids: ["l-billing"],
  is_ready: true,
});

export const closed = row("END-127", {
  title: "Store invoices in object storage",
  state: "Done",
  state_id: "s-done",
});

/** Someone else's, so only a view of everyone's shows it. */
export const theirs = row("END-150", {
  title: "Rotate the signing keys",
  assignee: "Mia",
  assignee_id: "mia",
});

export const details: Record<string, IssueDetail> = {
  "END-123": {
    issue: waiting,
    description: "## What happens\n\nThe guard sends it **straight back**.\n\n- Refresh first\n- Land on login once",
    comments: [{ author: "Mia", body: "Seen on Safari too.", created_at: "2026-10-30T10:00:00.000Z" }],
    links: [],
  },
  "END-141": {
    issue: ready,
    description: "A preview of the latest invoice.",
    comments: [],
    links: [{ kind: "blocked by", id: "END-127", title: closed.title, state: "Done", is_done: true }],
  },
};

/** The Issues API as a test can see into: every change it was asked for, and
 *  what it says next. */
export interface IssuesApi {
  posts: { path: string; source: string | null; body: unknown }[];
  /** What refuses a call, by the path it ends in. */
  refuse: (path: string) => string | null;
  /** What a change waits on before it is answered, as Linear takes its time. */
  hold: Promise<void> | null;
  isDown: boolean;
  sources: IssueSources;
  readOnly: boolean;
}

/** Answers the Issues API from the rows above, narrowing as the server does
 *  by the words of the line, and `personal` with a list of its own. What a
 *  change leaves an issue as is what it reads as from then on. */
export async function serveIssues(page: Page): Promise<IssuesApi> {
  const api: IssuesApi = {
    posts: [],
    refuse: () => null,
    hold: null,
    isDown: false,
    sources: { sources: ["personal", "work"], own: "work", why: null, views },
    readOnly: false,
  };
  const changed = new Map<string, IssueRow>();
  const current = (issue: IssueRow) => changed.get(issue.id) ?? issue;
  await page.route("**/api/issues**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname.replace("/api/issues", "");
    const source = url.searchParams.get("source");
    if (route.request().method() === "POST") {
      const body = route.request().postDataJSON() as Record<string, unknown>;
      api.posts.push({ path, source, body });
      await api.hold;
      const why = api.refuse(path);
      if (why) return route.fulfill({ status: 502, json: { error: why } });
      if (path.endsWith("/pick")) return route.fulfill({ json: { tree: "17" } });
      if (path.endsWith("/comment")) {
        return route.fulfill({ json: { author: "Sam", body: body.body, created_at: new Date().toISOString() } });
      }
      if (path === "") {
        const made = row("END-161", { title: String(body.title) });
        changed.set(made.id, made);
        return route.fulfill({ json: made });
      }
      const id = decodeURIComponent(path.slice(1));
      const was = current(details[id]!.issue);
      const named = (list: { id: string; name: string }[], key: unknown) => list.find((n) => n.id === key)?.name ?? null;
      const now: IssueRow = {
        ...was,
        ...body,
        state: named(team.states, body.state_id) ?? was.state,
        assignee: "assignee_id" in body ? named(team.members, body.assignee_id) : was.assignee,
        labels: Array.isArray(body.label_ids) ? body.label_ids.flatMap((l) => named(team.labels, l) ?? []) : was.labels,
      };
      changed.set(id, now);
      return route.fulfill({ json: now });
    }
    if (path === "/sources") return route.fulfill({ json: api.sources });
    if (path === "/team") return route.fulfill({ json: { team: api.readOnly ? null : team } });
    if (path !== "") {
      const id = decodeURIComponent(path.slice(1));
      const detail = details[id] ?? { issue: row(id), description: null, comments: [], links: [] };
      return route.fulfill({ json: { ...detail, issue: current(detail.issue) } });
    }
    if (api.isDown) return route.fulfill({ status: 502, json: { error: "Linear did not answer" } });
    const line = url.searchParams.get("line") ?? "";
    const group = url.searchParams.get("group");
    let issues = source === "personal" ? [row("PER-1", { title: "Book the dentist" })] : [waiting, ready, theirs, closed].map(current);
    if (!line.includes("state:any") && !line.includes("state:Done")) {
      issues = issues.filter((issue) => issue.state !== "Done");
    }
    if (line.split(" ").includes("mine")) issues = issues.filter((issue) => issue.assignee_id === team.me?.id);
    if (line.includes("cycle:current")) issues = issues.filter((issue) => issue.cycle);
    const state = /state:"?(Done|In Review)/.exec(line)?.[1];
    if (state) issues = issues.filter((issue) => issue.state === state);
    const words = line.split(" ").filter((word) => word && !word.includes(":") && word !== "mine");
    issues = issues.filter((issue) => words.every((word) => issue.title.toLowerCase().includes(word)));
    const label = /label:(\S+)/.exec(line)?.[1];
    if (label) issues = issues.filter((issue) => issue.labels.includes(label));
    const heading = (issue: IssueRow) =>
      group === "status" ? issue.state : group === "project" ? (issue.project ?? "No project") : "";
    const sections = [...new Set(issues.map(heading))].map((name) => ({
      heading: name,
      is_closed: name === "Done",
      issues: issues.filter((issue) => heading(issue) === name),
    }));
    return route.fulfill({ json: { sections } });
  });
  return api;
}

/** Drags a finger across `selector` from its right edge, `by` pixels left. */
export async function swipe(page: Page, selector: string, by: number, lift = true) {
  await page.locator(selector).first().scrollIntoViewIfNeeded();
  const box = (await page.locator(selector).first().boundingBox())!;
  const y = box.y + box.height / 2;
  const x = box.x + box.width - 20;
  const cdp = await page.context().newCDPSession(page);
  const touch = (type: string, at: number) =>
    cdp.send("Input.dispatchTouchEvent", {
      type: type as "touchStart",
      touchPoints: type === "touchEnd" ? [] : [{ x: at, y }],
    });
  await touch("touchStart", x);
  for (let dx = 10; dx <= by; dx += 10) await touch("touchMove", x - dx);
  if (lift) await touch("touchEnd", x - by);
}
