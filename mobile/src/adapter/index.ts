/**
 * R21 — RN PlatformAdapter（骨架：接口对接 React Native 能力；R22/R25
 * 填充相机/推送实现）。令牌经安全存储（Keychain/Keystore），core 不感知。
 */
import type {
  PlatformAdapter,
  SecureStoreAdapter,
  CameraAdapter,
  NotificationAdapter,
  HapticsAdapter,
  TransportAdapter,
} from "../../../src-tauri/frontend/src/remote/core/platform-adapter.ts";
import { rnSocketFactory } from "./rn-socket.ts";

/** RN 安全存储骨架：v1 用 Keychain/Keystore 桥（R22 落地原生模块）。 */
class RNSecureStore implements SecureStoreAdapter {
  async get(key: string): Promise<string | null> {
    // R22: 经 react-native-keychain / Keystore 读取。骨架返回 null。
    void key;
    return null;
  }
  async set(key: string, value: string): Promise<void> {
    void key;
    void value;
  }
  async remove(key: string): Promise<void> {
    void key;
  }
}

class RNCamera implements CameraAdapter {
  async scanPairCode(): Promise<string> {
    // R22: react-native-vision-camera 扫码 → rcode://pair 载荷。
    throw new Error("camera scanning lands in R22");
  }
}

class RNNotifications implements NotificationAdapter {
  async permission(): Promise<"granted" | "denied" | "unset" | "unsupported"> {
    return "unset";
  }
  async show(title: string, body: string): Promise<void> {
    // R25: 前台通知走 native 模块；此处骨架 no-op。
    void title;
    void body;
  }
}

class RNHaptics implements HapticsAdapter {
  light(): void {
    // R23: Vibration / Reanimated haptics。
  }
}

class RNTransport implements TransportAdapter {
  async connect(config: { host: string; port: number; deviceId: string; token: string }) {
    // RN WebSocket 与 DOM WebSocket API 面不同（无 addEventListener），
    // 差异由 rnSocketFactory 吸收；帧协议由 core transport 冻结。
    const { RemoteConnection } = await import(
      "../../../src-tauri/frontend/src/remote/transport.ts"
    );
    return RemoteConnection.connect({
      ...config,
      openSocket: rnSocketFactory,
      // 远程侧的身份就是设备 id（F6：daemon 按 client_id 强制能力）。
      clientId: config.deviceId,
    });
  }
}

export function createPlatformAdapter(): PlatformAdapter {
  return {
    transport: new RNTransport(),
    secureStore: new RNSecureStore(),
    camera: new RNCamera(),
    notifications: new RNNotifications(),
    haptics: new RNHaptics(),
  };
}
