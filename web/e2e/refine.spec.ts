// The phone's rough edges: footers that sit no higher than they must, a port
// a tap away, parking without a terminal, and replies that arrive the way
// someone writes them.
import { expect, test, type Page } from "@playwright/test";
import type { Card, ChatMessage, Pool, Tree } from "../src/api";
import { pool, serve, shot, tree17 } from "./fixtures";

/** An iPhone's home indicator, which headless Chromium does not have. */
async function withHomeIndicator(page: Page) {
  await page.addInitScript(() => {
    addEventListener("DOMContentLoaded", () => {
      const style = document.createElement("style");
      style.textContent = ":root { --safe-bottom: 34px; }";
      document.head.append(style);
    });
  });
}

/** How far `selector`'s bottom edge sits above the screen's. */
function gapBelow(page: Page, selector: string) {
  return page.locator(selector).evaluate((el) => innerHeight - el.getBoundingClientRect().bottom);
}

function events(...parts: [string, unknown][]): string {
  return "retry: 200\n\n" + parts.map(([name, data]) => `event: ${name}\ndata: ${JSON.stringify(data)}\n\n`).join("");
}

const working: Tree = {
  ...tree17,
  card: { ...tree17.card, mark: "busy", needs_you: false },
  agents: [{ ...tree17.agents[0]!, status: "busy" }],
  report: { state: "working", says: "fixing the redirect", steps: [] },
};

/** Tree 17's chat: the backlog, then what `later` gives once it says so. */
async function chat(page: Page, backlog: ChatMessage[], later: () => string, tree: Tree = working) {
  await serve(page, () => pool);
  await page.route("**/api/trees/17", (route) => route.fulfill({ json: tree }));
  let connections = 0;
  await page.route("**/api/trees/17/chat**", (route) => {
    connections += 1;
    return route.fulfill({
      headers: { "Content-Type": "text/event-stream" },
      body: connections === 1 ? events(["backlog", backlog]) : later(),
    });
  });
  await page.goto("/trees/17/chat");
}

test.describe("the foot of each screen", () => {
  test("the chat's box sits just above the home indicator", async ({ page }) => {
    await withHomeIndicator(page);
    await chat(page, [], () => ": quiet\n\n");
    await page.getByRole("textbox", { name: "Message 17" }).waitFor();
    expect(await gapBelow(page, "form textarea")).toBeLessThanOrEqual(22);
  });

  test("with the keyboard up there is no indicator to clear", async ({ page }) => {
    await withHomeIndicator(page);
    await chat(page, [], () => ": quiet\n\n");
    await page.getByRole("textbox", { name: "Message 17" }).focus();
    expect(await gapBelow(page, "form textarea")).toBeLessThanOrEqual(8);
  });

  test("the pool's button sits just above the home indicator", async ({ page }) => {
    await withHomeIndicator(page);
    await serve(page, () => pool);
    await page.goto("/");
    await page.getByRole("link", { name: "New agent" }).waitFor();
    expect(await gapBelow(page, "a.add")).toBeLessThanOrEqual(22);
  });

  test("a tree clears the indicator once, not again above its footer", async ({ page }) => {
    await withHomeIndicator(page);
    await serve(page, () => pool);
    await page.goto("/trees/17");
    await page.getByRole("button", { name: "More" }).waitFor();
    const under = await page.locator("main").last().evaluate((el) => parseFloat(getComputedStyle(el).paddingBottom));
    expect(under).toBeLessThanOrEqual(16);
    expect(await gapBelow(page, "footer .action.primary")).toBeLessThanOrEqual(22);
  });
});

