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
  /** "unknown" where git could not read a held tree; null for a free one. */
  git: Git | "unknown" | null;
  ports: number[];
  pr: Pr | null;
  /** Seconds since its agents last worked. */
  active_ago: number | null;
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
  free: number;
  parked: Card[];
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
}

export interface Port {
  port: number;
  process: string;
  pid: number;
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

export function fetchWorkspaces(): Promise<Workspaces> {
  return get<Workspaces>("/api/workspaces");
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
