/**
 * R07b remote PWA — 四屏交互版：连接屏 → 任务/审批/设置三 tab；
 * 会话屏（事件流 + 输入/中止）；断网全屏态 + 自动重连倒计时；
 * 能力投影控制按钮显隐（daemon 侧仍强制，F6）。
 * 视觉沿用桌面签名皮肤 obsidian（#181818 / #f4742b；F11），安全区 inset。
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  applyEvent,
  emptyProjection,
  type SessionProjection,
  type SessionRow,
} from "./remote/core/projection.ts";
import { RemoteConnection, type RemoteConnectionConfig } from "./remote/transport.ts";
import { domSocketFactory } from "./remote/dom-socket.ts";
import {
  initialSnapshot,
  stateBanner,
  transition,
  type ConnectionSnapshot,
} from "./remote/core/connection-state.ts";
import {
  approvalCardUi,
  capabilitiesFromLabels,
  composerUi,
  deviceInfoUi,
  type DeviceCapabilities,
} from "./remote/core/capability-ui.ts";
import {
  applyApprovalEvent,
  emptyAggregation,
  mergePendingList,
  optimisticRemove,
  type AggregationState,
} from "./remote/core/approvals-aggregate.ts";
import {
  notificationForEvent,
  notificationSupport,
  bodyIsSanitized,
} from "./remote/core/notifications.ts";
import { guidanceForError } from "./remote/core/error-guidance.ts";

const C = {
  bg: "#181818",
  card: "#262626",
  chip: "#303030",
  border: "#343434",
  fg: "#eeeeee",
  muted: "#b4b4b4",
  accent: "#f4742b",
  ok: "#69c798",
  danger: "#df6b62",
  inset: "#141414",
};

const tabHit: React.CSSProperties = {
  minHeight: 44, // 命中区 ≥44px（R07b.A2；iOS HIG 下限）
  padding: "0 16px",
  background: "none",
  border: "none",
  color: C.muted,
  fontSize: 15,
  cursor: "pointer",
};

const textInput: React.CSSProperties = {
  width: "100%",
  background: C.inset,
  border: `1px solid ${C.border}`,
  borderRadius: 8,
  color: C.fg,
  padding: "10px 12px",
  fontSize: 15,
  boxSizing: "border-box",
  minHeight: 44,
};

interface TaskRow {
  taskId: string;
  title: string;
  state: string;
  running: boolean;
}

interface PendingApprovalRow {
  opId: string;
  summary: string;
  taskId: string;
  ageMs?: number;
}

/** 断网/拒绝/未配对全屏态（状态机投影）。 */
function ConnectionOverlay({
  snapshot,
  onRepair,
}: {
  snapshot: ConnectionSnapshot;
  onRepair: () => void;
}) {
  if (snapshot.state === "online" || snapshot.state === "connecting") return null;
  const fatal = snapshot.state === "denied" || snapshot.state === "unpaired";
  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(13,13,13,.92)",
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        gap: 12,
        padding: 24,
        zIndex: 30,
      }}
    >
      <p style={{ color: fatal ? C.danger : C.fg, fontSize: 16 }}>{stateBanner(snapshot)}</p>
      {fatal ? (
        <button
          type="button"
          onClick={onRepair}
          style={{
            ...textInput,
            width: "auto",
            padding: "10px 20px",
            color: C.accent,
            background: "none",
            borderColor: C.accent,
          }}
        >
          清除本机凭据
        </button>
      ) : null}
    </div>
  );
}

