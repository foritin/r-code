/**
 * Room 时间线 —— sessionMessages 历史 + onAgentEvent 流式增量。
 * 每个条目带 data-t（相对会话起点秒数）；cur（胶片播放头）之前的正常、之后的调暗。
 * 流式窗口打开期间不做历史重建（delta 未落盘，重建会丢流）；run 结束（state 事件）后重建。
 */
import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";
import { agentResend, onAgentEvent as listenAgentEvent, sessionMessages } from "../../lib/ipc";
import type { AgentEvent, AgentSendMode } from "../../lib/types";
import { useTasksStore } from "../../store/tasks";
import { toolVerb } from "../../lib/format";
import { applyAgentEvent, buildTimeline, mergeRunItems, type TimelineItem } from "./model";

export interface TimelineHandle {
  /** 发送失败（消息可能已落盘）→ 以持久化历史重建。 */
  reload: () => void;
  /** 发送成功：立即本地追加用户气泡，稍后由持久化历史收敛。 */
  onSent: (text: string, mode: AgentSendMode) => void;
}

interface Props {
  taskId: string;
  /** 播放头秒数；null = live */
  cur: number | null;
  /** 运行期间不能改写历史分支，避免上下文竞争。 */
  running: boolean;
  /** 透传流式事件给 Room 的可观察活动 reducer。 */
  onAgentEvent?: (event: AgentEvent) => void;
}

/** 工具调用先于网关建立待审批记录到达，故立刻刷新并做两次短延迟兜底。 */
function mayCreatePermission(event: AgentEvent): boolean {
  if (event.type === "tool_call") return true;
  if (event.type === "activity") return event.phase === "waiting_permission";
  return event.type === "scoped" && mayCreatePermission(event.event);
}

function userSendModeLabel(mode: AgentSendMode): string | null {
  switch (mode) {
    case "auto":
      return null;
    case "steer":
      return "引导";
    case "queue":
      return "排队";
    case "send_now":
      return "立即发送";
  }
}

function queuedStateLabel(state: "queued" | "dispatching" | "failed"): string {
  switch (state) {
    case "queued":
      return "已排队，尚未送达";
    case "dispatching":
      return "正在交给新运行";
    case "failed":
      return "排队发送失败";
  }
}

