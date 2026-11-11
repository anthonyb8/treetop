// ANB-52. A phone that opened an earlier treetop must pick up the new one by
// itself: once updated, nothing it stored answers for the server, and an app
// that does not start says so rather than sitting on a blank screen.
//
// These need a real service worker, which the rest of the suite blocks, and a
// server that can change what it serves between loads, as an update does, so
// they bring their own in place of `vite preview`.
import { readFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { extname, join, normalize } from "node:path";
import { expect, test, type BrowserContext, type Page } from "@playwright/test";
import { pool, shot } from "./fixtures";

test.use({ serviceWorkers: "allow" });

const dist = join(import.meta.dirname, "..", "dist");
const released = readFileSync(join(import.meta.dirname, "released-sw.js"), "utf8");
const types: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json",
  ".webmanifest": "application/manifest+json",
  ".png": "image/png",
  ".svg": "image/svg+xml",
};

/** What the test server does, changed between loads to stand in for an update or a fault. */
interface Serving {
  url: string;
  /** The service worker it serves: the released one, or this build's. */
  worker: "released" | "built";
  /** How many more requests for the app's script it refuses. */
  refuseScript: number;
  /** Requests for the app's page it answered. */
  pages: number;
  /** While set, requests for the app's script wait for it to resolve. */
  holdScript: Promise<void> | null;
}

let server: Server;
let state: Serving;

test.beforeEach(async () => {
  state = { url: "", worker: "built", refuseScript: 0, pages: 0, holdScript: null };
  server = createServer(async (request, response) => {
    const path = new URL(request.url ?? "/", "http://x").pathname;
    const json = (body: unknown) => {
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(JSON.stringify(body));
    };
    if (path === "/api/pool") return json(pool);
    if (path === "/api/workspaces") return json({ default: "app", workspaces: ["app"] });
    if (path.startsWith("/api/waiting")) return json({ workspace: "app", entries: [], also: [], is_reading: false });
    if (path.startsWith("/api/")) {
      response.writeHead(404, { "Content-Type": "application/json" });
      return response.end(JSON.stringify({ error: "not in this test" }));
    }
    if (path === "/sw.js") {
      response.writeHead(200, { "Content-Type": types[".js"], "Cache-Control": "no-cache" });
      return response.end(state.worker === "released" ? released : readFileSync(join(dist, "sw.js")));
    }
    if (path.startsWith("/assets/") && path.endsWith(".js") && state.refuseScript > 0) {
      state.refuseScript -= 1;
      response.writeHead(404);
      return response.end("no such file");
    }
    if (path.startsWith("/assets/") && path.endsWith(".js") && state.holdScript) await state.holdScript;
    const file = extname(path) ? normalize(path).replace(/^(\.\.[/\\])+/, "") : "/index.html";
    if (file === "/index.html") state.pages += 1;
    try {
      const body = readFileSync(join(dist, file));
      response.writeHead(200, { "Content-Type": types[extname(file)] ?? "application/octet-stream", "Cache-Control": "no-cache" });
      response.end(body);
    } catch {
      response.writeHead(404);
      response.end("no such file");
    }
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  state.url = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
});

test.afterEach(async () => {
  state.holdScript = null;
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
});

async function storedCaches(page: Page): Promise<string[]> {
  return page.evaluate(() => caches.keys());
}

async function phone(context: BrowserContext): Promise<Page> {
  const page = await context.newPage();
  await page.goto(state.url);
  await expect(page.getByRole("region", { name: "Needs you" })).toBeVisible();
  return page;
}

test("a phone on the released worker answers from the server once updated, keeping its registration", async ({
  context,
}) => {
  state.worker = "released";
  const page = await phone(context);
  await page.evaluate(() => navigator.serviceWorker.ready);
  await page.reload();
  await expect(page.getByRole("region", { name: "Needs you" })).toBeVisible();
  await expect.poll(() => storedCaches(page)).toEqual(["treetop-v1"]);
  const before = await page.evaluate(async () => (await navigator.serviceWorker.getRegistration())?.scope);

  // treetop serve is updated: the next open fetches the new worker.
  state.worker = "built";
  await page.reload();
  await page.evaluate(async () => (await navigator.serviceWorker.getRegistration())?.update());
  await expect
    .poll(() =>
      page.evaluate(async () => {
        const registration = await navigator.serviceWorker.getRegistration();
        return registration?.active && !registration.installing && !registration.waiting
          ? (await caches.keys()).length
          : -1;
      }),
    )
    .toBe(0);
  expect(await page.evaluate(async () => (await navigator.serviceWorker.getRegistration())?.scope)).toBe(before);

  // Nothing stored answers for the server: with it gone, the page does not
  // come from the phone.
  const pages = state.pages;
  await page.reload();
  await expect(page.getByRole("region", { name: "Needs you" })).toBeVisible();
  await shot(page, "test-results/update-after-update.png");
  expect(state.pages).toBe(pages + 1);
  await context.setOffline(true);
  await expect(page.reload()).rejects.toThrow(/ERR_INTERNET_DISCONNECTED/);
  await context.setOffline(false);
});

test("an app whose script fails to load once reloads by itself and starts", async ({ context }) => {
  const page = await context.newPage();
  await page.clock.install();
  state.refuseScript = 1;
  await page.goto(state.url);
  await expect(page.getByRole("region", { name: "Needs you" })).toHaveCount(0);
  await page.clock.runFor(10_000);
  await expect(page.getByRole("region", { name: "Needs you" })).toBeVisible();
  expect(state.pages).toBe(2);
});

test("an app that still does not start after one reload says so and how to reset, without looping", async ({
  context,
}) => {
  const page = await context.newPage();
  await page.clock.install();
  state.refuseScript = 100;
  await page.goto(state.url);
  await page.clock.runFor(10_000);
  await expect.poll(() => state.pages).toBe(2);
  await page.waitForLoadState();
  await page.clock.runFor(10_000);
  await expect(page.getByRole("alert")).toContainText("treetop did not start");
  await shot(page, "test-results/update-did-not-start.png");
  await page.clock.runFor(30_000);
  expect(state.pages).toBe(2);

  // Try again is a fresh start, with its one reload back.
  await page.getByRole("alert").getByRole("button", { name: "Try again" }).click();
  await expect.poll(() => state.pages).toBe(3);
});

test("an app that starts after it was said not to clears the notice", async ({ context }) => {
  // Real timers: a held script also holds the page's load events, which the
  // fake clock would race. Two of the watchdog's 8 seconds, then the start.
  test.setTimeout(60_000);
  const page = await context.newPage();
  state.refuseScript = 1;
  let release = () => {};
  state.holdScript = new Promise((resolve) => (release = resolve));
  await page.goto(state.url, { waitUntil: "commit" });
  await expect(page.getByRole("alert")).toContainText("treetop did not start", { timeout: 25_000 });
  expect(state.pages).toBe(2);
  release();
  await expect(page.getByRole("region", { name: "Needs you" })).toBeVisible();
  await expect(page.getByRole("alert")).toHaveCount(0);
});