/** 审批卡（屏③聚合行；能力决定按钮显隐）。 */
function ApprovalCard({
  row,
  capabilities,
  onDecide,
}: {
  row: PendingApprovalRow;
  capabilities: DeviceCapabilities;
  onDecide: (opId: string, approve: boolean) => void;
}) {
  const ui = approvalCardUi(capabilities);
  return (
    <div
      style={{
        background: C.card,
        border: `1px solid ${C.border}`,
        borderRadius: 12,
        padding: 14,
        marginBottom: 10,
      }}
    >
      <div style={{ display: "flex", justifyContent: "space-between", gap: 8 }}>
        <span style={{ fontSize: 15, color: C.fg, wordBreak: "break-word" }}>
          {row.summary || row.opId}
        </span>
        {ui.readOnlyBadge ? (
          <span style={{ color: C.muted, fontSize: 12, whiteSpace: "nowrap" }}>
            {ui.readOnlyBadge}
          </span>
        ) : null}
      </div>
      <p style={{ color: C.muted, fontSize: 12, margin: "4px 0 10px" }}>任务 {row.taskId}</p>
      {ui.showDecideButtons ? (
        <div style={{ display: "flex", gap: 8 }}>
          <button
            type="button"
            onClick={() => onDecide(row.opId, true)}
            style={{
              ...textInput,
              width: "auto",
              flex: 1,
              background: C.accent,
              color: "#181818",
              fontWeight: 600,
              cursor: "pointer",
            }}
          >
            批准
          </button>
          <button
            type="button"
            onClick={() => onDecide(row.opId, false)}
            style={{
              ...textInput,
              width: "auto",
              flex: 1,
              background: "none",
              color: C.danger,
              borderColor: C.danger,
              cursor: "pointer",
            }}
          >
            拒绝
          </button>
        </div>
      ) : null}
    </div>
  );
}

