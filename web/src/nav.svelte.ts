// Where the app is: the workspace on show and the screen, which is the
// address, so the phone's back gesture, a reload and a link all land on
// the same screen.
import { recall, remember } from "./store";

export type Route = { screen: "pool" } | { screen: "tree"; tree: string };

export function parse(path: string): Route {
  const tree = /^\/trees\/([^/]+)\/?$/.exec(path);
  if (tree?.[1]) return { screen: "tree", tree: decodeURIComponent(tree[1]) };
  return { screen: "pool" };
}

export const nav = $state({
  path: location.pathname,
  workspace: recall<string>("workspace"),
});

/** How many screens deep the app went itself, so back never leaves it. */
let depth = (history.state as { depth?: number } | null)?.depth ?? 0;

export function go(path: string): void {
  if (path === nav.path) return;
  depth += 1;
  history.pushState({ depth }, "", path);
  nav.path = path;
}

/** Back a screen; to the pool when the app was opened on this one. */
export function back(): void {
  if (depth > 0) {
    history.back();
  } else {
    history.replaceState({ depth: 0 }, "", "/");
    nav.path = "/";
  }
}

export function chooseWorkspace(name: string): void {
  nav.workspace = name;
  remember("workspace", name);
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
