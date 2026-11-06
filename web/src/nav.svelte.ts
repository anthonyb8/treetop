// Where the app is: the workspace on show and the screen, which is the
// address, so the phone's back gesture, a reload and a link all land on
// the same screen.
import { recall, remember } from "./store";

export type Route =
  | { screen: "pool" }
  | { screen: "tree"; tree: string }
  | { screen: "chat"; tree: string }
  | { screen: "new"; tree: string | null }
  | { screen: "inbox" }
  | { screen: "issues" }
  | { screen: "issue"; id: string }
  | { screen: "issue-form"; id: string | null };

export function parse(path: string): Route {
  if (/^\/new\/?$/.test(path)) return { screen: "new", tree: null };
  if (/^\/inbox\/?$/.test(path)) return { screen: "inbox" };
  if (/^\/issues\/?$/.test(path)) return { screen: "issues" };
  if (/^\/issues\/new\/?$/.test(path)) return { screen: "issue-form", id: null };
  const issue = /^\/issues\/([^/]+)(\/edit)?\/?$/.exec(path);
  if (issue?.[1]) {
    const id = decodeURIComponent(issue[1]);
    return issue[2] ? { screen: "issue-form", id } : { screen: "issue", id };
  }
  const found = /^\/trees\/([^/]+)(?:\/(chat|new))?\/?$/.exec(path);
  if (!found?.[1]) return { screen: "pool" };
  const tree = decodeURIComponent(found[1]);
  if (found[2] === "chat") return { screen: "chat", tree };
  if (found[2] === "new") return { screen: "new", tree };
  return { screen: "tree", tree };
}

// A link that names its workspace, as a notification's does, opens there
// and makes it the one chosen, so its tree is the right repository's tree.
// The address keeps only the screen, as every other one does.
const linked = new URLSearchParams(location.search).get("workspace");
if (linked) {
  remember("workspace", linked);
  history.replaceState(history.state, "", location.pathname);
}

export const nav = $state({
  path: location.pathname,
  workspace: linked ?? recall<string>("workspace"),
  /** The issue source chosen; null for the repository's own. */
  source: recall<string>("source"),
  /** How many trees wait on you, for the tab bar. */
  needs: 0,
  /** The source on show can create issues, for the tab bar. */
  canCreate: true,
});

/** How many screens deep the app went itself, so back never leaves it. */
let depth = (history.state as { depth?: number } | null)?.depth ?? 0;

export function go(path: string): void {
  if (path === nav.path) return;
  depth += 1;
  history.pushState({ depth }, "", path);
  nav.path = path;
}

/** The screen under this one: a chat's tree, a tree's pool. */
function parent(path: string): string {
  const route = parse(path);
  if (route.screen === "chat") return `/trees/${encodeURIComponent(route.tree)}`;
  if (route.screen === "new" && route.tree) return `/trees/${encodeURIComponent(route.tree)}`;
  if (route.screen === "issue") return "/issues";
  if (route.screen === "issue-form") return route.id ? `/issues/${encodeURIComponent(route.id)}` : "/issues";
  return "/";
}

/** Back a screen; to the one under it when the app was opened on this one. */
export function back(): void {
  if (depth > 0) {
    history.back();
  } else {
    const to = parent(nav.path);
    history.replaceState({ depth: 0 }, "", to);
    nav.path = to;
  }
}

export function chooseWorkspace(name: string): void {
  nav.workspace = name;
  remember("workspace", name);
}

export function chooseSource(name: string): void {
  nav.source = name;
  remember("source", name);
}

window.addEventListener("popstate", (event) => {
  depth = (event.state as { depth?: number } | null)?.depth ?? 0;
  nav.path = location.pathname;
});

/** Follows a same-app link without reloading the page. */
export function link(event: MouseEvent): void {
  const anchor = event.currentTarget as HTMLAnchorElement;
  if (event.metaKey || event.ctrlKey || event.shiftKey || anchor.origin !== location.origin) return;
  event.preventDefault();
  go(anchor.pathname);
}