export const Timeline = forwardRef<TimelineHandle, Props>(function Timeline(
  { taskId, cur, running, onAgentEvent },
  ref
) {
  const [items, setItems] = useState<TimelineItem[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [showSubagentRuns, setShowSubagentRuns] = useState(false);
  const [editingMessageId, setEditingMessageId] = useState<string | null>(null);
  const [editingText, setEditingText] = useState("");
  const [editError, setEditError] = useState<string | null>(null);
  const [resending, setResending] = useState(false);
  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const eventsLen = useTasksStore((s) =>
    s.details[taskId]?.task.id === taskId ? s.details[taskId].events.length : 0
  );
  const runsStamp = useTasksStore((s) =>
    [...(s.details[taskId]?.runs ?? [])]
      .sort((a, b) => a.started_at.localeCompare(b.started_at) || a.id.localeCompare(b.id))
      .map(
        (run) =>
          `${run.id}:${run.started_at}:${run.ended_at ?? ""}:${run.review_state}:${run.model}:${run.agent_kind}:${run.agent_label ?? ""}:${run.summary ?? ""}`
      )
      .join("|")
  );

  const liveRef = useRef(false);
  const idRef = useRef(0);
  const startRef = useRef(Date.now());
  const scrollRef = useRef<HTMLDivElement>(null);
  const editRef = useRef<HTMLTextAreaElement>(null);
  const pinnedRef = useRef(true);

  const nid = useCallback(() => `live-${++idRef.current}`, []);
  const nowSec = useCallback(() => Math.max(0, (Date.now() - startRef.current) / 1000), []);

  const reload = useCallback(async () => {
    try {
      const msgs = await sessionMessages(taskId);
      const d = useTasksStore.getState().details[taskId];
      const startIso =
        msgs.find((m) => m.kind === "meta")?.timestamp ??
        d?.task.created_at ??
        new Date().toISOString();
      const parsed = Date.parse(startIso);
      startRef.current = Number.isNaN(parsed) ? Date.now() : parsed;
      setItems(
        buildTimeline(
          msgs,
          d?.events ?? [],
          d?.runs ?? [],
          startIso,
          d?.queued_messages ?? []
        )
      );
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, [taskId]);

  // 任务切换：重置并加载历史
  useEffect(() => {
    liveRef.current = false;
    idRef.current = 0;
    setItems([]);
    setError(null);
    setShowSubagentRuns(false);
    setEditingMessageId(null);
    setEditingText("");
    setEditError(null);
    setResending(false);
    void reload();
  }, [reload]);

  useEffect(() => {
    if (editingMessageId) {
      requestAnimationFrame(() => editRef.current?.focus());
    }
  }, [editingMessageId]);

  // 新运行开始后不能把原分支继续暴露为可编辑状态。
  useEffect(() => {
    if (running && editingMessageId) {
      setEditingMessageId(null);
      setEditingText("");
      setEditError(null);
    }
  }, [running, editingMessageId]);

  // 事件或运行快照变化：非流式重建；流式期间只同步 AgentRun，避免覆盖尚未落盘的增量。
  useEffect(() => {
    const detail = useTasksStore.getState().details[taskId];
    if (liveRef.current) {
      if (detail) {
        setItems((prev) => mergeRunItems(prev, detail.runs, startRef.current));
      }
      return;
    }
    void reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [eventsLen, runsStamp, taskId]);

  // 流式事件订阅
  useEffect(() => {
    let dead = false;
    let un: (() => void) | undefined;
    const refreshTimers = new Set<ReturnType<typeof window.setTimeout>>();
    const refreshPermissionSoon = () => {
      void refreshDetail(taskId);
      for (const delay of [180, 650]) {
        const timer = window.setTimeout(() => {
          refreshTimers.delete(timer);
          void refreshDetail(taskId);
        }, delay);
        refreshTimers.add(timer);
      }
    };
    listenAgentEvent((tid, ev) => {
      if (tid !== taskId) return;
      onAgentEvent?.(ev);
      if (mayCreatePermission(ev)) refreshPermissionSoon();
      if (
        ev.type === "scoped" &&
        ev.event.type === "subagent_lifecycle"
      ) {
        void refreshDetail(taskId);
      }
      if (ev.type === "state") {
        liveRef.current = false;
        void refreshDetail(taskId);
        void reload();
        return;
      }
      liveRef.current = true;
      setItems((prev) => applyAgentEvent(prev, ev, nowSec(), nid));
    })
      .then((u) => {
        if (dead) u();
        else un = u;
      })
      .catch(() => {});
    return () => {
      dead = true;
      un?.();
      refreshTimers.forEach((timer) => window.clearTimeout(timer));
    };
  }, [taskId, refreshDetail, reload, nowSec, nid, onAgentEvent]);

  useImperativeHandle(
    ref,
    () => ({
      reload: () => void reload(),
      onSent: (text, mode) => {
        setItems((prev) => [
          ...prev,
          {
            kind: "you",
            id: nid(),
            t: nowSec(),
            text,
            sendMode: mode,
            queuedState: mode === "queue" || mode === "send_now" ? "queued" : undefined,
          },
        ]);
      },
    }),
    [reload, nid, nowSec]
  );

  // live 且贴底时自动滚底
  useEffect(() => {
    const el = scrollRef.current;
    if (el && pinnedRef.current && cur == null) el.scrollTop = el.scrollHeight;
  }, [items, cur]);

  const dim = (t: number) => (cur != null && t > cur ? " dimmed" : "");
  const cancelEdit = useCallback(() => {
    if (resending) return;
    setEditingMessageId(null);
    setEditingText("");
    setEditError(null);
  }, [resending]);
  const beginEdit = useCallback((messageId: string, text: string) => {
    if (running || resending) return;
    setEditingMessageId(messageId);
    setEditingText(text);
    setEditError(null);
  }, [running, resending]);
  const resendEdited = useCallback(async () => {
    const messageId = editingMessageId;
    const message = editingText.trim();
    if (!messageId || !message || running || resending) return;
    setResending(true);
    setEditError(null);
    try {
      await agentResend(taskId, messageId, message);
      setEditingMessageId(null);
      setEditingText("");
      await refreshDetail(taskId);
      await reload();
    } catch (cause) {
      setEditError(String(cause));
    } finally {
      setResending(false);
    }
  }, [editingMessageId, editingText, running, resending, taskId, refreshDetail, reload]);
  const subagentRunCount = items.filter(
    (item) => item.kind === "run" && item.agentKind === "subagent"
  ).length;
  const visibleItems = showSubagentRuns
    ? items
    : items.filter((item) => item.kind !== "run" || item.agentKind !== "subagent");

  return (
    <div
      className="timeline"
      ref={scrollRef}
      onScroll={(e) => {
        const el = e.currentTarget;
        pinnedRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 60;
      }}
    >
      {error && <div className="tl-error">时间线加载失败:{error}</div>}
      {!error && items.length === 0 && (
        <div className="empty">
          还没有对话。
          <br />
          在下方输入第一句话,运行中再发即为 steer。
        </div>
      )}
      {subagentRunCount > 0 && (
        <button
          type="button"
          className="timeline-subagent-toggle"
          onClick={() => setShowSubagentRuns((visible) => !visible)}
          aria-expanded={showSubagentRuns}
        >
          子代理运行 · {subagentRunCount}
          <span>{showSubagentRuns ? "收起" : "展开"}</span>
        </button>
      )}
      {visibleItems.map((it) => {
        switch (it.kind) {
          case "ms":
            return (
              <div className={"ms" + dim(it.t)} data-t={it.t} key={it.id}>
                {it.ok === true && <span className="ok">✓</span>}
                {it.ok === false && <span className="bad">✗</span>}
                {it.label}
              </div>
            );
          case "you":
            {
              const modeLabel = userSendModeLabel(it.sendMode);
              const queueState =
                it.queuedState === "queued" ||
                it.queuedState === "dispatching" ||
                it.queuedState === "failed"
                  ? it.queuedState
                  : null;
              const editing = Boolean(it.messageId && editingMessageId === it.messageId);
            return (
              <div
                className={
                  "you" +
                  (modeLabel ? ` user-mode-${it.sendMode}` : "") +
                  (queueState ? " user-message-queued" : "") +
                  (editing ? " editing" : "") +
                  dim(it.t)
                }
                data-t={it.t}
                key={it.id}
              >
                <div className="who">
                  YOU
                  {modeLabel && <span className="user-send-mode">{modeLabel}</span>}
                  {it.messageId && !running && !editing && (
                    <button
                      type="button"
                      className="message-edit"
                      aria-label="编辑此消息"
                      title="编辑此消息"
                      onClick={() => beginEdit(it.messageId!, it.text)}
                    >
                      ✎
                    </button>
                  )}
                </div>
                {editing ? (
                  <div className="message-inline-edit">
                    <textarea
                      ref={editRef}
                      value={editingText}
                      disabled={resending}
                      aria-label="编辑历史消息"
                      onChange={(event) => setEditingText(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key === "Escape") {
                          event.preventDefault();
                          cancelEdit();
                        } else if (
                          event.key === "Enter" &&
                          !event.shiftKey &&
                          (event.ctrlKey || event.metaKey)
                        ) {
                          event.preventDefault();
                          void resendEdited();
                        }
                      }}
                    />
                    <div className="message-inline-actions">
                      <span>Ctrl/⌘+Enter 发送 · Esc 取消</span>
                      <button type="button" className="quiet-link" disabled={resending} onClick={cancelEdit}>
                        取消
                      </button>
                      <button
                        type="button"
                        className="btn accent sm"
                        disabled={resending || !editingText.trim()}
                        onClick={() => void resendEdited()}
                      >
                        {resending ? "重发中…" : "发送"}
                      </button>
                    </div>
                    {editError && <div className="message-inline-error" role="alert">重发失败：{editError}</div>}
                  </div>
                ) : (
                  it.text
                )}
                {queueState && <div className={`user-queue-state state-${queueState}`}>{queuedStateLabel(queueState)}</div>}
              </div>
            );
            }
          case "agent":
            return (
              <div className={"agent" + dim(it.t)} data-t={it.t} key={it.id}>
                <div className="who">R-CODE</div>
                {it.text}
                {it.streaming && <span className="caret" />}
              </div>
            );
          case "plan": {
            const firstTodo = it.steps.findIndex((s) => !s.completed);
            return (
              <div className={"plan-card" + dim(it.t)} data-t={it.t} key={it.id}>
                <div className="head">
                  Plan — {it.steps.length} steps
                  {firstTodo === -1 && <span className="ok">✓ all done</span>}
                </div>
                <ol>
                  {it.steps.map((s, i) => (
                    <li key={i} className={s.completed ? "done" : i === firstTodo ? "now" : ""}>
                      <b>{s.completed ? "✓" : i + 1}</b>
                      {s.description}
                    </li>
                  ))}
                </ol>
              </div>
            );
          }
          case "run":
            return (
              <div
                className={"run-row " + it.state + (it.agentKind === "subagent" ? " subagent" : "") + dim(it.t)}
                data-t={it.t}
                key={it.id}
                title={`Run · ${it.model} · ${it.label}`}
              >
                {it.state === "active" && <span className="spin" />}
                <span className="run-name">{it.agentKind === "subagent" ? "子代理" : "运行"}</span>
                <span className="run-model">
                  {it.agentLabel || (it.agentKind === "subagent" ? "只读调查" : it.model || "agent")}
                </span>
                <span className="run-status">{it.label}</span>
                {it.agentKind === "subagent" && it.agentSummary && (
                  <span className="run-summary" title={it.agentSummary}>
                    {it.agentSummary}
                  </span>
                )}
              </div>
            );
          case "tool":
            return (
              <div
                className={"trow" + (it.state === "active" ? " active" : "") + dim(it.t)}
                data-t={it.t}
                key={it.id}
                title={it.summary || undefined}
              >
                {it.state === "active" && <span className="spin" />}
                <span className="verb">{toolVerb(it.name)}</span>
                <span className="target">{it.target || it.name}</span>
                {it.state === "ok" && <span className="ok">✓ {it.summary}</span>}
                {it.state === "fail" && <span className="fail">✗ {it.summary}</span>}
              </div>
            );
        }
      })}
    </div>
  );
});