function SessionScreen({
  conn,
  taskId,
  capabilities,
  onBack,
}: {
  conn: RemoteConnection;
  taskId: string;
  capabilities: DeviceCapabilities;
  onBack: () => void;
}) {
  const [projection, setProjection] = useState<SessionProjection>(emptyProjection());
  const composer = composerUi(capabilities);
  const [text, setText] = useState("");
  const [note, setNote] = useState("");
  const cursorRef = useRef(0);

  useEffect(() => {
    let disposed = false;
    conn.onEvents((events) => {
      if (disposed) return;
      const mine = events.filter((event) => event.task_id === taskId);
      if (mine.length === 0) return;
      cursorRef.current = Math.max(cursorRef.current, ...mine.map((event) => event.seq));
      setProjection((state) => mine.reduce((acc, event) => applyEvent(acc, event), state));
    });
    conn.subscribe(0);
    return () => {
      disposed = true;
    };
  }, [conn, taskId]);

  const send = async () => {
    if (!text.trim() || !composer.canSend) return;
    try {
      const outcome = await conn.call<{ queued?: boolean }>("task.sendMessage", {
        taskId,
        text: text.trim(),
      });
      setText("");
      setNote(outcome?.queued ? "已排队（当前 run 结束后发送）" : "已发送");
    } catch (failure) {
      setNote(failure instanceof Error ? failure.message : String(failure));
    }
  };

  const abort = async () => {
    try {
      await conn.call("task.cancel", { taskId });
      setNote("已请求中止");
    } catch (failure) {
      setNote(failure instanceof Error ? failure.message : String(failure));
    }
  };

  const rowStyleFor = (row: SessionRow): React.CSSProperties => {
    const base: React.CSSProperties = {
      fontSize: 14,
      lineHeight: 1.55,
      padding: "8px 12px",
      borderRadius: 8,
      marginBottom: 6,
      whiteSpace: "pre-wrap",
      wordBreak: "break-word",
    };
    switch (row.type) {
      case "user":
        return { ...base, background: C.chip };
      case "assistant":
        return base;
      case "tool":
        return {
          ...base,
          color: row.ok ? C.muted : C.danger,
          fontFamily: "'Cascadia Code', Consolas, monospace",
          fontSize: 13,
          background: "#202020",
        };
      case "state":
        return {
          ...base,
          color: row.tone === "fail" ? C.danger : row.tone === "ok" ? C.ok : C.muted,
          fontSize: 12,
          padding: "2px 12px",
        };
    }
  };

  const rowText = (row: SessionRow): string => {
    switch (row.type) {
      case "user":
        return `你：${row.text}`;
      case "assistant":
        return row.text;
      case "tool":
        return `$ ${row.name}${row.ok ? "" : " ✗"}`;
      case "state":
        return row.label;
    }
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100dvh" }}>
      <header
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          padding: "10px 12px",
          borderBottom: `1px solid ${C.border}`,
          position: "sticky",
          top: 0,
          background: C.bg,
        }}
      >
        <button type="button" onClick={onBack} style={{ ...tabHit, color: C.accent }}>
          ← 任务
        </button>
        <span style={{ fontSize: 14, flex: 1, overflow: "hidden", textOverflow: "ellipsis" }}>
          {taskId}
        </span>
      </header>
      <main style={{ flex: 1, overflowY: "auto", padding: 12, overflowX: "hidden" }}>
        {projection.rows.map((row, index) => (
          <div key={index} style={rowStyleFor(row)}>
            {rowText(row)}
          </div>
        ))}
        {projection.pendingApproval ? (
          <ApprovalCard
            row={{
              opId: projection.pendingApproval.opId,
              summary: projection.pendingApproval.summary,
              taskId,
              ageMs: 0,
            }}
            capabilities={capabilities}
            onDecide={(opId, approve) => {
              void conn
                .call("approvals.decide", {
                  operationId: opId,
                  decision: approve ? "granted" : "denied",
                })
                .catch((failure: unknown) =>
                  setNote(failure instanceof Error ? failure.message : String(failure)),
                );
            }}
          />
        ) : null}
        {note ? <p style={{ color: C.muted, fontSize: 12 }}>{note}</p> : null}
      </main>
      <div
        style={{
          borderTop: `1px solid ${C.border}`,
          padding: "10px 12px calc(10px + env(safe-area-inset-bottom))",
        }}
      >
        <div style={{ display: "flex", gap: 8 }}>
          <input
            style={textInput}
            placeholder={composer.placeholder}
            value={text}
            disabled={!composer.canSend}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.nativeEvent.isComposing) void send();
            }}
          />
          {projection.running && composer.canAbort ? (
            <button
              type="button"
              onClick={abort}
              style={{ ...textInput, width: "auto", color: C.danger, cursor: "pointer" }}
            >
              中止
            </button>
          ) : null}
          <button
            type="button"
            disabled={!composer.canSend || !text.trim()}
            onClick={send}
            style={{
              ...textInput,
              width: "auto",
              background: composer.canSend ? C.accent : C.chip,
              color: composer.canSend ? "#181818" : C.muted,
              fontWeight: 600,
              cursor: "pointer",
            }}
          >
            发送
          </button>
        </div>
      </div>
    </div>
  );
}

type Tab = "tasks" | "approvals" | "settings";