test.describe("a port a tap away", () => {
  test("a port on a card opens where the phone can reach it", async ({ page, context }) => {
    await serve(page, () => pool);
    let asked: string | null = null;
    await page.route("**/api/ports/5173/share**", (route) => {
      asked = route.request().url();
      return route.fulfill({ json: { url: "https://server.example.ts.net:5173" } });
    });
    await context.route("https://server.example.ts.net:5173/**", (route) =>
      route.fulfill({ contentType: "text/html", body: "<title>the app</title>" }),
    );
    await page.goto("/");
    const opening = context.waitForEvent("page");
    await page.getByRole("button", { name: "Open port 5173" }).first().click();
    const tab = await opening;
    await expect(tab).toHaveURL("https://server.example.ts.net:5173/");
    expect(asked).not.toBeNull();
    await expect(page).toHaveURL(/\/$/);
  });

  test("a port blocked from opening by itself leaves a link to tap", async ({ page }) => {
    await serve(page, () => pool);
    await page.route("**/api/ports/5173/share**", (route) =>
      route.fulfill({ json: { url: "https://server.example.ts.net:5173" } }),
    );
    await page.addInitScript(() => (window.open = () => null));
    await page.goto("/");
    await page.getByRole("button", { name: "Open port 5173" }).first().click();
    const link = page.getByRole("link", { name: "Open :5173" });
    await expect(link).toHaveAttribute("href", "https://server.example.ts.net:5173");
    await shot(page, "test-results/port-link.png");
  });

  test("a port that could not be shared says why", async ({ page }) => {
    await serve(page, () => pool);
    await page.route("**/api/ports/5173/share**", (route) =>
      route.fulfill({ status: 502, json: { error: "tether printed no address" } }),
    );
    await page.goto("/");
    await page.getByRole("button", { name: "Open port 5173" }).first().click();
    await expect(page.getByRole("alert")).toContainText("tether printed no address");
  });

  test("the rest of the card still opens the tree", async ({ page }) => {
    await serve(page, () => pool);
    await page.goto("/");
    await page.getByRole("link", { name: "tree 17" }).click();
    await expect(page).toHaveURL(/\/trees\/17$/);
  });

  test("a chat offers its tree's ports, and so does the tree", async ({ page }) => {
    await chat(page, [], () => ": quiet\n\n");
    const onTop = page.locator(".layer").last();
    await expect(onTop.getByRole("button", { name: "Open port 5173" })).toHaveCount(1);
    await page.getByRole("button", { name: "Back to tree 17" }).click();
    await expect(page.locator("section[aria-label='Tree']").getByRole("button", { name: "Open port 5173" })).toHaveCount(1);
  });
});

