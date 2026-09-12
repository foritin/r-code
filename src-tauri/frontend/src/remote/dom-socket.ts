/**
 * R07/R21 — PWA（浏览器）的 socket 工厂：把 DOM WebSocket 的
 * addEventListener API 适配到 core transport 的 RemoteSocketFactory。
 * 该文件只允许在浏览器环境加载（RN 侧用 rn-socket.ts）。
 */

import type { RemoteSocket, RemoteSocketFactory } from "./transport.ts";

export const domSocketFactory: RemoteSocketFactory = {
  open(url, handlers): RemoteSocket {
    const socket = new WebSocket(url);
    socket.addEventListener("open", handlers.onOpen, { once: true });
    // DOM 的 error 后必跟 close；此处只负责握手/连接期的失败语义。
    socket.addEventListener("error", handlers.onError, { once: true });
    socket.addEventListener("message", (event: MessageEvent) => {
      handlers.onMessage(String(event.data));
    });
    socket.addEventListener("close", handlers.onClose, { once: true });
    return {
      send: (data: string) => socket.send(data),
      close: () => socket.close(),
    };
  },
};
