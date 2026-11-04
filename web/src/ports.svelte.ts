// Reaching a port a tree serves: a tap shares it, as `p` does in the
// terminal, then opens it. A phone may refuse to open a window once the tap
// is a moment old, so the address stays on screen as a link to tap.
//
// Or a port goes public, as `P` puts it on the internet, for sending to
// someone off the tailnet: the link is copied as it arrives, and the toast
// offers the share sheet, to text it, and a way to take it down again.
import { ApiError, publishPort, sharePort, unsharePort } from "./api";
import { nav } from "./nav.svelte";

/** How long the link stays once the port has opened by itself. */
const LINGER_MS = 8000;

/** How long "Stopped" stays once a public port has come down. */
const STOPPED_MS = 3000;

interface Poke {
  port: number;
  url: string | null;
  why: string | null;
  /** On the internet, from the public-link button, not only the tailnet. */
  isPublic: boolean;
  /** The public link is on the clipboard. */
  isCopied: boolean;
  /** Taking it down: under way, then done. */
  stop: "stopping" | "stopped" | null;
}

export const poking = $state<{ current: Poke | null }>({ current: null });

let latest = 0;
let timer: ReturnType<typeof setTimeout> | undefined;

export function dismiss() {
  latest += 1;
  clearTimeout(timer);
  poking.current = null;
}

function show(poke: Partial<Poke> & { port: number }) {
  poking.current = { url: null, why: null, isPublic: false, isCopied: false, stop: null, ...poke };
}

function why(err: unknown, otherwise: string): string {
  return err instanceof ApiError ? err.message : otherwise;
}

export async function poke(port: number) {
  dismiss();
  const mine = latest;
  show({ port });
  try {
    const { url } = await sharePort(nav.workspace, port);
    if (mine !== latest) return;
    show({ port, url });
    const tab = window.open(url, "_blank");
    if (tab) {
      tab.opener = null;
      timer = setTimeout(dismiss, LINGER_MS);
    }
  } catch (err) {
    if (mine !== latest) return;
    show({ port, why: why(err, "It could not be shared") });
  }
}

/**
 * Puts `port` on the internet and copies its link, or, where it is public
 * already at `known`, opens the share sheet with it straight from the tap.
 */
export async function goPublic(port: number, known: string | null) {
  dismiss();
  const mine = latest;
  if (known) {
    show({ port, url: known, isPublic: true });
    // The tap is still fresh, which the share sheet and clipboard ask for.
    if (canShare()) {
      void sendLink();
      return;
    }
    const isCopied = await copy(Promise.resolve(known));
    if (mine === latest && poking.current) poking.current.isCopied = isCopied;
    return;
  }
  show({ port, isPublic: true });
  const asked = publishPort(nav.workspace, port);
  // Started inside the tap, before any wait: iOS refuses a copy once the
  // tap is a few seconds old, and publishing can take that long.
  const copied = copy(asked.then(({ url }) => url));
  try {
    const { url } = await asked;
    if (mine !== latest) return;
    show({ port, url, isPublic: true });
    const isCopied = await copied;
    if (mine === latest && poking.current) poking.current.isCopied = isCopied;
  } catch (err) {
    if (mine !== latest) return;
    show({ port, isPublic: true, why: why(err, "It could not be published") });
  }
}

/** The phone can hand a link to its share sheet, to text or post it. */
export function canShare(): boolean {
  return typeof navigator.share === "function";
}

/** Opens the share sheet with the public link, else copies it. */
export async function sendLink() {
  const shown = poking.current;
  if (!shown?.url) return;
  if (canShare()) {
    // Closing the sheet without sending rejects, which is no failure.
    await navigator.share({ url: shown.url }).catch(() => undefined);
    return;
  }
  await copyLink();
}

/** Copies the public link again, from a tap of its own. */
export async function copyLink() {
  const shown = poking.current;
  if (!shown?.url) return;
  const isCopied = await copy(Promise.resolve(shown.url));
  if (poking.current === shown) shown.isCopied = isCopied;
}

/** Takes the public port down, off the tailnet with it, as `tether unshare` does. */
export async function stopPublic() {
  const shown = poking.current;
  if (!shown?.isPublic || shown.stop) return;
  const mine = latest;
  shown.stop = "stopping";
  try {
    await unsharePort(nav.workspace, shown.port);
    if (mine !== latest) return;
    show({ port: shown.port, isPublic: true, stop: "stopped" });
    timer = setTimeout(dismiss, STOPPED_MS);
  } catch (err) {
    if (mine !== latest) return;
    show({ port: shown.port, isPublic: true, url: shown.url, why: why(err, "It could not be stopped") });
  }
}

/**
 * Puts `text` on the clipboard once it arrives. A ClipboardItem takes the
 * promise itself, so the copy counts as made during the tap however long the
 * text takes; where there is none, it is written once it is here.
 */
async function copy(text: Promise<string>): Promise<boolean> {
  try {
    if (typeof ClipboardItem === "function" && navigator.clipboard?.write) {
      const blob = text.then((t) => new Blob([t], { type: "text/plain" }));
      await navigator.clipboard.write([new ClipboardItem({ "text/plain": blob })]);
      return true;
    }
    await navigator.clipboard.writeText(await text);
    return true;
  } catch {
    return false;
  }
}
