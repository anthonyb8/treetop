// What the Issues screens work out from what the server sends.
import type { IssueRow, IssueView, Mark } from "./api";

/** Linear's priorities by number; the first is none. */
export const PRIORITIES = ["None", "Urgent", "High", "Medium", "Low"];

/** The filters laid over a view, as the filter line says them. */
export interface IssueFilters {
  /** "mine", "unassigned", "any", or a person's name. */
  assignee?: string;
  /** A state's name, or "any" for closed ones too. */
  state?: string;
  label?: string;
  project?: string;
  /** "current", or a cycle's number. */
  cycle?: string;
  group?: IssueView["group"];
}

function quoted(value: string): string {
  return value.includes(" ") ? `"${value}"` : value;
}

/** `filters` as words of a filter line. */
export function filterWords(filters: IssueFilters): string {
  const words: string[] = [];
  if (filters.assignee === "mine") words.push("mine");
  else if (filters.assignee && filters.assignee !== "any") words.push(`assignee:${quoted(filters.assignee)}`);
  for (const key of ["state", "label", "project", "cycle"] as const) {
    const value = filters[key];
    if (value) words.push(`${key}:${quoted(value)}`);
  }
  return words.join(" ");
}

/** The line that lists `view` with `filters` over it. An assignee replaces
 *  the view's `mine`, which would otherwise win. */
export function lineOf(view: IssueView, filters: IssueFilters): string {
  const own = filters.assignee ? view.line.replace(/(^|\s)mine(?=\s|$)/, "").trim() : view.line;
  return [own, filterWords(filters)].filter(Boolean).join(" ");
}

/** What the WORK column says of an issue, and how loudly. */
export function workOf(issue: IssueRow): { text: string; mark: Mark | null; tone: "loud" | "busy" | "ready" | "quiet" } | null {
  if (issue.is_ready) return { text: "ready", mark: null, tone: "ready" };
  const card = issue.card;
  if (!card) return null;
  const word = card.mark === "no-agent" || card.mark === "none" ? "no agent" : card.mark;
  const tone = card.needs_you ? "loud" : card.mark === "busy" ? "busy" : "quiet";
  return { text: `${card.tree} · ${word}`, mark: card.mark, tone };
}

/** Seconds since `at`, an RFC 3339 time. */
export function secondsSince(at: string): number {
  return Math.max(0, Math.floor((Date.now() - Date.parse(at)) / 1000));
}
