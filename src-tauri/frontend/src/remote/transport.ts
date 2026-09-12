/**
 * R07 remote PWA transport — 远端 daemon 的 WebSocket 客户端（与 Rust 侧
 * r-code-client/ws 同协议：architecture §4 hello → 命令/事件帧）。
 *
 * 本文件**零 DOM 依赖**：具体的 WebSocket 由平台 socket 工厂注入
 * （`RemoteSocketFactory`）。DOM 侧见 dom-socket.ts，React Native 侧见
 * mobile/src/adapter/rn-socket.ts —— RN 的 WebSocket 没有 addEventListener，
 * 早先直连 DOM API 的实现在真机上必然崩溃（从未被编译过所以没暴露）。
 */

import type { EventEnvelope } from "./core/projection.ts";

export interface RemoteConnectionConfig {
  host: string;
  port: number;
  deviceId: string;
  token: string;
  /** 平台 WebSocket 工厂：URL + 四个回调 → 可收发的可关闭套接字。 */
  openSocket: RemoteSocketFactory;
  /** 命令帧里的 client_id（默认沿用旧的 "pwa" 以保 PWA 行为不变）。 */
  clientId?: string;
}

/** 打开成功后的最小套接字面（send/close）。 */
export interface RemoteSocket {
  send(data: string): void;
  close(): void;
}

/** 平台差异（DOM addEventListener vs RN on* 属性）在此吸收。 */
export interface RemoteSocketFactory {
  open(
    url: string,
    handlers: {
      onOpen: () => void;
      onError: () => void;
      onMessage: (data: string) => void;
      onClose: () => void;
    },
  ): RemoteSocket;
}

interface Pending {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
}

export class RemoteConnection {
  private socket: RemoteSocket;
  private pending = new Map<string, Pending>();
  private eventListeners: ((events: EventEnvelope[]) => void)[] = [];
  private closedListeners: (() => void)[] = [];
  private nextId = 1;
  private welcomeLabels: string[] = [];
  private readonly clientId: string;

  private constructor(socket: RemoteSocket, clientId: string) {
    this.socket = socket;
    this.clientId = clientId;
  }

  /** welcome.capabilities 标签（daemon 权威能力集；R07b 投影输入）。 */
  async firstWelcomeCapabilities(): Promise<string[]> {
    return this.welcomeLabels;
  }

  static async connect(config: RemoteConnectionConfig): Promise<RemoteConnection> {
    const url = `wss://${config.host}:${config.port}/remote`;
    // welcome 握手的完成把手；阶段内 messages 先走 welcome 解析，
    // 握手成功后同一个 onMessage 通道交给常规帧处理。
    let settleWelcome!: (value: void | PromiseLike<void>) => void;
    let failWelcome!: (error: Error) => void;
    const welcomed = new Promise<void>((resolve, reject) => {
      settleWelcome = resolve;
      failWelcome = reject;
    });

    // 显式阶段标志：welcome 未完成时所有帧都按 welcome 解析。不能用
    // "是否已挂上帧处理器"来判断——那会在 welcome 帧到达时把它当常规帧
    // 丢掉（常规帧处理只认 command_id / 事件数组），连接永远等不到握手。
    let streaming = false;
    let connection: RemoteConnection | null = null;
    let closedSink: (() => void) | null = null;

    const socket = config.openSocket.open(url, {
      onOpen: () => {},
      onError: () => failWelcome(new Error("连接失败：无法到达主机")),
      onMessage: (data) => {
        if (streaming) {
          connection?.onFrame(data);
          return;
        }
        try {
          const welcome = JSON.parse(data) as {
            ok?: boolean;
            capabilities?: string[];
            error?: { code?: string };
          };
          if (welcome.ok === true) {
            if (connection) {
              connection.welcomeLabels = Array.isArray(welcome.capabilities)
                ? (welcome.capabilities as string[])
                : [];
            }
            streaming = true;
            settleWelcome();
          } else {
            failWelcome(new Error(`认证失败：${welcome.error?.code ?? "unauthorized"}`));
          }
        } catch {
          failWelcome(new Error("协议错误：欢迎帧不可解析"));
        }
      },
      onClose: () => closedSink?.(),
    });

    connection = new RemoteConnection(socket, config.clientId ?? "pwa");
    closedSink = () =>
      connection?.closedListeners.forEach((fn) => fn());

    socket.send(
      JSON.stringify({
        hello: "r-code-remote/1",
        device_id: config.deviceId,
        token: config.token,
      client_id: connection.clientId,
    }),
    );
    await welcomed;
    return connection;
  }

  private onFrame(data: string) {
    let frame: unknown;
    try {
      frame = JSON.parse(data);
    } catch {
      return;
    }
    // ApplicationFrame::Events(Vec<EventEnvelope>) serializes (untagged)
    // as a bare array.
    if (Array.isArray(frame)) {
      this.eventListeners.forEach((listener) => listener(frame as EventEnvelope[]));
      return;
    }
    if (frame !== null && typeof frame === "object" && "command_id" in frame) {
      const result = frame as {
        command_id: string;
        outcome?: { Ok?: unknown; Err?: string };
      };
      const pending = this.pending.get(result.command_id);
      if (!pending) return;
      this.pending.delete(result.command_id);
      const outcome = result.outcome;
      if (outcome !== null && typeof outcome === "object" && "Err" in (outcome ?? {})) {
        pending.reject(new Error(String((outcome as { Err: string }).Err)));
      } else {
        pending.resolve((outcome as { Ok: unknown } | undefined)?.Ok ?? null);
      }
    }
  }

  onEvents(listener: (events: EventEnvelope[]) => void) {
    this.eventListeners.push(listener);
  }

  onClose(listener: () => void) {
    this.closedListeners.push(listener);
  }

  /** 执行一条命令（command_id 幂等重放语义 F7；daemon 侧强制 device 身份）。 */
  async call<T = unknown>(method: string, params: Record<string, unknown>): Promise<T> {
    const commandId = `${this.clientId}-${Date.now().toString(36)}-${this.nextId++}`;
    const payload = JSON.stringify({
      client_id: this.clientId,
      command_id: commandId,
      method,
      params,
    });
    const answer = new Promise<unknown>((resolve, reject) => {
      this.pending.set(commandId, { resolve, reject });
      setTimeout(() => {
        if (this.pending.delete(commandId)) {
          reject(new Error("命令超时"));
        }
      }, 30_000);
    });
    this.socket.send(payload);
    return answer as Promise<T>;
  }

  /** 订阅事件流（先补齐 after_seq 历史，再推增量；F8）。 */
  subscribe(afterSeq: number) {
    this.socket.send(JSON.stringify({ after_seq: afterSeq }));
  }

  close() {
    this.socket.close();
  }
}
