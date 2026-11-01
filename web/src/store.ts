// What the app keeps on the phone between opens: the workspace chosen and
// the last pool seen, so it opens instantly and still shows something while
// the server is out of reach. Storage can be missing or full, as in a
// private tab, so every call is allowed to fail.

const PREFIX = "treetop:";

export function remember(key: string, value: unknown): void {
  try {
    localStorage.setItem(PREFIX + key, JSON.stringify(value));
  } catch {
    // Not kept; the next open asks the server instead.
  }
}

export function recall<T>(key: string): T | null {
  try {
    const raw = localStorage.getItem(PREFIX + key);
    return raw === null ? null : (JSON.parse(raw) as T);
  } catch {
    return null;
  }
}
