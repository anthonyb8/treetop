// ANB-20. Turning notifications on from the pool subscribes this phone with
// the server's key and tells the server where to push; refused in Settings,
// the bell says so instead of asking again.
import { expect, test, type Page } from "@playwright/test";
import { pool, serve } from "./fixtures";

const subscription = {
  endpoint: "https://web.push.apple.com/QGuQ",
  keys: { p256dh: "BOJ0nwggt5", auth: "6Ew7POPkn" },
};

/** Stands in for the home-screen app's push support, which the test browser lacks. */
async function phone(page: Page, permission: NotificationPermission, answer: NotificationPermission) {
  await page.addInitScript(
    ({ permission, answer, subscription }) => {
      let current = permission;
      let subscribedWith: string | null = null;
      class FakeNotification {
        static get permission() {
          return current;
        }
        static requestPermission() {
          current = answer;
          return Promise.resolve(answer);
        }
      }
      Object.defineProperty(window, "Notification", { value: FakeNotification });
      Object.defineProperty(window, "PushManager", { value: class {} });
      const registration = {
        pushManager: {
          getSubscription: () => Promise.resolve(null),
          subscribe: (options: { applicationServerKey: Uint8Array }) => {
            subscribedWith = btoa(String.fromCharCode(...options.applicationServerKey));
            (window as unknown as { subscribedWith: string | null }).subscribedWith = subscribedWith;
            return Promise.resolve({ toJSON: () => subscription });
          },
        },
      };
      Object.defineProperty(navigator, "serviceWorker", {
        value: { ready: Promise.resolve(registration), register: () => Promise.resolve(registration) },
      });
    },
    { permission, answer, subscription },
  );
}

test("the bell turns notifications on and the server learns where to push", async ({ page }) => {
  await serve(page, () => pool);
  await phone(page, "default", "granted");
  await page.route("**/api/push/key", (route) => route.fulfill({ json: { key: "AQID" } }));
  let told: unknown = null;
  await page.route("**/api/push/subscribe", (route) => {
    told = route.request().postDataJSON();
    return route.fulfill({ json: {} });
  });
  await page.goto("/");
  await page.screenshot({ path: "test-results/push-bell.png" });
  await page.getByRole("button", { name: "Turn on notifications" }).click();
  await expect(page.getByRole("button", { name: "Turn on notifications" })).toHaveCount(0);
  expect(told).toEqual(subscription);
  expect(await page.evaluate(() => (window as unknown as { subscribedWith: string }).subscribedWith)).toBe("AQID");
});

test("notifications refused in Settings show as blocked, not as a question", async ({ page }) => {
  await serve(page, () => pool);
  await phone(page, "denied", "denied");
  await page.goto("/");
  await expect(page.getByRole("button", { name: "Notifications are blocked in Settings" })).toBeDisabled();
  await expect(page.getByRole("button", { name: "Turn on notifications" })).toHaveCount(0);
});

test("a phone that allowed them before is told to the server again on open", async ({ page }) => {
  await serve(page, () => pool);
  await phone(page, "granted", "granted");
  await page.route("**/api/push/key", (route) => route.fulfill({ json: { key: "AQID" } }));
  let told = 0;
  await page.route("**/api/push/subscribe", (route) => {
    told += 1;
    return route.fulfill({ json: {} });
  });
  await page.goto("/");
  await expect.poll(() => told).toBe(1);
  await expect(page.getByRole("button", { name: /notifications/i })).toHaveCount(0);
});
