/**
 * R21/R26 — React Native 的 socket 工厂：RN 的 WebSocket 只有 on* 属性，
 * 没有 DOM 的 addEventListener（R21 初版直连 DOM API，在真机上会抛
 * `addEventListener is not a function`）。这里把差异吸收掉，
 * 上层的帧协议与命令语义仍完全复用 core transport。
 */

import type {
  RemoteSocket,
  RemoteSocketFactory,
} from "../../../src-tauri/frontend/src/remote/transport.ts";

type SocketHandlers = Parameters<RemoteSocketFactory["open"]>[1];

export const rnSocketFactory: RemoteSocketFactory = {
  open(url, handlers): RemoteSocket {
    const socket = new WebSocket(url);
    socket.onopen = () => handlers.onOpen();
    socket.onerror = () => handlers.onError();
    socket.onmessage = (event) => handlers.onMessage(String(event.data));
    socket.onclose = () => handlers.onClose();
    return {
      send: (data: string) => socket.send(data),
      close: () => socket.close(),
    };
  },
};

/** 离线可用的 socket：jest 里驱动生命周期与帧，不触网。 */
export interface FakeSocket {
  sent: string[];
  closed: boolean;
  handlers: SocketHandlers | null;
  openConnection(): void;
  push(data: string): void;
  fail(): void;
  drop(): void;
}

export function fakeSocketFactory(): RemoteSocketFactory & { last: FakeSocket | null } {
  const factory = {
    last: null as FakeSocket | null,
    open(_url: string, handlers: SocketHandlers): RemoteSocket {
      const fake: FakeSocket = {
        sent: [],
        closed: false,
        handlers: null,
        openConnection: () => handlers.onOpen(),
        push: (data: string) => handlers.onMessage(data),
        fail: () => handlers.onError(),
        drop: () => handlers.onClose(),
      };
      fake.handlers = handlers;
      factory.last = fake;
      return {
        send: (data: string) => fake.sent.push(data),
        close: () => {
          fake.closed = true;
        },
      };
    },
  };
  return factory;
}
