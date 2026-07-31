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
  useMemo,
  useRef,
  useState,
} from "react";
import { agentResend, onAgentEvent as listenAgentEvent, sessionMessages } from "../../lib/ipc";
import type { AgentEvent, AgentSendMode, SessionAttachmentMeta } from "../../lib/types";
import { useTasksStore } from "../../store/tasks";
import { IconAttach, IconChevronDown, IconChevronRight } from "../icons";
import { parseWorkflowInvocation } from "../../lib/slash-commands";
import { applyAgentEvent, buildTimeline, mergeRunItems, type TimelineItem } from "./model";
import { Markdown } from "./Markdown";
import {
  TimelineContextEvent,
  TimelineSubagentGroup,
  TimelineToolGroup,
} from "./TimelineActivity";
import {
  buildTimelineTurns,
  type TimelineDisplayItem,
  type TimelineRunItem,
  type TimelineUserItem,
} from "./timeline-presentation";

export interface TimelineHandle {
  /** 发送失败（消息可能已落盘）→ 以持久化历史重建。 */
  reload: () => void;
  /** 发送成功：立即本地追加用户气泡，稍后由持久化历史收敛。 */
  onSent: (text: string, mode: AgentSendMode, attachments?: SessionAttachmentMeta[]) => void;
}

interface Props {
  taskId: string;
  workspacePath: string | null;
  /** 播放头秒数；null = live */
  cur: number | null;
  /** 运行期间不能改写历史分支，避免上下文竞争。 */
  running: boolean;
  /** 透传流式事件给 Room 的可观察活动 reducer。 */
  onAgentEvent?: (event: AgentEvent) => void;
  /** 点击内联子代理芯片后在任务工作台打开公开运行详情。 */
  onInspectSubagent?: (runId: string) => void;
  selectedSubagentId?: string | null;
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

function runRuntimeLabel(kind: "native" | "codex_exec" | "codex_mcp"): string {
  if (kind === "codex_exec") return "Codex CLI";
  if (kind === "codex_mcp") return "Codex MCP";
  return "R-Code Agent";
}

function runTimeLabel(value: string): string {
  const timestamp = Date.parse(value);
  if (Number.isNaN(timestamp)) return "—";
  return new Date(timestamp).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function runUsageLabel(value: string | null): string | null {
  if (!value) return null;
  try {
    const usage = JSON.parse(value) as Record<string, unknown>;
    const input = typeof usage.input_tokens === "number" ? usage.input_tokens : null;
    const output = typeof usage.output_tokens === "number" ? usage.output_tokens : null;
    if (input == null && output == null) return null;
    return [
      input == null ? null : `输入 ${input.toLocaleString()}`,
      output == null ? null : `输出 ${output.toLocaleString()}`,
    ].filter(Boolean).join(" · ");
  } catch {
    return null;
  }
}

function runDurationLabel(startedAt: string, endedAt: string | null): string {
  const start = Date.parse(startedAt);
  const end = endedAt ? Date.parse(endedAt) : Date.now();
  if (Number.isNaN(start) || Number.isNaN(end)) return "";
  const seconds = Math.max(0, Math.floor((end - start) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  if (minutes < 60) return `${minutes}m ${String(rest).padStart(2, "0")}s`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${String(minutes % 60).padStart(2, "0")}m`;
}

export const Timeline = forwardRef<TimelineHandle, Props>(function Timeline(
  { taskId, workspacePath, cur, running, onAgentEvent, onInspectSubagent, selectedSubagentId },
  ref
) {
  const [items, setItems] = useState<TimelineItem[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [editingMessageId, setEditingMessageId] = useState<string | null>(null);
  const [editingText, setEditingText] = useState("");
  const [editError, setEditError] = useState<string | null>(null);
  const [resending, setResending] = useState(false);
  const [expandedRunIds, setExpandedRunIds] = useState<Set<string>>(() => new Set());
  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const eventsLen = useTasksStore((s) =>
    s.details[taskId]?.task.id === taskId ? s.details[taskId].events.length : 0
  );
  const runsStamp = useTasksStore((s) =>
    [...(s.details[taskId]?.runs ?? [])]
      .sort((a, b) => a.started_at.localeCompare(b.started_at) || a.id.localeCompare(b.id))
      .map(
        (run) =>
          `${run.id}:${run.started_at}:${run.ended_at ?? ""}:${run.review_state}:${run.model}:${run.agent_kind}:${run.agent_label ?? ""}:${run.access_mode}:${run.routing_reason ?? ""}:${run.summary ?? ""}`
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
    setEditingMessageId(null);
    setEditingText("");
    setEditError(null);
    setResending(false);
    setExpandedRunIds(new Set());
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
      onSent: (text, mode, attachments = []) => {
        setItems((prev) => [
          ...prev,
          {
            kind: "you",
            id: nid(),
            t: nowSec(),
            text,
            imageCount: 0,
            imageMediaTypes: [],
            attachments,
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
  const toggleRun = useCallback((runId: string) => {
    setExpandedRunIds((current) => {
      const next = new Set(current);
      if (next.has(runId)) next.delete(runId);
      else next.add(runId);
      return next;
    });
  }, []);
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
  const turns = useMemo(() => buildTimelineTurns(items), [items]);
  type RenderableItem = TimelineUserItem | TimelineRunItem | TimelineDisplayItem;

  const renderTimelineItem = (it: RenderableItem, finalResponse = false) => {
    switch (it.kind) {
      case "ms":
        return (
          <div className={"ms" + dim(it.t)} data-t={it.t} key={it.id}>
            {it.ok === true && <span className="ok">✓</span>}
            {it.ok === false && <span className="bad">✗</span>}
            {it.label}
          </div>
        );
      case "context":
        return (
          <TimelineContextEvent
            key={it.id}
            t={it.t}
            label={it.label}
            detail={it.detail}
            dim={dim(it.t)}
          />
        );
      case "you": {
        const workflow = parseWorkflowInvocation(it.text);
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
              <span className="message-author">YOU</span>
              {modeLabel && <span className="user-send-mode">{modeLabel}</span>}
              {it.messageId && !running && !editing && !workflow && it.imageCount === 0 && (
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
            ) : workflow ? (
              <div className="workflow-invocation">
                <span>内置工作流</span>
                <strong>/{workflow.name}</strong>
                {workflow.args && <small>{workflow.args}</small>}
              </div>
            ) : it.text ? (
              it.text
            ) : null}
            {it.attachments.length > 0 && (
              <div className="message-attachment-summary" aria-label={`${it.attachments.length} 个附件`}>
                {it.attachments.map((attachment, index) => (
                  <span
                    className={`message-attachment-item kind-${attachment.kind}`}
                    title={`${attachment.name} · ${attachment.media_type}`}
                    key={`${attachment.name}-${index}`}
                  >
                    <IconAttach width={13} height={13} aria-hidden="true" />
                    {attachment.name}
                  </span>
                ))}
              </div>
            )}
            {queueState && <div className={`user-queue-state state-${queueState}`}>{queuedStateLabel(queueState)}</div>}
          </div>
        );
      }
      case "agent":
        return (
          <div
            className={`agent${finalResponse ? " timeline-final-response" : ""}${dim(it.t)}`}
            data-t={it.t}
            key={it.id}
          >
            <div className="who">R-CODE</div>
            <Markdown text={it.text} streaming={it.streaming} taskId={taskId} workspacePath={workspacePath} />
          </div>
        );
      case "plan": {
        const firstTodo = it.steps.findIndex((step) => !step.completed);
        return (
          <div className={"plan-card" + dim(it.t)} data-t={it.t} key={it.id}>
            <div className="head">
              计划 · {it.steps.length} 步
              {firstTodo === -1 && <span className="ok">✓ 全部完成</span>}
            </div>
            <ol>
              {it.steps.map((step, index) => (
                <li key={index} className={step.completed ? "done" : index === firstTodo ? "now" : ""}>
                  <b>{step.completed ? "✓" : index + 1}</b>
                  {step.description}
                </li>
              ))}
            </ol>
          </div>
        );
      }
      case "run": {
        const expanded = expandedRunIds.has(it.id);
        const detailId = `${it.id}-details`;
        const usage = runUsageLabel(it.usageJson);
        const duration = runDurationLabel(it.startedAt, it.endedAt);
        const abnormal = it.state === "failed" || it.state === "aborted";
        return (
          <div
            className={`run-disclosure run-summary ${it.state}${dim(it.t)}`}
            data-t={it.t}
            key={it.id}
          >
            <button
              type="button"
              className="run-row ring-inset"
              aria-expanded={expanded}
              aria-controls={detailId}
              title={expanded ? "收起本轮运行详情" : "展开本轮运行详情"}
              onClick={() => toggleRun(it.id)}
            >
              <span className={`run-summary-mark${it.state === "active" ? " active" : ""}`} aria-hidden="true" />
              <span className="run-name">{it.state === "active" ? "处理中" : "已处理"}</span>
              {duration && <span className="run-duration">{duration}</span>}
              {abnormal && <span className="run-status">{it.label}</span>}
              <span className="run-chevron" aria-hidden="true">
                {expanded ? <IconChevronDown width={13} height={13} /> : <IconChevronRight width={13} height={13} />}
              </span>
            </button>
            {expanded && (
              <div className="run-detail" id={detailId}>
                <div className="run-detail-meta">
                  <span><b>模型</b>{it.model || "默认"}</span>
                  <span><b>运行时</b>{runRuntimeLabel(it.runtimeKind)}</span>
                  <span><b>开始</b>{runTimeLabel(it.startedAt)}</span>
                  {it.endedAt && <span><b>结束</b>{runTimeLabel(it.endedAt)}</span>}
                  {usage && <span><b>用量</b>{usage} tokens</span>}
                </div>
                {it.agentSummary && (
                  <div className="run-detail-summary">
                    <b>{it.state === "active" ? "当前工作" : "结果摘要"}</b>
                    <p>{it.agentSummary}</p>
                  </div>
                )}
              </div>
            )}
          </div>
        );
      }
      case "tool_group":
        return <TimelineToolGroup key={it.id} item={it} dim={dim(it.t)} />;
      case "subagent_group":
        return (
          <TimelineSubagentGroup
            key={it.id}
            item={it}
            selectedSubagentId={selectedSubagentId}
            onInspectSubagent={onInspectSubagent}
            dim={dim(it.t)}
          />
        );
    }
  };

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
      {turns.map((turn) => {
        const lastActivity = turn.items.reduce(
          (last, item, index) =>
            item.kind === "tool_group" || item.kind === "subagent_group" || item.kind === "context"
              ? index
              : last,
          -1
        );
        const finalResponseIndex = turn.hasActivity
          ? turn.items.findIndex((item, index) => index > lastActivity && item.kind === "agent")
          : -1;
        return (
          <section className={`timeline-turn${turn.hasActivity ? " has-activity" : ""}`} key={turn.id}>
            {turn.user && renderTimelineItem(turn.user)}
            {turn.runs.length > 0 && (
              <div className="timeline-run-summaries">
                {turn.runs.map((run) => renderTimelineItem(run))}
              </div>
            )}
            <div className={`timeline-turn-trace${turn.hasActivity ? " has-activity" : ""}`}>
              {turn.items.map((item, index) =>
                renderTimelineItem(item, index === finalResponseIndex)
              )}
            </div>
          </section>
        );
      })}
    </div>
  );
});
