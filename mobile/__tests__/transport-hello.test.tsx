/**
 * R21/R26 — RN transport 的凭据与身份断言：hello 帧带 device_id 与 token，
 * 且 client_id 必须是设备 id（F6：daemon 按 client_id 强制能力；早先
 * 硬编码 "pwa"，会让服务端把 RN 设备当成 PWA）。全程离线（fake socket）。
 */
import { fakeSocketFactory } from "../src/adapter/rn-socket.ts";
import { RemoteConnection } from "../../src-tauri/frontend/src/remote/transport.ts";

test("R21: hello 帧带设备身份，client_id = deviceId（不是 'pwa'）", async () => {
  const factory = fakeSocketFactory();
  const connecting = RemoteConnection.connect({
    host: "192.168.1.10",
    port: 8787,
    deviceId: "dev-42",
    token: "tok-abc",
    openSocket: factory,
    clientId: "dev-42",
  });

  // open 之前不会发帧；socket 打开后 hello 立即发出。
  factory.last.openConnection();
  factory.last.push(JSON.stringify({ ok: true, capabilities: ["events-read"] }));
  const connection = await connecting;

  const hello = JSON.parse(factory.last.sent[0]);
  expect(hello.hello).toBe("r-code-remote/1");
  expect(hello.device_id).toBe("dev-42");
  expect(hello.token).toBe("tok-abc");
  expect(hello.client_id).toBe("dev-42");

  // 能力来自 welcome（daemon 权威），不是本地臆造。
  await expect(connection.firstWelcomeCapabilities()).resolves.toEqual(["events-read"]);
});

test("R21: 认证失败（ok:false）时连接被拒绝且给出错误码", async () => {
  const factory = fakeSocketFactory();
  const connecting = RemoteConnection.connect({
    host: "h",
    port: 1,
    deviceId: "dev-1",
    token: "bad",
    openSocket: factory,
    clientId: "dev-1",
  });
  factory.last.openConnection();
  factory.last.push(JSON.stringify({ ok: false, error: { code: "unauthorized" } }));
  await expect(connecting).rejects.toThrow(/unauthorized|认证失败/);
});
