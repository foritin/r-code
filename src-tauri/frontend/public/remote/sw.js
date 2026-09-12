/* R07 service worker — 只缓存壳（F10：任务数据永不落缓存）。
   缓存名单：HTML 壳 + 带 hash 的静态资源；/events、数据帧 network-only。 */
const SHELL_CACHE = "r-code-remote-shell-v1";

self.addEventListener("install", (event) => {
  event.waitUntil(caches.open(SHELL_CACHE).then((cache) => cache.addAll(["/app/"])));
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((key) => key !== SHELL_CACHE).map((key) => caches.delete(key))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);
  // WebSocket 升级与数据接口绝不拦截；同源静态资源 cache-first（构建产物带 hash）。
  if (event.request.method !== "GET" || url.protocol !== "https:") return;
  event.respondWith(
    caches.match(event.request).then((hit) => hit || fetch(event.request)),
  );
});