test.describe("a public link to send", () => {
  const LINK = "https://server.example.ts.net/";

  /** What the phone's share sheet was handed, or null where it has none. */
  async function withShareSheet(page: Page) {
    await page.addInitScript(() => {
      (window as unknown as { shared: ShareData[] }).shared = [];
      navigator.share = async (data?: ShareData) => {
        (window as unknown as { shared: ShareData[] }).shared.push(data ?? {});
      };
    });
  }

  function shared(page: Page): Promise<ShareData[]> {
    return page.evaluate(() => (window as unknown as { shared: ShareData[] }).shared);
  }

  test("a port goes public, its link is copied, and Share hands it to the share sheet", async ({ page, context }) => {
    await context.grantPermissions(["clipboard-read", "clipboard-write"]);
    await withShareSheet(page);
    await serve(page, () => pool);
    let published = 0;
    await page.route("**/api/ports/5173/publish**", async (route) => {
      published += 1;
      // As slow as Funnel can be, which a copy made after the wait would lose.
      await new Promise((done) => setTimeout(done, 1500));
      return route.fulfill({ json: { url: LINK } });
    });
    await page.goto("/trees/17");
    await page.getByRole("button", { name: "Get a public link to port 5173" }).click();
    await expect(page.getByRole("status")).toContainText("Publishing :5173");
    await expect(page.getByRole("status")).toContainText("Public link copied");
    await expect(page.getByRole("status")).toContainText(LINK);
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(LINK);
    expect(published).toBe(1);
    await shot(page, "test-results/public-link.png");
    await page.getByRole("button", { name: "Share the public link" }).click();
    expect(await shared(page)).toEqual([{ url: LINK }]);
  });

  test("a phone that will not copy leaves the link and a Copy button", async ({ page }) => {
    await page.addInitScript(() => {
      const refuse = () => Promise.reject(new DOMException("not allowed", "NotAllowedError"));
      Object.defineProperty(navigator, "share", { value: undefined });
      navigator.clipboard.write = refuse;
      navigator.clipboard.writeText = refuse;
    });
    await serve(page, () => pool);
    await page.route("**/api/ports/5173/publish**", (route) => route.fulfill({ json: { url: LINK } }));
    await page.goto("/trees/17");
    await page.getByRole("button", { name: "Get a public link to port 5173" }).click();
    await expect(page.getByRole("status")).toContainText("Public link ready");
    await expect(page.getByRole("status")).toContainText(LINK);
    await expect(page.getByRole("button", { name: "Copy the public link" })).toBeVisible();
    await shot(page, "test-results/public-link-uncopied.png");
  });

  test("Stop takes a public port down", async ({ page }) => {
    await withShareSheet(page);
    await serve(page, () => pool);
    await page.route("**/api/ports/5173/publish**", (route) => route.fulfill({ json: { url: LINK } }));
    let stopped = false;
    await page.route("**/api/ports/5173/unshare**", (route) => {
      stopped = true;
      return route.fulfill({ json: {} });
    });
    await page.goto("/trees/17");
    await page.getByRole("button", { name: "Get a public link to port 5173" }).click();
    await page.getByRole("button", { name: "Stop publishing :5173" }).click();
    await expect(page.getByRole("status")).toContainText(":5173 is no longer public");
    expect(stopped).toBe(true);
  });

  test("a port already public sends its link at once, publishing nothing", async ({ page }) => {
    await withShareSheet(page);
    await serve(page, () => pool);
    await page.route("**/api/trees/17", (route) =>
      route.fulfill({ json: { ...tree17, ports: [{ ...tree17.ports[0]!, public: LINK }] } }),
    );
    let published = false;
    await page.route("**/api/ports/5173/publish**", (route) => {
      published = true;
      return route.fulfill({ json: { url: LINK } });
    });
    await page.goto("/trees/17");
    await page.getByRole("button", { name: "Send the public link to port 5173" }).click();
    await expect.poll(() => shared(page)).toEqual([{ url: LINK }]);
    await expect(page.getByRole("button", { name: "Stop publishing :5173" })).toBeVisible();
    expect(published).toBe(false);
    await shot(page, "test-results/public-already.png");
  });

  test("a port that could not go public says why", async ({ page }) => {
    await serve(page, () => pool);
    await page.route("**/api/ports/5173/publish**", (route) =>
      route.fulfill({ status: 502, json: { error: "tether failed: Funnel is not enabled" } }),
    );
    await page.goto("/trees/17");
    await page.getByRole("button", { name: "Get a public link to port 5173" }).click();
    await expect(page.getByRole("alert")).toContainText("Could not publish :5173: tether failed: Funnel is not enabled");
  });
});

/** Drags a finger across `selector` from its right edge, `by` pixels left. */
async function swipe(page: Page, selector: string, by: number, lift = true) {
  await page.locator(selector).first().scrollIntoViewIfNeeded();
  const box = (await page.locator(selector).first().boundingBox())!;
  const y = box.y + box.height / 2;
  const x = box.x + box.width - 20;
  const cdp = await page.context().newCDPSession(page);
  const touch = (type: string, at: number) =>
    cdp.send("Input.dispatchTouchEvent", {
      type: type as "touchStart",
      touchPoints: type === "touchEnd" ? [] : [{ x: at, y }],
    });
  await touch("touchStart", x);
  for (let dx = 10; dx <= by; dx += 10) await touch("touchMove", x - dx);
  if (lift) await touch("touchEnd", x - by);
}

