/**
 * Room 时间线 —— sessionMessages 历史 + onAgentEvent 流式增量。
 * 每个条目带 data-t（相对会话起点秒数）；cur（胶片播放头）之前的正常、之后的调暗。
 * 流式窗口打开期间不做历史重建（delta 未落盘，重建会丢流）；run 结束（state 事件）后重建。
 */
import {
  forwardRef,
  memo,
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  agentResend,
  onAgentEvent as listenAgentEvent,
  sessionMessages,
  sessionMessagesForBranch,
} from "../../lib/ipc";
import type { AgentEvent, AgentSendMode, SessionAttachmentMeta } from "../../lib/types";
import { useTasksStore } from "../../store/tasks";
import { IconAttach, IconChevronDown, IconChevronRight } from "../icons";
import { parseWorkflowInvocation } from "../../lib/slash-commands";
import { useSharedNow } from "../../lib/shared-clock";
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
  /** Historical branch to render read-only; null/undefined keeps the live active branch. */
  branchId?: string | null;
  workspacePath: string | null;
  /** 播放头秒数；null = live */
  cur: number | null;
  /** 运行期间不能改写历史分支，避免上下文竞争。 */
  running: boolean;
  /** 质量门禁仍在检查最新草稿；此时可见文本尚不是正式交付。 */
  reviewing: boolean;
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

/**
 * 解析 run 的 usage_json（后端键为 snake_case）并格式化为用量文案。
 * 缓存命中字段：cache_read_tokens（命中）/ cache_write_tokens（未命中，DeepSeek
 * prompt_cache_hit/miss_tokens 归一后的键名）。两者都显式存在时展示命中率百分比；
 * 任一缺失时只展示命中 token 数（比例未知，不臆造）；两者都缺失时保持原有
 * 输入/输出 行为完全不变。导出仅供前端回归测试直接断言。
 */
export function runUsageLabel(value: string | null): string | null {
  if (!value) return null;
  try {
    const usage = JSON.parse(value) as Record<string, unknown>;
    const input = typeof usage.input_tokens === "number" ? usage.input_tokens : null;
    const output = typeof usage.output_tokens === "number" ? usage.output_tokens : null;
    const cacheRead = typeof usage.cache_read_tokens === "number" ? usage.cache_read_tokens : null;
    const cacheWrite = typeof usage.cache_write_tokens === "number" ? usage.cache_write_tokens : null;
    if (input == null && output == null && cacheRead == null && cacheWrite == null) return null;
    const parts = [
      input == null ? null : `输入 ${input.toLocaleString()}`,
      output == null ? null : `输出 ${output.toLocaleString()}`,
    ];
    if (cacheRead != null || cacheWrite != null) {
      const read = cacheRead ?? 0;
      const readText = `命中 ${read.toLocaleString()}`;
      if (cacheRead != null && cacheWrite != null) {
        const total = read + cacheWrite;
        const ratio = total > 0 ? Math.round((read / total) * 100) : null;
        parts.push(ratio != null ? `${readText} (${ratio}%)` : readText);
      } else {
        parts.push(readText);
      }
    }
    return parts.filter(Boolean).join(" · ");
  } catch {
    return null;
  }
}

/**
 * 解析 run 的 usage_json 中的流重放计数（stream_retries 键，P1-E §8：agent 层
 * 冻结请求重放的 run 级累计次数，对齐 Reasonix RequestAttemptCounter）。
 * 仅在 >0 时返回「重试 N 次」，否则返回 null（与用量文案同一 JSON，缺键/非法
 * 输入一律不展示）。导出仅供前端回归测试直接断言。
 */
export function runStreamRetriesLabel(value: string | null): string | null {
  if (!value) return null;
  try {
    const usage = JSON.parse(value) as Record<string, unknown>;
    const retries = typeof usage.stream_retries === "number" ? usage.stream_retries : null;
    if (retries == null || retries <= 0) return null;
    return `重试 ${retries} 次`;
  } catch {
    return null;
  }
}

