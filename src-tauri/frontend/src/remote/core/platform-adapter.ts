/**
 * R21 — PlatformAdapter 冻结接口（F13/F14）：平台专属能力的注入点。
 * core 模块（同目录其余文件）零平台 import；PWA 用 DOM adapter（transport.ts），
 * RN 用 mobile/src/adapter（本仓库 mobile/ 骨架）。接口形状两端共用。
 */

/** 传输：连接远端 daemon 并收发应用帧（PWA=DOM WebSocket，RN=React Native
 * WebSocket，桌面=r-code-client）。握手/帧语义由 r-code-client 与 relay
 * 协议冻结（architecture §4 + relay-interface）。 */
export interface TransportAdapter {
  connect(config: {
    host: string;
    port: number;
    deviceId: string;
    token: string;
  }): Promise<TransportConnection>;
}

export interface TransportConnection {
  call<T = unknown>(method: string, params: Record<string, unknown>): Promise<T>;
  subscribe(afterSeq: number): void;
  onEvents(listener: (events: unknown[]) => void): void;
  onClose(listener: () => void): void;
  close(): void;
}

/** 安全存储：令牌/指纹只进系统安全区（R22：Keychain / Keystore）。 */
export interface SecureStoreAdapter {
  get(key: string): Promise<string | null>;
  set(key: string, value: string): Promise<void>;
  remove(key: string): Promise<void>;
}

/** 相机扫码（R22）：解析 rcode://pair 载荷。 */
export interface CameraAdapter {
  scanPairCode(): Promise<string>;
}

/** 通知（R13/R25）：系统能力检测 + 前台横幅降级由 core 投影。 */
export interface NotificationAdapter {
  permission(): Promise<"granted" | "denied" | "unset" | "unsupported">;
  show(title: string, body: string): Promise<void>;
}

/** 触感反馈（原生体验细节）。 */
export interface HapticsAdapter {
  light(): void;
}

/** 平台装配点：PWA 与 RN 各自提供实现；core 消费此接口，不反向依赖。 */
export interface PlatformAdapter {
  transport: TransportAdapter;
  secureStore: SecureStoreAdapter;
  camera: CameraAdapter;
  notifications: NotificationAdapter;
  haptics: HapticsAdapter;
}
