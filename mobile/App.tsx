/**
 * R21/R26 — RN App 骨架 + 屏④设置/诊断：任务/审批/设置三 tab 导航
 * （对照原型屏①②③④），消费共享 TS core（projection / connection-state /
 * capability-ui / connection-strategy / settings-diagnostics）与
 * PlatformAdapter。
 *
 * 数据面随 R22 的原生安全存储（Keychain/Keystore）落地后点亮——本屏只读
 * 真实状态，未配对时诚实显示未配对，不臆造数据（R07/F14）。
 */
import React from "react";
import { ScrollView, StyleSheet, Text, TouchableOpacity, View } from "react-native";
import { createPlatformAdapter } from "./src/adapter/index.ts";
import { SettingsScreen } from "./src/screens/SettingsScreen.tsx";
import { capabilitiesFromLabels } from "../src-tauri/frontend/src/remote/core/capability-ui.ts";
import {
  initialSnapshot,
  stateBanner,
  transition,
  type ConnectionSnapshot,
} from "../src-tauri/frontend/src/remote/core/connection-state.ts";
import { planConnection } from "../src-tauri/frontend/src/remote/core/connection-strategy.ts";
import {
  appendLog,
  settingsModel,
  type ReconnectLogEntry,
  type SettingsAction,
  type SettingsInput,
} from "../src-tauri/frontend/src/remote/core/settings-diagnostics.ts";

const adapter = createPlatformAdapter();

/** 凭据键（只经安全存储读写；设置屏永不展示令牌）。 */
const CREDENTIAL_KEYS = ["device.token", "device.capabilities", "host.fingerprint"] as const;
/** 端点缓存键（清除令牌时一并清空）。 */
const ENDPOINT_KEYS = ["lan.host", "lan.port", "relay.url"] as const;

const APP_VERSION = "1.0.1";

type Tab = "tasks" | "approvals" | "settings";

interface Endpoints {
  lanHost: string | null;
  lanPort: number | null;
  relayUrl: string | null;
}

const NO_ENDPOINTS: Endpoints = { lanHost: null, lanPort: null, relayUrl: null };

const styles = StyleSheet.create({
  container: { flex: 1, backgroundColor: "#181818", paddingTop: 48 },
  title: { color: "#eeeeee", fontSize: 19, fontWeight: "600", padding: 16 },
  banner: { color: "#f4742b", fontSize: 13, paddingHorizontal: 16, paddingBottom: 4 },
  body: { flex: 1, padding: 16 },
  row: { color: "#b4b4b4", fontSize: 14, paddingVertical: 8 },
  nav: {
    flexDirection: "row",
    justifyContent: "space-around",
    borderTopWidth: StyleSheet.hairlineWidth,
    borderTopColor: "#343434",
    paddingVertical: 8,
    minHeight: 44,
  },
  tabText: { color: "#b4b4b4", fontSize: 15 },
  tabTextActive: { color: "#f4742b", fontWeight: "600" },
});

function App(): React.JSX.Element {
  const [tab, setTab] = React.useState<Tab>("tasks");
  const [snapshot, setSnapshot] = React.useState<ConnectionSnapshot>(initialSnapshot);
  const [capabilities, setCapabilities] = React.useState(capabilitiesFromLabels([]));
  const [endpoints, setEndpoints] = React.useState<Endpoints>(NO_ENDPOINTS);
  const [forceRelay, setForceRelay] = React.useState(false);
  const [log, setLog] = React.useState<ReconnectLogEntry[]>([]);
  const [fingerprint, setFingerprint] = React.useState<string | null>(null);

  // 从安全存储恢复真实状态（R22 的 Keychain/Keystore 落地后即点亮）。
  React.useEffect(() => {
    void (async () => {
      const [token, storedCaps, storedFingerprint, lanHost, lanPort, relayUrl] = await Promise.all([
        adapter.secureStore.get("device.token"),
        adapter.secureStore.get("device.capabilities"),
        adapter.secureStore.get("host.fingerprint"),
        adapter.secureStore.get("lan.host"),
        adapter.secureStore.get("lan.port"),
        adapter.secureStore.get("relay.url"),
      ]);
      const port = Number(lanPort);
      setCapabilities(capabilitiesFromLabels((storedCaps ?? "").split(",").filter(Boolean)));
      setFingerprint(storedFingerprint);
      setEndpoints({
        lanHost,
        lanPort: Number.isInteger(port) && port > 0 ? port : null,
        relayUrl,
      });
      setSnapshot((current) =>
        transition(current, { type: "credentialsChanged", hasCredentials: token !== null }),
      );
    })();
  }, []);

  function handleAction(action: SettingsAction): void {
    switch (action.kind) {
      case "copy":
        // 展开后的文本由 RN `selectable` 提供系统长按复制，这里只给触感反馈。
        adapter.haptics.light();
        return;
      case "switch-strategy":
        setForceRelay(action.forceRelay);
        setLog((current) =>
          appendLog(current, {
            atMs: Date.now(),
            event: "retry",
            detail: action.forceRelay ? "切换到仅中继" : "切换到直连优先",
          }),
        );
        return;
      case "clear-token":
        void (async () => {
          for (const key of CREDENTIAL_KEYS) {
            await adapter.secureStore.remove(key);
          }
          for (const key of ENDPOINT_KEYS) {
            await adapter.secureStore.remove(key);
          }
          setEndpoints(NO_ENDPOINTS);
          setForceRelay(false);
          setCapabilities(capabilitiesFromLabels([]));
          setFingerprint(null);
          // 本地注销 = 回到未配对态；电脑端吊销在其设备管理里做。
          setSnapshot((current) =>
            transition(current, { type: "credentialsChanged", hasCredentials: false }),
          );
          setLog((current) =>
            appendLog(current, {
              atMs: Date.now(),
              event: "denied",
              detail: "本机令牌已清除",
            }),
          );
        })();
        return;
    }
  }

  const input: SettingsInput = {
    capabilities,
    snapshot,
    plan: planConnection({ ...endpoints, forceRelay }),
    relayUrl: endpoints.relayUrl,
    forceRelay,
    log,
    fingerprint,
    rttMs: null,
    deviceName: null,
    pairedAtMs: null,
    version: APP_VERSION,
  };

  return (
    <View style={styles.container}>
      <Text style={styles.title}>R-Code Remote</Text>
      <Text style={styles.banner}>{stateBanner(snapshot)}</Text>
      {tab === "settings" ? (
        <SettingsScreen sections={settingsModel(input)} onAction={handleAction} />
      ) : (
        <ScrollView style={styles.body}>
          <Text style={styles.row}>
            {tab === "tasks" ? "任务列表（R22/R23 接通真实数据）" : "待审批聚合（R24 接通）"}
          </Text>
        </ScrollView>
      )}
      <View style={styles.nav}>
        {(["tasks", "approvals", "settings"] as const).map((key) => (
          <TouchableOpacity
            key={key}
            onPress={() => setTab(key)}
            style={{ minHeight: 44, paddingHorizontal: 16 }}
          >
            <Text style={[styles.tabText, tab === key && styles.tabTextActive]}>{key}</Text>
          </TouchableOpacity>
        ))}
      </View>
    </View>
  );
}

export default App;
