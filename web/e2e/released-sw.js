// The service worker treetop serve shipped up to 0.1.68, kept as it was so
// update.spec.ts can put it on a test phone and then update it (ANB-52).

// Keeps the app's own files on the phone. Pages come from the network
// first, so a new treetop is picked up on the next open, and from the cache
// when the server cannot be reached. Built files are named for their
// contents, so a kept one never goes stale. The API is never kept here:
// the app keeps the last pool itself and says when it is showing it.

const CACHE = "treetop-v1";

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(CACHE)
      .then((cache) => cache.addAll(["/", "/manifest.webmanifest", "/icons/icon-192.png"]))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const request = event.request;
  const url = new URL(request.url);
  if (request.method !== "GET" || url.origin !== self.location.origin || url.pathname.startsWith("/api/")) {
    return;
  }
  if (request.mode === "navigate") {
    event.respondWith(
      fetch(request)
        .then((response) => {
          if (response.ok) {
            const copy = response.clone();
            void caches.open(CACHE).then((cache) => cache.put("/", copy));
          }
          return response;
        })
        .catch(() => caches.match("/").then((cached) => cached ?? Response.error())),
    );
    return;
  }
  event.respondWith(
    caches.match(request).then(
      (cached) =>
        cached ??
        fetch(request).then((response) => {
          if (response.ok) {
            const copy = response.clone();
            void caches.open(CACHE).then((cache) => cache.put(request, copy));
          }
          return response;
        }),
    ),
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
