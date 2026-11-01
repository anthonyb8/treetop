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
  const query = workspace ? `?workspace=${encodeURIComponent(workspace)}` : "";
  return get<Pool>(`/api/pool${query}`);
}

export function fetchWorkspaces(): Promise<Workspaces> {
  return get<Workspaces>("/api/workspaces");
}