test.describe("parking", () => {
  function parkedPool(): { current: Pool; asked: unknown[] } {
    return { current: structuredClone(pool), asked: [] };
  }

  /** The server, parking and bringing back as it is asked. */
  async function serveParking(page: Page, state: { current: Pool; asked: unknown[] }) {
    await serve(page, () => state.current);
    await page.route(/\/api\/trees\/\d+\/park/, async (route) => {
      const { parked } = route.request().postDataJSON() as { parked: boolean };
      const name = /trees\/(\d+)\/park/.exec(route.request().url())![1]!;
      state.asked.push({ tree: name, parked });
      const main = state.current.groups[0]!;
      const everyone: Card[] = [...main.cards, ...state.current.parked];
      const moved = everyone.find((c) => c.tree === name)!;
      main.cards = main.cards.filter((c) => c.tree !== name);
      state.current.parked = state.current.parked.filter((c) => c.tree !== name);
      if (parked) state.current.parked.push({ ...moved, parked });
      else main.cards.push({ ...moved, parked });
      await route.fulfill({ json: {} });
    });
  }

  test("swiping a card away parks it", async ({ page }) => {
    const state = parkedPool();
    await serveParking(page, state);
    await page.goto("/");
    await swipe(page, ".swipe:has([aria-label='tree 14'])", 160);
    await expect(page.getByRole("button", { name: "Parked · 2" })).toBeVisible();
    await expect(page.getByRole("link", { name: "tree 14" })).toHaveCount(0);
    expect(state.asked).toEqual([{ tree: "14", parked: true }]);
    await expect(page).toHaveURL(/\/$/);
  });

  test("a short swipe springs back and does nothing", async ({ page }) => {
    const state = parkedPool();
    await serveParking(page, state);
    await page.goto("/");
    await swipe(page, ".swipe:has([aria-label='tree 14'])", 40);
    await expect(page.locator(".swipe:has([aria-label='tree 14']) .front")).not.toHaveAttribute("style", /translateX/);
    expect(state.asked).toEqual([]);
    await expect(page).toHaveURL(/\/$/);
  });

  test("swiping a parked card brings it back", async ({ page }) => {
    const state = parkedPool();
    await serveParking(page, state);
    await page.goto("/");
    await page.getByRole("button", { name: "Parked · 1" }).click();
    await swipe(page, ".swipe:has([aria-label='tree 6'])", 160);
    await expect(page.getByRole("button", { name: /^Parked/ })).toHaveCount(0);
    await expect(page.getByRole("link", { name: "tree 6" })).toBeVisible();
    expect(state.asked).toEqual([{ tree: "6", parked: false }]);
  });

  test("a card that would not park comes back and says why", async ({ page }) => {
    await serve(page, () => pool);
    await page.route(/\/api\/trees\/\d+\/park/, (route) =>
      route.fulfill({ status: 409, json: { error: "tree 14 is not leased: nothing to park" } }),
    );
    await page.goto("/");
    await swipe(page, ".swipe:has([aria-label='tree 14'])", 160);
    await expect(page.getByRole("alert")).toContainText("nothing to park");
    await expect(page.getByRole("link", { name: "tree 14" })).toBeVisible();
  });

  test("a tree parks and comes back from its own screen", async ({ page }) => {
    let tree: Tree = tree17;
    const asked: boolean[] = [];
    await serve(page, () => pool);
    await page.route("**/api/trees/17", (route) => route.fulfill({ json: tree }));
    await page.route("**/api/trees/17/park**", (route) => {
      const { parked } = route.request().postDataJSON() as { parked: boolean };
      asked.push(parked);
      tree = { ...tree, card: { ...tree.card, parked } };
      return route.fulfill({ json: {} });
    });
    await page.goto("/trees/17");
    await page.getByRole("button", { name: "More" }).click();
    await page.getByRole("button", { name: "Park", exact: true }).click();
    await expect(page.getByText("parked", { exact: true })).toBeVisible();
    await shot(page, "test-results/tree-parked.png");
    await page.getByRole("button", { name: "More" }).click();
    await page.getByRole("button", { name: "Unpark" }).click();
    await expect(page.getByText("parked", { exact: true })).toHaveCount(0);
    expect(asked).toEqual([true, false]);
  });
});

