// Shows treetop serve's pushes and opens the tree a tapped one names. It
// keeps nothing and answers no requests, so every page and file comes from
// the server and a phone can never be left running a copy the server has
// moved on from. Earlier workers kept the app in a cache, which taking over
// deletes; the registration stays, and with it the phone's push
// subscription.

self.addEventListener("install", () => {
  void self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.map((key) => caches.delete(key))))
      .then(() => self.clients.claim()),
  );
});

// A push from treetop serve: a tree and what happened to it. Tapping it
// opens that tree's screen in the app, or the app if it is not open.
self.addEventListener("push", (event) => {
  let notice = { title: "treetop", body: "", url: "/" };
  try {
    notice = { ...notice, ...event.data.json() };
  } catch {
    // A push that is not treetop's own shape still shows, plainly.
  }
  event.waitUntil(
    self.registration.showNotification(notice.title, {
      body: notice.body,
      tag: notice.url,
      data: { url: notice.url },
      icon: "/icons/icon-192.png",
      badge: "/icons/icon-192.png",
    }),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const url = new URL(event.notification.data?.url ?? "/", self.location.origin).href;
  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((windows) => {
      const open = windows.find((w) => new URL(w.url).origin === self.location.origin);
      if (open) {
        return open.focus().then((w) => w.navigate(url));
      }
      return self.clients.openWindow(url);
    }),
  );
});
