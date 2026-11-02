// Reaching a port a tree serves: a tap shares it, as `p` does in the
// terminal, then opens it. A phone may refuse to open a window once the tap
// is a moment old, so the address stays on screen as a link to tap.
import { ApiError, sharePort } from "./api";
import { nav } from "./nav.svelte";

/** How long the link stays once the port has opened by itself. */
const LINGER_MS = 8000;

interface Poke {
  port: number;
  url: string | null;
  why: string | null;
}

export const poking = $state<{ current: Poke | null }>({ current: null });

let latest = 0;
let timer: ReturnType<typeof setTimeout> | undefined;

export function dismiss() {
  latest += 1;
  clearTimeout(timer);
  poking.current = null;
}

export async function poke(port: number) {
  dismiss();
  const mine = latest;
  poking.current = { port, url: null, why: null };
  try {
    const { url } = await sharePort(nav.workspace, port);
    if (mine !== latest) return;
    poking.current = { port, url, why: null };
    const tab = window.open(url, "_blank");
    if (tab) {
      tab.opener = null;
      timer = setTimeout(dismiss, LINGER_MS);
    }
  } catch (err) {
    if (mine !== latest) return;
    poking.current = { port, url: null, why: err instanceof ApiError ? err.message : "It could not be shared" };
  }
}