function runDurationLabel(startedAt: string, endedAt: string | null, now: number): string {
  const start = Date.parse(startedAt);
  const end = endedAt ? Date.parse(endedAt) : now;
  if (Number.isNaN(start) || Number.isNaN(end)) return "";
  const seconds = Math.max(0, Math.floor((end - start) / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  if (minutes < 60) return `${minutes}m ${String(rest).padStart(2, "0")}s`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${String(minutes % 60).padStart(2, "0")}m`;
}

/**
 * F16：duration 的渲染隔离。共享时钟订阅下沉到本组件：`now` 每秒变化时
 * 只有正在运行的 run 条目重新渲染，父 Timeline 不再因时钟订阅而整体重渲染。
 */
const RunDuration = memo(function RunDuration({ startedAt, endedAt }: {
  startedAt: string;
  endedAt: string | null;
}) {
  const now = useSharedNow(endedAt ? null : 1000);
  const duration = runDurationLabel(startedAt, endedAt, now);
  if (!duration) return null;
  return <span className="run-duration">{duration}</span>;
});

export const Timeline = forwardRef<TimelineHandle, Props>(function Timeline(
  {
    taskId,
    branchId = null,
    workspacePath,
    cur,
    running,
    reviewing,
    onAgentEvent,
    onInspectSubagent,
    selectedSubagentId,
  },
  ref
) {
  const [items, setItems] = useState<TimelineItem[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [editingMessageId, setEditingMessageId] = useState<string | null>(null);
  const [editingText, setEditingText] = useState("");
  const [editError, setEditError] = useState<string | null>(null);
  const [resending, setResending] = useState(false);
  const [expandedRunIds, setExpandedRunIds] = useState<Set<string>>(() => new Set());
  const [visibleTurnLimit, setVisibleTurnLimit] = useState(80);
  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const eventsLen = useTasksStore((s) =>
    s.details[taskId]?.task.id === taskId ? s.details[taskId].events.length : 0
  );
  const taskRuns = useTasksStore((s) => s.details[taskId]?.runs);
  const runsStamp = useMemo(
    () => [...(taskRuns ?? [])]
      .sort((a, b) => a.started_at.localeCompare(b.started_at) || a.id.localeCompare(b.id))
      .map(
        (run) =>
          `${run.id}:${run.started_at}:${run.ended_at ?? ""}:${run.review_state}:${run.model}:${run.agent_kind}:${run.agent_label ?? ""}:${run.access_mode}:${run.routing_reason ?? ""}:${run.summary ?? ""}`
      )
      .join("|"),
    [taskRuns],
  );

  const liveRef = useRef(false);
  const idRef = useRef(0);
  const startRef = useRef(Date.now());
  const scrollRef = useRef<HTMLDivElement>(null);
  const editRef = useRef<HTMLTextAreaElement>(null);
  const pinnedRef = useRef(true);
  const prependScrollHeightRef = useRef<number | null>(null);
  const previousTurnCountRef = useRef(0);
  const reloadGenerationRef = useRef(0);

  useEffect(() => {
    setVisibleTurnLimit(80);
    previousTurnCountRef.current = 0;
    prependScrollHeightRef.current = null;
    pinnedRef.current = true;
  }, [taskId]);

  const nid = useCallback(() => `live-${++idRef.current}`, []);
  const nowSec = useCallback(() => Math.max(0, (Date.now() - startRef.current) / 1000), []);

  const reload = useCallback(async () => {
    const generation = ++reloadGenerationRef.current;
    try {
      const msgs = branchId
        ? await sessionMessagesForBranch(taskId, branchId)
        : await sessionMessages(taskId);
      if (generation !== reloadGenerationRef.current) return;
      const d = branchId ? undefined : useTasksStore.getState().details[taskId];
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
      if (generation !== reloadGenerationRef.current) return;
      setError(String(e));
    }
  }, [taskId, branchId]);

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
    setVisibleTurnLimit(80);
    previousTurnCountRef.current = 0;
    void reload();
    return () => {
      // A delayed historical read must never replace the live timeline after the user returns.
      reloadGenerationRef.current += 1;
    };
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
    if (branchId) return;
    const detail = useTasksStore.getState().details[taskId];
    if (liveRef.current) {
      if (detail) {
        setItems((prev) => mergeRunItems(prev, detail.runs, startRef.current));
      }
      return;
    }
    void reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [eventsLen, runsStamp, taskId, branchId]);

  // 流式事件订阅
  useEffect(() => {
    if (branchId) return;
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
  }, [taskId, branchId, refreshDetail, reload, nowSec, nid, onAgentEvent]);

  useImperativeHandle(
    ref,
    () => ({
      reload: () => void reload(),
      onSent: (text, mode, attachments = []) => {
        if (branchId) return;
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
    [reload, nid, nowSec, branchId]
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
    if (branchId || running || resending) return;
    setEditingMessageId(messageId);
    setEditingText(text);
    setEditError(null);
  }, [branchId, running, resending]);
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
    if (branchId || !messageId || !message || running || resending) return;
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
  }, [branchId, editingMessageId, editingText, running, resending, taskId, refreshDetail, reload]);
  const turns = useMemo(() => buildTimelineTurns(items), [items]);
  const visibleTurns = useMemo(
    () => turns.length > visibleTurnLimit ? turns.slice(turns.length - visibleTurnLimit) : turns,
    [turns, visibleTurnLimit]
  );
  const hiddenTurnCount = turns.length - visibleTurns.length;

  // When the user is reading above the live edge, retain the currently mounted window as new
  // turns arrive. At the live edge the fixed tail window can advance without growing the DOM.
  useEffect(() => {
    const previous = previousTurnCountRef.current;
    const added = Math.max(0, turns.length - previous);
    previousTurnCountRef.current = turns.length;
    if (added > 0 && !pinnedRef.current) setVisibleTurnLimit((current) => current + added);
  }, [turns.length]);

  useLayoutEffect(() => {
    const previousHeight = prependScrollHeightRef.current;
    const element = scrollRef.current;
    if (previousHeight == null || !element) return;
    element.scrollTop += element.scrollHeight - previousHeight;
    prependScrollHeightRef.current = null;
  }, [visibleTurnLimit]);

  const loadEarlierTurns = useCallback(() => {
    const element = scrollRef.current;
    if (element) prependScrollHeightRef.current = element.scrollHeight;
    pinnedRef.current = false;
    setVisibleTurnLimit((current) => Math.min(turns.length, current + 80));
  }, [turns.length]);
  const provisionalAgentId = useMemo(() => {
    if (branchId || !reviewing) return null;
    for (let turnIndex = turns.length - 1; turnIndex >= 0; turnIndex--) {
      const turnItems = turns[turnIndex].items;
      for (let itemIndex = turnItems.length - 1; itemIndex >= 0; itemIndex--) {
        if (turnItems[itemIndex].kind === "agent") return turnItems[itemIndex].id;
      }
    }
    return null;
  }, [branchId, reviewing, turns]);
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
        const editing = Boolean(!branchId && it.messageId && editingMessageId === it.messageId);
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
              {it.messageId && !branchId && !running && !editing && !workflow && it.imageCount === 0 && (
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
        const provisional = it.id === provisionalAgentId;
        return (
          <div
            className={`agent${finalResponse ? " timeline-final-response" : ""}${provisional ? " is-provisional" : ""}${dim(it.t)}`}
            data-t={it.t}
            key={it.id}
          >
            <div className="who">R-CODE</div>
            {provisional && (
              <div className="agent-delivery-state" role="status">
                <span>草稿</span>
                质量复核进行中，尚未正式交付
              </div>
            )}
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
        const streamRetries = runStreamRetriesLabel(it.usageJson);
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
              <RunDuration startedAt={it.startedAt} endedAt={it.endedAt} />
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
                  {streamRetries && <span>{streamRetries}</span>}
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
      className={`timeline${visibleTurns.length >= 80 ? " is-long" : ""}`}
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
      {hiddenTurnCount > 0 && <button type="button" className="timeline-load-earlier" onClick={loadEarlierTurns}>加载更早的 {Math.min(80, hiddenTurnCount)} 轮 · 尚有 {hiddenTurnCount} 轮</button>}
      {visibleTurns.map((turn) => {
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
