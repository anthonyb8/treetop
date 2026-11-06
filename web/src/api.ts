// The shapes `treetop serve` answers with, as `src/serve/pool.rs` builds
// them. A test on the Rust side checks its output against the fields read
// here, so renaming one there fails the build rather than the phone.

export type Mark =
  | "waiting"
  | "busy"
  | "idle"
  | "cold"
  | "done"
  | "failed"
  | "unknown"
  | "no-agent"
  | "none";

export type Stage =
  | "gate"
  | "failed"
  | "draft"
  | "checks"
  | "ci_failed"
  | "changes"
  | "review"
  | "approved"
  | "merged"
  | "closed"
  | "idle"
  | "running";

export interface Git {
  unpushed: number;
  staged: number;
  unstaged: number;
  untracked: number;
}

export interface Pr {
  label: string;
  stage: Stage;
  url: string | null;
}

export interface Card {
  tree: string;
  title: string | null;
  branch: string | null;
  ticket: string | null;
  mark: Mark;
  agent: string | null;
  more: number;
  says: string | null;
  needs_you: boolean;
  /** Set aside under Parked. */
  parked: boolean;
  /** Worked on, but not now: under Backburner. */
  backburner: boolean;
  /** "unknown" where git could not read a held tree; null for a free one. */
  git: Git | "unknown" | null;
  ports: number[];
  pr: Pr | null;
  /** Seconds since its agents last worked. */
  active_ago: number | null;
  /** The keel run working in it, if any. */
  run: Run | null;
}

/** A keel run: a routed task, the playbook it follows and how far it is. */
export interface Run {
  playbook: string;
  /** The step being worked on out of the playbook's, as "2/6". */
  step: string;
  step_title: string | null;
  state: "active" | "paused" | "done" | "stopped" | "failed";
  goal: string;
  /** Decisions flagged for a look. */
  attention: number;
  /** Active, but no process was in the tree at keel's last sweep. */
  stale: boolean;
  /** keel keeps the tree until the run stops. */
  holds: boolean;
}

export interface Group {
  base: string;
  /** The main checkout, when it heads this base's group. */
  checkout: { ports: number[]; git: Git | "unknown" | null } | null;
  cards: Card[];
}

export interface Pool {
  workspace: string;
  is_stale: boolean;
  error: string | null;
  needs_you: Card[];
  groups: Group[];
  /** Held trees worked on, but not now, between the groups and the free trees. */
  backburner: Card[];
  free: number;
  parked: Card[];
  /** The bases trees are cut from, the pool's own first. */
  bases: string[];
}

export type ReportState = "blocked" | "working" | "done" | "stopped" | "failed";

export interface Step {
  /** Seconds since the epoch; null when the stamp could not be read. */
  at: number | null;
  state: ReportState | null;
  detail: string;
}

export interface Report {
  state: ReportState;
  says: string;
  steps: Step[];
}

export interface AgentRow {
  name: string;
  status: "busy" | "idle" | "waiting" | "cold" | null;
  has_chat: boolean;
  active_ago: number | null;
  /** What sets it apart, such as "keel" for a session under keel. */
  label: string | null;
}

export interface Port {
  port: number;
  process: string;
  pid: number;
  /** Its address on the internet, where it is public, as `P` puts it there. */
  public: string | null;
}

export interface TreePr extends Pr {
  detail: string[];
}

export interface Tree {
  card: Card;
  path: string;
  holder: string | null;
  /** The branch it was cut from, for a held tree. */
  base: string | null;
  /** What its agent was sent to do, when treetop started it. */
  task: string | null;
  agents: AgentRow[];
  ports: Port[];
  pr: TreePr | null;
  /** Why no plugin could say where its PR stands. */
  pr_unknown: string | null;
  report: Report | null;
}

export interface ToolCall {
  /** What it did, as "Ran", "Edited", "Read". */
  verb: string;
  /** What it did it to, in a few words: a file, a command's description. */
  what: string | null;
  /** Lines an edit or write added and took away. */
  added: number | null;
  removed: number | null;
  status: "running" | "ok" | "error";
  /** The start of what it printed, once it has finished. */
  output: string | null;
  /** For a question the agent asks you: what it asks and the choices. */
  ask: Question[] | null;
}

