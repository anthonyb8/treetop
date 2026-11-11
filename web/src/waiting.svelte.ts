// What waits on you, as the desktop's banner names it: in every workspace,
// the oldest first, each until it stops being true. Asked for while the app
// is open, whichever screen is on show, so the tab's count is current
// before the tab is opened.
import { ApiError, fetchWaiting, type Waiting, type WaitingEntry } from "./api";
import { chooseWorkspace, go, nav } from "./nav.svelte";

/** How often what waits is asked for while the app is on screen. */
const EVERY_MS = 5000;

export const waiting = $state<{ list: Waiting | null; failure: string | null }>({ list: null, failure: null });

export async function loadWaiting(): Promise<void> {
  try {
    const asked = nav.workspace;
    const fresh = await fetchWaiting(asked);
    if (asked !== nav.workspace) return;
    waiting.failure = null;
    // Partway through reading a pool the server knows only some of what
    // waits, and an empty answer would say nothing does: the last list
    // stays, or the screen stays loading, until it has read them all.
    if (fresh.is_reading) return;
    waiting.list = fresh;
  } catch (err) {
    waiting.failure = err instanceof ApiError ? err.message : "Something went wrong";
  }
}

/** Asks now and every few seconds while the app is on screen; the returned
 * function stops it. */
export function watchWaiting(): () => void {
  void loadWaiting();
  const timer = setInterval(() => {
    if (document.visibilityState === "visible") void loadWaiting();
  }, EVERY_MS);
  const onVisible = () => {
    if (document.visibilityState === "visible") void loadWaiting();
  };
  document.addEventListener("visibilitychange", onVisible);
  return () => {
    clearInterval(timer);
    document.removeEventListener("visibilitychange", onVisible);
  };
}

/** It lives in a workspace other than the one on show. */
export function isElsewhere(entry: { workspace: string }): boolean {
  return entry.workspace !== (nav.workspace ?? waiting.list?.workspace);
}

/** Opens what waits, as its notification would: an agent's chat, else its
 * tree. Another workspace's switches there first. */
export function openEntry(entry: WaitingEntry): void {
  if (isElsewhere(entry)) chooseWorkspace(entry.workspace);
  const tree = `/trees/${encodeURIComponent(entry.tree)}`;
  go(entry.kind === "needs_you" ? `${tree}/chat` : tree);
}