function HomeScreen({
  conn,
  capabilities,
  onOpenTask,
  onLost,
}: {
  conn: RemoteConnection;
  capabilities: DeviceCapabilities;
  onOpenTask: (taskId: string) => void;
  onLost: () => void;
}) {
  const [tab, setTab] = useState<Tab>("tasks");
  const [tasks, setTasks] = useState<TaskRow[]>([]);
  const [note, setNote] = useState("");
  // R07c：审批聚合 = approvals.list 快照 + 事件流合并（同一模块，测试与
  // UI 共用）；乐观移除失败时列表回滚。
  const [aggregate, setAggregate] = useState<AggregationState>(emptyAggregation);

  const refreshTasks = async () => {
    try {
      const list = await conn.call<TaskRow[]>("task.list", {});
      setTasks(Array.isArray(list) ? list : []);
    } catch {
      onLost();
    }
  };

  const refreshApprovals = async () => {
    try {
      const answer = await conn.call<{ pending: Array<Record<string, unknown>> }>(
        "approvals.list",
        {},
      );
      setAggregate((state) => mergePendingList(state, answer?.pending ?? []));
    } catch {
      /* 无 events:read 时聚合为空（默认设备有） */
    }
  };

  useEffect(() => {
    void refreshTasks();
    void refreshApprovals();
    conn.onEvents((events) => {
      setAggregate((state) =>
        events.reduce((acc, event) => applyApprovalEvent(acc, event), state),
      );
      void refreshTasks();
    });
    const timer = setInterval(() => void refreshTasks(), 3000);
    return () => clearInterval(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [conn]);

  const decide = async (opId: string, approve: boolean) => {
    setAggregate((state) => optimisticRemove(state, opId)); // 乐观移除
    try {
      await conn.call("approvals.decide", {
        operationId: opId,
        decision: approve ? "granted" : "denied",
      });
    } catch (failure) {
      setNote(failure instanceof Error ? failure.message : String(failure));
      void refreshApprovals(); // 失败回滚（同一聚合实现）
    }
  };

  const info = deviceInfoUi(capabilities);

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100dvh" }}>
      <main style={{ flex: 1, overflowY: "auto", padding: 12, overflowX: "hidden" }}>
        {tab === "tasks" ? (
          tasks.length === 0 ? (
            <p style={{ color: C.muted, fontSize: 14, padding: 8 }}>
              暂无任务（在电脑上创建会话后出现在这里）
            </p>
          ) : (
            tasks.map((task) => (
              <button
                key={task.taskId}
                type="button"
                onClick={() => onOpenTask(task.taskId)}
                style={{
                  display: "block",
                  width: "100%",
                  textAlign: "left",
                  background: C.card,
                  border: `1px solid ${C.border}`,
                  borderRadius: 12,
                  padding: "12px 14px",
                  marginBottom: 8,
                  cursor: "pointer",
                  color: C.fg,
                  minHeight: 44,
                }}
              >
                <div
                  style={{
                    display: "flex",
                    justifyContent: "space-between",
                    alignItems: "center",
                  }}
                >
                  <span
                    style={{
                      fontSize: 15,
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                    }}
                  >
                    {task.title || task.taskId}
                  </span>
                  {task.running ? (
                    <span style={{ color: C.accent, fontSize: 12 }}>运行中</span>
                  ) : null}
                </div>
                <span style={{ fontSize: 12, color: C.muted }}>{task.state}</span>
              </button>
            ))
          )
        ) : tab === "approvals" ? (
          aggregate.pending.length === 0 ? (
            <p style={{ color: C.muted, fontSize: 14, padding: 8 }}>没有待审批的操作</p>
          ) : (
            aggregate.pending.map((row) => (
              <ApprovalCard
                key={row.opId}
                row={row}
                capabilities={capabilities}
                onDecide={(opId, approve) => void decide(opId, approve)}
              />
            ))
          )
        ) : (
          <div
            style={{
              background: C.card,
              border: `1px solid ${C.border}`,
              borderRadius: 12,
              padding: 14,
            }}
          >
            {info.map((row) => (
              <div key={row.label} style={{ marginBottom: 8 }}>
                <p style={{ color: C.muted, fontSize: 12 }}>{row.label}</p>
                <p style={{ fontSize: 14 }}>{row.value}</p>
              </div>
            ))}
            <p style={{ color: C.muted, fontSize: 12 }}>能力在电脑端授予；本页只读。</p>
          </div>
        )}
        {note ? <p style={{ color: C.danger, fontSize: 12, padding: 8 }}>{note}</p> : null}
      </main>
      <nav
        style={{
          display: "flex",
          justifyContent: "space-around",
          borderTop: `1px solid ${C.border}`,
          paddingBottom: "calc(8px + env(safe-area-inset-bottom))",
          paddingTop: 8,
          background: C.bg,
        }}
      >
        {(
          [
            ["tasks", `任务`],
            ["approvals", `审批${aggregate.pending.length > 0 ? ` (${aggregate.pending.length})` : ""}`],
            ["settings", `设置`],
          ] as Array<[Tab, string]>
        ).map(([key, label]) => (
          <button
            key={key}
            type="button"
            onClick={() => setTab(key)}
            style={{
              ...tabHit,
              color: tab === key ? C.accent : C.muted,
              fontWeight: tab === key ? 600 : 400,
            }}
          >
            {label}
          </button>
        ))}
      </nav>
    </div>
  );
}

function ConnectScreen({
  onConnected,
}: {
  onConnected: (conn: RemoteConnection, capabilities: DeviceCapabilities) => void;
}) {
  const [host, setHost] = useState("");
  const [port, setPort] = useState("");
  const [deviceId, setDeviceId] = useState("");
  const [token, setToken] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    const saved = localStorage.getItem("r-code-remote-connection");
    if (saved) {
      try {
        const parsed = JSON.parse(saved) as Partial<RemoteConnectionConfig>;
        setHost(parsed.host ?? "");
        setPort(parsed.port ? String(parsed.port) : "");
        setDeviceId(parsed.deviceId ?? "");
        setToken(parsed.token ?? "");
      } catch {
        /* 本地状态损坏则忽略 */
      }
    }
  }, []);

  const connect = async () => {
    setBusy(true);
    setError("");
    try {
      const config: RemoteConnectionConfig = {
        host: host.trim(),
        port: Number(port),
        deviceId: deviceId.trim(),
        token: token.trim(),
        // 浏览器端注入 DOM WebSocket 工厂（transport 保持平台无关）。
        openSocket: domSocketFactory,
      };
      const conn = await RemoteConnection.connect(config);
      localStorage.setItem(
        "r-code-remote-connection",
        JSON.stringify({
          host: config.host,
          port: config.port,
          deviceId: config.deviceId,
          token: config.token,
        }),
      );
      const labels = await conn.firstWelcomeCapabilities();
      onConnected(conn, capabilitiesFromLabels(labels));
    } catch (failure) {
      const raw = failure instanceof Error ? failure.message : String(failure);
      const guidance = guidanceForError(raw);
      setError(`${guidance.headline}——${guidance.detail}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      style={{
        padding: 24,
        maxWidth: 420,
        margin: "0 auto",
        minHeight: "100dvh",
        display: "flex",
        flexDirection: "column",
        justifyContent: "center",
      }}
    >
      <h1 style={{ fontSize: 19, marginBottom: 4 }}>R-Code Remote</h1>
      <p style={{ color: C.muted, fontSize: 13, marginBottom: 20 }}>
        连接你的电脑（扫码配对在后续版本；手动输入与扫码等效）
      </p>
      <input
        style={textInput}
        placeholder="主机（如 192.168.1.10）"
        value={host}
        onChange={(e) => setHost(e.target.value)}
      />
      <input
        style={{ ...textInput, marginTop: 10 }}
        placeholder="端口"
        inputMode="numeric"
        value={port}
        onChange={(e) => setPort(e.target.value)}
      />
      <input
        style={{ ...textInput, marginTop: 10 }}
        placeholder="设备 ID"
        value={deviceId}
        onChange={(e) => setDeviceId(e.target.value)}
      />
      <input
        style={{ ...textInput, marginTop: 10 }}
        placeholder="设备令牌"
        type="password"
        value={token}
        onChange={(e) => setToken(e.target.value)}
      />
      {error ? <p style={{ color: C.danger, fontSize: 13, margin: "10px 0" }}>{error}</p> : null}
      <button
        type="button"
        disabled={busy || !host || !port || !deviceId || !token}
        onClick={connect}
        style={{
          ...textInput,
          marginTop: 14,
          background: C.accent,
          color: "#181818",
          fontWeight: 600,
          cursor: "pointer",
        }}
      >
        {busy ? "连接中…" : "连接"}
      </button>
    </div>
  );
}

export function RemoteApp() {
  const [conn, setConn] = useState<RemoteConnection | null>(null);
  const [capabilities, setCapabilities] = useState<DeviceCapabilities | null>(null);
  const [taskId, setTaskId] = useState<string | null>(null);
  const [snapshot, setSnapshot] = useState<ConnectionSnapshot>(initialSnapshot);

  const [banner, setBanner] = useState("");
  const support = notificationSupport(
    typeof Notification !== "undefined",
    typeof Notification !== "undefined" ? Notification.permission : null,
  );

  // 断网全屏态：close 事件驱动状态机（自动重连倒计时投影）。
  useEffect(() => {
    if (!conn) return;
    conn.onClose(() => {
      setSnapshot((snap) => transition(snap, { type: "disconnected" }));
    });
    const timer = setInterval(() => {
      setSnapshot((snap) => transition(snap, { type: "tick", nowSeconds: Date.now() / 1000 }));
    }, 1000);
    return () => clearInterval(timer);
  }, [conn]);

  // R13：前台通知——approval.requested / run.completed 触发；正文通用
  // （F15），无系统能力时应用内横幅降级。
  useEffect(() => {
    if (!conn) return;
    conn.onEvents((events) => {
      for (const event of events) {
        const content = notificationForEvent(event);
        if (!content || !bodyIsSanitized(content.body)) continue;
        if (support.canUseSystemNotifications) {
          try {
            void Notification.requestPermission().then((permission) => {
              if (permission === "granted") {
                new Notification(content.title, { body: content.body });
              } else {
                setBanner(support.fallbackBanner);
              }
            });
          } catch {
            setBanner(support.fallbackBanner);
          }
        } else if (support.fallbackBanner) {
          setBanner(support.fallbackBanner);
        }
      }
    });
  }, [conn, support]);

  const body = useMemo(() => {
    if (!conn || !capabilities) {
      return (
        <ConnectScreen
          onConnected={(connection, caps) => {
            setConn(connection);
            setCapabilities(caps);
            setSnapshot(transition(initialSnapshot, { type: "connected" }));
          }}
        />
      );
    }
    if (taskId) {
      return (
        <SessionScreen
          conn={conn}
          taskId={taskId}
          capabilities={capabilities}
          onBack={() => setTaskId(null)}
        />
      );
    }
    return (
      <HomeScreen
        conn={conn}
        capabilities={capabilities}
        onOpenTask={setTaskId}
        onLost={() => setSnapshot(transition(snapshot, { type: "disconnected" }))}
      />
    );
  }, [conn, capabilities, taskId, snapshot]);

  return (
    <div
      style={{
        background: C.bg,
        color: C.fg,
        minHeight: "100dvh",
        overflowX: "hidden", // R07b.A2：390×844 无横向滚动
        fontFamily:
          "'Segoe UI Variable Text','PingFang SC','Microsoft YaHei UI',sans-serif",
      }}
    >
      {body}
      {banner ? (
        <div
          style={{
            position: "fixed",
            left: 12,
            right: 12,
            bottom: "calc(72px + env(safe-area-inset-bottom))",
            background: C.card,
            border: `1px solid ${C.border}`,
            borderRadius: 8,
            padding: "10px 12px",
            fontSize: 13,
            color: C.muted,
            zIndex: 25,
          }}
          onClick={() => setBanner("")}
        >
          {banner}
        </div>
      ) : null}
      <ConnectionOverlay
        snapshot={snapshot}
        onRepair={() => {
          localStorage.removeItem("r-code-remote-connection");
          setConn(null);
          setCapabilities(null);
          setSnapshot(
            transition(initialSnapshot, {
              type: "credentialsChanged",
              hasCredentials: false,
            }),
          );
        }}
      />
    </div>
  );
}

const container = document.getElementById("remote-root");
if (container) {
  createRoot(container).render(<RemoteApp />);
  if ("serviceWorker" in navigator && window.location.protocol === "https:") {
    window.addEventListener("load", () => {
      void navigator.serviceWorker.register("/remote/sw.js");
    });
  }
}