export interface Question {
  question: string;
  /** A word or two that names it, as a chip. */
  header: string;
  /** It takes several of the options rather than one. */
  multi: boolean;
  options: Choice[];
}

export interface Choice {
  label: string;
  description: string;
}

/** One entry of an agent's conversation, as `src/serve/chat.rs` reads it. */
export interface ChatMessage {
  id: string;
  kind: "you" | "claude" | "tool" | "result";
  at?: string;
  text?: string;
  tool?: ToolCall;
  /** A result's, which finishes the tool call of the same id. */
  status?: "ok" | "error";
  output?: string;
}

export interface Workspaces {
  default: string;
  workspaces: string[];
}

export type InboxKind = "needs_you" | "failed" | "died" | "done" | "merged";

/** One thing that wants you, as the desktop's inbox lists it. */
export interface InboxEntry {
  /** What opening and dismissing it are remembered by. */
  key: string;
  kind: InboxKind;
  /** The workspace it lives in, where that is not the one asked about. */
  workspace: string | null;
  tree: string;
  title: string | null;
  what: string;
  /** Seconds since it happened, where that is known. */
  ago: number | null;
  seen: boolean;
}

/** Every workspace's entries, unseen first, then seen. */
export interface Inbox {
  entries: InboxEntry[];
  unseen: number;
  /** The server has not finished reading the pools yet, so the entries
   *  may be missing some, or all. */
  is_reading: boolean;
}

export interface IssueRow {
  id: string;
  title: string;
  state: string;
  state_id: string | null;
  /** 0 none, 1 urgent, 2 high, 3 medium, 4 low. */
  priority: number;
  labels: string[];
  label_ids: string[];
  assignee: string | null;
  assignee_id: string | null;
  project: string | null;
  project_id: string | null;
  cycle: string | null;
  url: string | null;
  /** The tree working on it, as the pool shows that tree. */
  card: Card | null;
  /** Nobody works on it, and a merge has left nothing blocking it. */
  is_ready: boolean;
}

export interface IssueSection {
  /** Empty for a list with no groups. */
  heading: string;
  /** Its issues' work is over, so it comes folded. */
  is_closed: boolean;
  issues: IssueRow[];
}

export interface IssueList {
  sections: IssueSection[];
}

export interface IssueComment {
  author: string;
  body: string;
  created_at: string;
}

export interface IssueLink {
  /** As "blocked by", "blocks", "related", from this issue's side. */
  kind: string;
  id: string;
  title: string;
  state: string;
  is_done: boolean;
}

export interface IssueDetail {
  issue: IssueRow;
  description: string | null;
  comments: IssueComment[];
  links: IssueLink[];
}

export interface Named {
  id: string;
  name: string;
}

export interface IssueState extends Named {
  is_closed: boolean;
}

export interface IssueCycle {
  number: number;
  name: string | null;
  is_active: boolean;
}

/** What a source's issues can be set to. */
export interface IssueTeam {
  states: IssueState[];
  labels: Named[];
  members: Named[];
  projects: Named[];
  cycles: IssueCycle[];
  me: Named | null;
}

export type IssueGroup = "none" | "project" | "status";

export interface IssueView {
  name: string;
  /** Its filter line, as `line` of a list takes it. */
  line: string;
  group: IssueGroup;
}

export interface IssueSources {
  sources: string[];
  /** The repository's own, when it has one. */
  own: string | null;
  /** Why it has none. */
  why: string | null;
  views: IssueView[];
}

/** What can change on an issue; a field left out is left alone. */
export interface IssueChange {
  title?: string;
  description?: string;
  state_id?: string;
  assignee_id?: string | null;
  label_ids?: string[];
  project_id?: string | null;
  priority?: number;
}

export interface NewIssue {
  title: string;
  description: string | null;
  state_id: string | null;
  assignee_id: string | null;
  label_ids: string[];
  project_id: string | null;
  priority: number | null;
}

/** A call the server turned down, or one that never reached it. */
export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number | null,
  ) {
    super(message);
  }
}

