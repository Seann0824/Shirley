// Shirley's service worker. It does two things only:
// - makes the page installable as an app (Chrome needs a registered worker for that on Android);
// - shows reply notifications and brings the app back to the right conversation when one is
//   clicked. Android Chrome can only show notifications through a worker.
// No caching: Shirley is useless offline, and a stale bundle is worse than a network error.

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const bot = event.notification.data && event.notification.data.bot;
  event.waitUntil(
    (async () => {
      const windows = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
      for (const client of windows) {
        if ("focus" in client) {
          client.postMessage({ type: "open-bot", bot });
          return client.focus();
        }
      }
      return self.clients.openWindow(bot ? `/?bot=${encodeURIComponent(bot)}` : "/");
    })(),
  );
});