test.describe("a reply as it is written", () => {
  const paragraph =
    "The guard redirects before the token refresh finishes, so an expired session bounces between the login page and the app, over and over. ";
  const reply = paragraph.repeat(4) + "Moving **the check** after the `refresh` fixes it.";

  /** Tree 17's chat, with `later` sent once, just after it opens. */
  async function open(page: Page, later: [string, unknown][], backlog: ChatMessage[] = []) {
    let isSent = false;
    await chat(page, backlog, () => {
      if (isSent) return ": quiet\n\n";
      isSent = true;
      return events(...later);
    });
  }

  test("words fade in rather than being typed in", async ({ page }) => {
    await open(page, [["message", { id: "a1", kind: "claude", text: reply }]]);
    const words = page.locator(".claude .word");
    await expect.poll(() => words.count(), { timeout: 5000 }).toBeGreaterThan(40);
    // The whole reply is there from the start, so nothing reflows and no
    // half-written Markdown shows.
    await expect(page.locator(".claude strong")).toHaveText("the check");
    const opacity = await words.evaluateAll((all) => all.map((w) => Number(getComputedStyle(w).opacity)));
    expect(opacity[0]!).toBeGreaterThan(opacity.at(-1)!);
    expect(opacity.at(-1)).toBe(0);
  });

  test("the conversation moves smoothly while a reply comes in", async ({ page }) => {
    const backlog: ChatMessage[] = Array.from({ length: 30 }, (_, i) => ({ id: `u${i}`, kind: "you", text: `Line ${i}` }));
    await open(page, [["message", { id: "a1", kind: "claude", text: reply }]], backlog);
    await page.evaluate(() => {
      const seen: number[] = [];
      (window as unknown as { tops: number[] }).tops = seen;
      const scroller = document.querySelector("main[aria-label='Conversation']")!;
      const look = () => {
        seen.push(scroller.scrollTop);
        requestAnimationFrame(look);
      };
      requestAnimationFrame(look);
    });
    await expect(page.locator(".claude .word").first()).toBeAttached({ timeout: 5000 });
    await expect(page.locator(".claude .word")).toHaveCount(0, { timeout: 8000 });
    await expect(page.locator(".claude")).toContainText("fixes it.");
    const tops = await page.evaluate(() => (window as unknown as { tops: number[] }).tops);
    const steps = tops.slice(1).map((top, i) => top - tops[i]!);
    // Follows to the end, never lurching a whole line in one frame.
    expect(Math.max(...steps)).toBeLessThan(14);
    expect(steps.filter((step) => step > 0).length).toBeGreaterThan(20);
    const atEnd = await page
      .locator("main[aria-label='Conversation']")
      .evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight);
    expect(atEnd).toBeLessThan(2);
  });

  test("what the agent did next waits for the reply before it", async ({ page }) => {
    const tool: ChatMessage = {
      id: "t1",
      kind: "tool",
      tool: { verb: "Ran", what: "the tests", added: null, removed: null, status: "running", output: null, ask: null },
    };
    await open(page, [
      ["message", { id: "a1", kind: "claude", text: reply }],
      ["message", tool],
    ]);
    await expect(page.locator(".claude .word").first()).toBeAttached({ timeout: 5000 });
    await expect(page.locator(".tool")).toHaveCount(0);
    await expect(page.locator(".tool")).toHaveCount(1, { timeout: 8000 });
  });

  test("a sent line settles into the transcript without moving", async ({ page }) => {
    let isSaid = false;
    await chat(page, [], () =>
      isSaid ? events(["message", { id: "u1", kind: "you", text: "Fix it." }]) : ": quiet\n\n",
    );
    await page.route("**/api/trees/17/send**", (route) => route.fulfill({ json: { said: "sent" } }));
    await page.getByRole("textbox", { name: "Message 17" }).fill("Fix it.");
    await page.getByRole("button", { name: "Send" }).click();
    await expect(page.getByText("Sent", { exact: true })).toBeVisible();
    await page.waitForTimeout(300);
    isSaid = true;
    await expect(page.getByText("Sent", { exact: true })).toHaveCount(0, { timeout: 3000 });
    await expect(page.locator(".you")).toHaveCount(1);
    const animations = await page.locator(".you").evaluate((el) => el.getAnimations().length);
    expect(animations).toBe(0);
  });
});