async function get<T>(path: string): Promise<T> {
  let response: Response;
  try {
    response = await fetch(path, { cache: "no-store" });
  } catch {
    throw new ApiError("Can't reach the server", null);
  }
  if (!response.ok) {
    const body = (await response.json().catch(() => null)) as { error?: string } | null;
    throw new ApiError(body?.error ?? `The server said ${response.status}`, response.status);
  }
  return (await response.json()) as T;
}

export function fetchPool(workspace: string | null): Promise<Pool> {
  return get<Pool>(`/api/pool${workspaceQuery(workspace)}`);
}

function workspaceQuery(workspace: string | null): string {
  return workspace ? `?workspace=${encodeURIComponent(workspace)}` : "";
}

export function fetchTree(workspace: string | null, tree: string): Promise<Tree> {
  return get<Tree>(`/api/trees/${encodeURIComponent(tree)}${workspaceQuery(workspace)}`);
}

/** The question on the agent's screen, read there because Claude Code
 * writes it to the transcript only once it is answered. */
export function fetchPrompt(workspace: string | null, tree: string): Promise<{ ask: Question[] }> {
  return get<{ ask: Question[] }>(`/api/trees/${encodeURIComponent(tree)}/prompt${workspaceQuery(workspace)}`);
}

export function fetchWorkspaces(): Promise<Workspaces> {
  return get<Workspaces>("/api/workspaces");
}

export function fetchInbox(workspace: string | null): Promise<Inbox> {
  return get<Inbox>(`/api/inbox${workspaceQuery(workspace)}`);
}

/** Marks an inbox entry opened, or takes it out of the inbox. */
export function markInbox(key: string, as: "seen" | "dismiss"): Promise<void> {
  return post<unknown>(`/api/inbox/${as}`, { key }).then(() => undefined);
}

export function chatUrl(workspace: string | null, tree: string): string {
  return `/api/trees/${encodeURIComponent(tree)}/chat${workspaceQuery(workspace)}`;
}

/** Sends a line to the tree's agent, as `treetop send` does. */
export async function sendLine(workspace: string | null, tree: string, text: string): Promise<void> {
  let response: Response;
  try {
    response = await fetch(`/api/trees/${encodeURIComponent(tree)}/send${workspaceQuery(workspace)}`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ text }),
    });
  } catch {
    throw new ApiError("Can't reach the server", null);
  }
  if (!response.ok) {
    const body = (await response.json().catch(() => null)) as { error?: string } | null;
    throw new ApiError(body?.error ?? `The server said ${response.status}`, response.status);
  }
}

