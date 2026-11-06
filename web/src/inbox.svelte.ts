// The inbox, as the desktop's beside the list: what wants you in every
// workspace. Asked for while the app is open, whichever screen is on show,
// so the tab's count is current before the inbox is opened.
import { ApiError, fetchInbox, markInbox, type Inbox, type InboxEntry } from "./api";
import { chooseWorkspace, go, nav } from "./nav.svelte";

/** How often the inbox is asked for while the app is on screen. */
const EVERY_MS = 5000;

export const inbox = $state<{ list: Inbox | null; failure: string | null }>({ list: null, failure: null });

export async function loadInbox(): Promise<void> {
  try {
    const asked = nav.workspace;
    const fresh = await fetchInbox(asked);
    if (asked !== nav.workspace) return;
    inbox.failure = null;
    // Partway through reading a pool the server knows only some of what
    // waits, and an empty answer would say nothing does: the last list
    // stays, or the screen stays loading, until it has read them all.
    if (fresh.is_reading) return;
    inbox.list = fresh;
  } catch (err) {
    inbox.failure = err instanceof ApiError ? err.message : "Something went wrong";
  }
}

/** Asks now and every few seconds while the app is on screen; the returned
 * function stops it. */
export function watchInbox(): () => void {
  void loadInbox();
  const timer = setInterval(() => {
    if (document.visibilityState === "visible") void loadInbox();
  }, EVERY_MS);
  const onVisible = () => {
    if (document.visibilityState === "visible") void loadInbox();
  };
  document.addEventListener("visibilitychange", onVisible);
  return () => {
    clearInterval(timer);
    document.removeEventListener("visibilitychange", onVisible);
  };
}

/** Opens what the entry is about, as a notification of it would: an agent's
 * chat, else its tree. Another workspace's entry switches there first. */
export function openEntry(entry: InboxEntry): void {
  if (!entry.seen && inbox.list) {
    entry.seen = true;
    inbox.list.unseen = Math.max(0, inbox.list.unseen - 1);
  }
  void markInbox(entry.key, "seen").catch(() => undefined);
  if (entry.workspace) chooseWorkspace(entry.workspace);
  const tree = `/trees/${encodeURIComponent(entry.tree)}`;
  go(entry.kind === "needs_you" || entry.kind === "done" ? `${tree}/chat` : tree);
}

/** Takes the entry out of the inbox, here and on the desktop. */
export async function dismissEntry(entry: InboxEntry): Promise<void> {
  await markInbox(entry.key, "dismiss");
  if (inbox.list) {
    inbox.list = {
      ...inbox.list,
      entries: inbox.list.entries.filter((e) => e.key !== entry.key),
      unseen: inbox.list.unseen - (entry.seen ? 0 : 1),
    };
  }
}