async function post<T>(path: string, body: unknown): Promise<T> {
  let response: Response;
  try {
    response = await fetch(path, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
  } catch {
    throw new ApiError("Can't reach the server", null);
  }
  const answer = (await response.json().catch(() => null)) as (T & { error?: string }) | null;
  if (!response.ok) throw new ApiError(answer?.error ?? `The server said ${response.status}`, response.status);
  return answer as T;
}

/** Starts an agent on `task`, in a fresh tree cut from `base`, or in `tree`. */
export function dispatch(
  workspace: string | null,
  asked: { task: string; tree: string | null; base: string | null },
): Promise<{ tree: string }> {
  return post<{ tree: string }>(`/api/dispatch${workspaceQuery(workspace)}`, asked);
}

/** Starts the tree's stopped or failed agent again. */
export function resume(workspace: string | null, tree: string): Promise<void> {
  return post<void>(`/api/trees/${encodeURIComponent(tree)}/resume${workspaceQuery(workspace)}`, {});
}

/** Gives the tree back to the pool, stopping its agents. */
export function giveBack(workspace: string | null, tree: string): Promise<void> {
  return post<void>(`/api/trees/${encodeURIComponent(tree)}/return${workspaceQuery(workspace)}`, {});
}

/** Sets the tree aside under Parked, or brings it back. */
export function park(workspace: string | null, tree: string, parked: boolean): Promise<void> {
  return post<void>(`/api/trees/${encodeURIComponent(tree)}/park${workspaceQuery(workspace)}`, { parked });
}

/** Puts the tree on the Backburner, or brings it back. */
export function backburner(workspace: string | null, tree: string, backburner: boolean): Promise<void> {
  return post<void>(`/api/trees/${encodeURIComponent(tree)}/backburner${workspaceQuery(workspace)}`, { backburner });
}

/** Shares a port a tree serves with the phone, and says where to reach it. */
export function sharePort(workspace: string | null, port: number): Promise<{ url: string }> {
  return post<{ url: string }>(`/api/ports/${port}/share${workspaceQuery(workspace)}`, {});
}

/** Puts a port a tree serves on the internet and says its public address. */
export function publishPort(workspace: string | null, port: number): Promise<{ url: string }> {
  return post<{ url: string }>(`/api/ports/${port}/publish${workspaceQuery(workspace)}`, {});
}

/** Takes a port off the internet, and off the tailnet with it. */
export function unsharePort(workspace: string | null, port: number): Promise<void> {
  return post<void>(`/api/ports/${port}/unshare${workspaceQuery(workspace)}`, {});
}

/** The server's key a browser subscribes to pushes with; null when it sends none. */
export async function fetchPushKey(): Promise<string | null> {
  try {
    const response = await fetch("/api/push/key", { cache: "no-store" });
    if (!response.ok) return null;
    return ((await response.json()) as { key: string }).key;
  } catch {
    return null;
  }
}

/** Tells the server where to push to this device. */
export function subscribePush(subscription: PushSubscriptionJSON): Promise<void> {
  return post<void>("/api/push/subscribe", subscription);
}

/** How a prompt the agent waits on is answered: let it run, stop it, or in words. */
export type Answer = { action: "allow" } | { action: "deny" } | { action: "say"; text: string };

/** Answers the prompt the tree's agent waits on. */
export function answer(workspace: string | null, tree: string, given: Answer): Promise<void> {
  return post<void>(`/api/trees/${encodeURIComponent(tree)}/answer${workspaceQuery(workspace)}`, given);
}

function issuesQuery(workspace: string | null, source: string | null, extra: Record<string, string> = {}): string {
  const query = new URLSearchParams(extra);
  if (workspace) query.set("workspace", workspace);
  if (source) query.set("source", source);
  const text = query.toString();
  return text ? `?${text}` : "";
}

/** The issue sources there are, the views, and the repository's own. */
export function fetchIssueSources(workspace: string | null): Promise<IssueSources> {
  return get<IssueSources>(`/api/issues/sources${issuesQuery(workspace, null)}`);
}

export function fetchIssues(
  workspace: string | null,
  source: string | null,
  line: string,
  group: IssueGroup,
): Promise<IssueList> {
  return get<IssueList>(`/api/issues${issuesQuery(workspace, source, { line, group })}`);
}

/** What the source's issues can be set to; null for one that cannot change them. */
export async function fetchIssueTeam(workspace: string | null, source: string | null): Promise<IssueTeam | null> {
  const answer = await get<{ team: IssueTeam | null }>(`/api/issues/team${issuesQuery(workspace, source)}`);
  return answer.team;
}

export function fetchIssue(workspace: string | null, source: string | null, id: string): Promise<IssueDetail> {
  return get<IssueDetail>(`/api/issues/${encodeURIComponent(id)}${issuesQuery(workspace, source)}`);
}

export function changeIssue(
  workspace: string | null,
  source: string | null,
  id: string,
  change: IssueChange,
): Promise<IssueRow> {
  return post<IssueRow>(`/api/issues/${encodeURIComponent(id)}${issuesQuery(workspace, source)}`, change);
}

export function createIssue(workspace: string | null, source: string | null, issue: NewIssue): Promise<IssueRow> {
  return post<IssueRow>(`/api/issues${issuesQuery(workspace, source)}`, issue);
}

export function commentOnIssue(
  workspace: string | null,
  source: string | null,
  id: string,
  body: string,
): Promise<IssueComment> {
  return post<IssueComment>(`/api/issues/${encodeURIComponent(id)}/comment${issuesQuery(workspace, source)}`, { body });
}

/** Puts an agent on the issue in a fresh tree cut from `base`; says which tree. */
export function pickIssue(
  workspace: string | null,
  source: string | null,
  id: string,
  base: string | null,
): Promise<{ tree: string }> {
  return post<{ tree: string }>(`/api/issues/${encodeURIComponent(id)}/pick${issuesQuery(workspace, source)}`, { base });
}
