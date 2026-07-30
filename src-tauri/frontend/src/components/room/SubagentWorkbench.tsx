import { useEffect, useMemo, useState } from "react";
import { subagentSessionMessages } from "../../lib/ipc";
import type { AgentRun, SessionMessage } from "../../lib/types";
import {
  IconActivity,
  IconCheck,
  IconChevronDown,
  IconChevronLeft,
  IconSidebar,
  IconStop,
  IconTerminal,
} from "../icons";
import { Markdown } from "./Markdown";
import { SubagentAvatar } from "./SubagentIdentity";
import { ToolPayloadDetails } from "./ToolCard";
import type { ToolState } from "./model";
import type {
  ActivitySubagent,
  ActivitySubagentEvent,
  ActivityTraceState,
  SubagentStatus,
} from "./activity";

interface Props {
  taskId: string;
  activity: ActivityTraceState;
  runs: readonly AgentRun[];
  selectedSubagentId: string | null;
  onSelect: (subagentId: string) => void;
  onBack: () => void;
  onClose: () => void;
  onAbort: (subagentId: string) => Promise<void>;
}

interface SessionMessageEntry {
  id: string;
  kind: "message";
  text: string;
  tone: "normal" | "danger";
}

interface SessionToolEntry {
  id: string;
  kind: "tool";
  toolName: string;
  summary: string;
  inputJson: string | null;
  outputJson: string | null;
  state: ToolState;
}

interface SessionStatusEntry {
  id: string;
  kind: "status";
  text: string;
  tone: "normal" | "danger";
}

type SessionEntry = SessionMessageEntry | SessionToolEntry | SessionStatusEntry;

/**
 * 子智能体是工作台内的一条完整导航链，而不是一个覆盖其他工具的临时详情：
 * 列表负责总览，详情负责单个会话，返回键始终回列表。
 */
export function SubagentWorkbench({
  taskId,
  activity,
  runs,
  selectedSubagentId,
  onSelect,
  onBack,
  onClose,
  onAbort,
}: Props) {
  const children = useMemo(
    () => mergeSubagents(activity.subagents, runs),
    [activity.subagents, runs],
  );
  const selectedIndex = children.findIndex((child) => child.id === selectedSubagentId);
  const selected = selectedIndex >= 0 ? children[selectedIndex] : undefined;

  return (
    <div
      className="subagent-workbench"
      data-testid={selected ? "subagent-detail" : "subagent-list"}
      data-subagent-view={selected ? "detail" : "list"}
    >
      {selected ? (
        <>
          <SubagentDetailHeader child={selected} index={selectedIndex} onBack={onBack} />
          <SubagentInspector taskId={taskId} child={selected} index={selectedIndex} onAbort={onAbort} />
        </>
      ) : (
        <>
          <SubagentListHeader onClose={onClose} />
          <SubagentList children={children} onSelect={onSelect} />
        </>
      )}
    </div>
  );
}

function SubagentListHeader({ onClose }: { onClose: () => void }) {
  return (
    <header className="subagent-page-header">
      <SubagentAvatar index={0} />
      <strong>子智能体</strong>
      <span className="subagent-page-header-spacer" />
      <button type="button" className="subagent-page-icon-button" onClick={onClose} aria-label="返回运行与子代理" title="返回运行与子代理">
        <IconSidebar width={16} height={16} />
      </button>
    </header>
  );
}

function SubagentDetailHeader({
  child,
  index,
  onBack,
}: {
  child: ActivitySubagent;
  index: number;
  onBack: () => void;
}) {
  return (
    <header className="subagent-page-header">
      <button type="button" className="subagent-page-icon-button" onClick={onBack} aria-label="返回子智能体列表" title="返回子智能体列表">
        <IconChevronLeft width={17} height={17} />
      </button>
      <SubagentAvatar index={index} />
      <strong title={child.label}>{child.label}</strong>
      <span className="subagent-page-header-spacer" />
      <span className={`subagent-page-status status-${child.status}`}>
        <SubagentStateMark status={child.status} />
        <span>{statusLabel(child.status)}</span>
      </span>
    </header>
  );
}

function SubagentList({
  children,
  onSelect,
}: {
  children: readonly ActivitySubagent[];
  onSelect: (subagentId: string) => void;
}) {
  const active = children.filter((child) => isActive(child.status));
  const completed = children.filter((child) => child.status === "completed").reverse();
  const incomplete = children.filter((child) => child.status === "failed" || child.status === "cancelled").reverse();
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (active.length === 0) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [active.length]);

  if (children.length === 0) {
    return (
      <div className="subagent-list-empty">
        <strong>还没有子智能体</strong>
        <p>主代理委派并行任务后，运行状态和结果会出现在这里。</p>
      </div>
    );
  }

  return (
    <div className="subagent-workbench-list" aria-label="子智能体列表">
      {active.length > 0 && (
        <SubagentListSection title="进行中" children={active} allChildren={children} now={now} onSelect={onSelect} />
      )}
      {completed.length > 0 && (
        <SubagentListSection title="已完成" children={completed} allChildren={children} now={now} onSelect={onSelect} />
      )}
      {incomplete.length > 0 && (
        <SubagentListSection title="未完成" children={incomplete} allChildren={children} now={now} onSelect={onSelect} />
      )}
    </div>
  );
}

function SubagentListSection({
  title,
  children,
  allChildren,
  now,
  onSelect,
}: {
  title: string;
  children: readonly ActivitySubagent[];
  allChildren: readonly ActivitySubagent[];
  now: number;
  onSelect: (subagentId: string) => void;
}) {
  return (
    <section className="subagent-list-section" aria-label={title}>
      <h3><span>{title}</span><span>{children.length}</span></h3>
      <div className="subagent-list-rows">
        {children.map((child) => {
          const index = Math.max(0, allChildren.findIndex((item) => item.id === child.id));
          return (
            <button
              type="button"
              className={`subagent-list-row status-${child.status}`}
              key={child.id}
              onClick={() => onSelect(child.id)}
              aria-label={`${child.label}，${statusLabel(child.status)}`}
            >
              <SubagentAvatar index={index} />
              <span className="subagent-list-row-copy">
                <strong title={child.label}>{child.label}</strong>
                <small title={observation(child)}>{observation(child)}</small>
              </span>
              <span className="subagent-list-row-meta">
                <SubagentStateMark status={child.status} />
                <span>{elapsedCompact(child.startedAt, child.endedAt ?? now)}</span>
              </span>
            </button>
          );
        })}
      </div>
    </section>
  );
}

function SubagentInspector({
  taskId,
  child,
  index,
  onAbort,
}: {
  taskId: string;
  child: ActivitySubagent;
  index: number;
  onAbort: (subagentId: string) => Promise<void>;
}) {
  const [messages, setMessages] = useState<SessionMessage[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(true);
  const [stopping, setStopping] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const active = isActive(child.status);

  useEffect(() => {
    setMessages([]);
    setLoading(true);
    setError(null);
    setExpanded(true);
    let dead = false;
    const load = () => {
      subagentSessionMessages(taskId, child.id)
        .then((items) => {
          if (!dead) {
            setMessages(items);
            setError(null);
            setLoading(false);
          }
        })
        .catch((cause) => {
          if (!dead) {
            setError(String(cause));
            setLoading(false);
          }
        });
    };
    load();
    const timer = active ? window.setInterval(load, 1600) : null;
    return () => {
      dead = true;
      if (timer != null) window.clearInterval(timer);
    };
  }, [active, child.id, taskId]);

  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [active]);

  const persistedEntries = useMemo(() => buildPersistedEntries(messages), [messages]);
  const liveEntries = useMemo(() => buildLiveEntries(child.events), [child.events]);
  const entries = useMemo(
    () => mergeSessionEntries(persistedEntries, liveEntries),
    [liveEntries, persistedEntries],
  );
  const runtimeEntries = entries.filter((entry): entry is SessionStatusEntry => entry.kind === "status");
  const transcriptEntries = entries.filter((entry): entry is SessionMessageEntry | SessionToolEntry => entry.kind !== "status");

  const stop = async () => {
    if (stopping || !active) return;
    setStopping(true);
    setError(null);
    try {
      await onAbort(child.id);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setStopping(false);
    }
  };

  return (
    <div className="subagent-detail-body">
      <article className="subagent-session">
        <button
          type="button"
          className="subagent-session-summary"
          aria-expanded={expanded}
          aria-controls={`subagent-session-${child.id}`}
          onClick={() => setExpanded((value) => !value)}
        >
          <span>已处理 {elapsedCompact(child.startedAt, child.endedAt ?? now)}</span>
          <IconChevronDown width={13} height={13} />
        </button>

        {expanded && (
          <div className="subagent-session-body" id={`subagent-session-${child.id}`}>
            {error && <div className="subagent-session-error">读取子智能体记录失败：{error}</div>}
            {runtimeEntries.length > 0 && <SubagentRuntimeLog entries={runtimeEntries} />}
            {loading && transcriptEntries.length === 0 ? (
              <p className="subagent-session-placeholder">正在读取子智能体记录…</p>
            ) : transcriptEntries.length === 0 ? (
              <p className="subagent-session-placeholder">{active ? "Codex 正在运行；公开回复和工具输出会实时出现在这里。" : "运行已经结束，没有保存可见回复。"}</p>
            ) : (
              <div className="subagent-transcript" aria-label="子智能体公开输出">
                {transcriptEntries.map((entry) => entry.kind === "tool" ? (
                  <SubagentToolEvent entry={entry} key={entry.id} />
                ) : (
                  <article className={`subagent-transcript-message${entry.tone === "danger" ? " is-error" : ""}`} key={entry.id}>
                    <div className="subagent-transcript-speaker">
                      <SubagentAvatar index={index} size="xs" />
                      <span>子智能体</span>
                    </div>
                    <Markdown text={entry.text} />
                  </article>
                ))}
              </div>
            )}
            <div className={`subagent-session-state status-${child.status}`} role="status" aria-live="polite">
              <SubagentStateMark status={child.status} />
              <span>{liveStateLabel(child.status)}</span>
              {active && (
                <button type="button" disabled={stopping} onClick={() => void stop()}>
                  <IconStop width={11} height={11} /> {stopping ? "停止中…" : "停止"}
                </button>
              )}
            </div>
          </div>
        )}
      </article>
    </div>
  );
}

function SubagentRuntimeLog({ entries }: { entries: readonly SessionStatusEntry[] }) {
  const latest = entries[entries.length - 1];
  return (
    <details className="subagent-runtime-log">
      <summary>
        <span className="subagent-runtime-log-icon"><IconActivity width={13} height={13} /></span>
        <span>运行记录</span>
        <small title={latest?.text}>{latest?.text}</small>
        <em>{entries.length}</em>
        <IconChevronDown width={13} height={13} />
      </summary>
      <ol>
        {entries.map((entry) => (
          <li className={entry.tone === "danger" ? "is-error" : undefined} key={entry.id}>
            <span aria-hidden="true" />
            <span>{entry.text}</span>
          </li>
        ))}
      </ol>
    </details>
  );
}

function SubagentToolEvent({ entry }: { entry: SessionToolEntry }) {
  const [open, setOpen] = useState(false);
  const hasDetails = Boolean(entry.inputJson?.trim() || entry.outputJson?.trim());
  return (
    <section className={`subagent-transcript-tool state-${entry.state}${open ? " open" : ""}`}>
      <button
        type="button"
        className="subagent-transcript-tool-head ring-inset"
        aria-expanded={hasDetails ? open : undefined}
        disabled={!hasDetails}
        onClick={() => setOpen((value) => !value)}
      >
        <span className="subagent-transcript-tool-icon"><IconTerminal width={13} height={13} /></span>
        <span className="subagent-transcript-tool-name">{entry.toolName}</span>
        <code title={entry.summary}>{entry.summary}</code>
        <span className={`subagent-transcript-tool-state state-${entry.state}`}>{toolStateLabel(entry.state)}</span>
        {hasDetails && <IconChevronDown width={13} height={13} />}
      </button>
      {open && (
        <div className="subagent-transcript-tool-body">
          <ToolPayloadDetails inputJson={entry.inputJson} outputJson={entry.outputJson} state={entry.state} />
        </div>
      )}
    </section>
  );
}

function SubagentStateMark({ status }: { status: SubagentStatus }) {
  if (isActive(status)) return <span className={`subagent-spinner status-${status}`} aria-hidden="true" />;
  if (status === "completed") {
    return <span className="subagent-complete-mark" aria-hidden="true"><IconCheck width={11} height={11} /></span>;
  }
  return <span className={`subagent-incomplete-mark status-${status}`} aria-hidden="true" />;
}

function mergeSubagents(current: readonly ActivitySubagent[], runs: readonly AgentRun[]): ActivitySubagent[] {
  const merged = new Map(current.map((child) => [child.id, child]));
  for (const run of runs) {
    if (run.agent_kind !== "subagent" || merged.has(run.id)) continue;
    const startedAt = parseTimestamp(run.started_at);
    const endedAt = run.ended_at ? parseTimestamp(run.ended_at) : null;
    merged.set(run.id, {
      id: run.id,
      label: run.agent_label?.trim() || "子智能体",
      runtimeKind: run.runtime_kind,
      model: run.model || null,
      status: runStatus(run),
      phase: run.ended_at ? "idle" : "requesting",
      detail: compactText(run.summary),
      startedAt,
      lastEventAt: endedAt ?? startedAt,
      endedAt,
      events: [],
    });
  }
  return [...merged.values()].sort((left, right) => left.startedAt - right.startedAt || left.id.localeCompare(right.id));
}

function buildLiveEntries(events: readonly ActivitySubagentEvent[]): SessionEntry[] {
  const entries: SessionEntry[] = [];
  for (const event of events) {
    if (event.kind === "tool_call") {
      const toolName = compactText(event.label) ?? "Codex 工具";
      const rawSummary = compactText(event.detail) ?? toolName;
      const summary = rawSummary.startsWith(`${toolName} · `)
        ? rawSummary.slice(toolName.length + 3)
        : rawSummary;
      entries.push({
        id: event.id,
        kind: "tool",
        toolName,
        summary,
        inputJson: JSON.stringify({ command: summary }),
        outputJson: null,
        state: "active",
      });
      continue;
    }
    if (event.kind === "message") {
      const text = visibleText(event.detail);
      if (text) entries.push({ id: event.id, kind: "message", text, tone: event.isError ? "danger" : "normal" });
      continue;
    }
    const text = compactText(event.detail) ?? compactText(event.label);
    if (text) pushUniqueStatus(entries, {
      id: event.id,
      kind: "status",
      text,
      tone: event.isError ? "danger" : "normal",
    });
  }
  return entries;
}

function buildPersistedEntries(messages: readonly SessionMessage[]): SessionEntry[] {
  const entries: SessionEntry[] = [];
  const toolsByCallId = new Map<string, number>();
  for (const [index, message] of messages.entries()) {
    const id = message.id ?? `subagent-entry-${index}`;
    if (message.kind === "tool_call") {
      const input = parseObject(message.input_json);
      const toolName = compactText(message.tool_name) ?? "Codex 工具";
      const summary = compactText(firstString(input, ["summary", "command", "path"]) ?? toolName) ?? toolName;
      const entry: SessionToolEntry = {
        id,
        kind: "tool",
        toolName,
        summary,
        inputJson: normalizeToolInput(message.input_json, toolName, summary),
        outputJson: null,
        state: "active",
      };
      entries.push(entry);
      if (message.call_id) toolsByCallId.set(message.call_id, entries.length - 1);
      continue;
    }
    if (message.kind === "tool_result") {
      const toolIndex = message.call_id ? toolsByCallId.get(message.call_id) : undefined;
      if (toolIndex != null && entries[toolIndex]?.kind === "tool") {
        const tool = entries[toolIndex] as SessionToolEntry;
        entries[toolIndex] = {
          ...tool,
          outputJson: normalizeToolOutput(message.output_json),
          state: message.is_error ? "fail" : "ok",
        };
      } else if (message.is_error) {
        pushUniqueStatus(entries, {
          id,
          kind: "status",
          text: compactText(readToolResultText(message.output_json)) ?? "工具执行失败",
          tone: "danger",
        });
      }
      continue;
    }
    if (message.kind === "message" && message.role === "assistant") {
      const text = visibleText(message.text);
      if (text) entries.push({ id, kind: "message", text, tone: text.startsWith("[error]") ? "danger" : "normal" });
      continue;
    }
    if (message.kind !== "system" || (message.text !== "subagent_activity" && message.text !== "subagent_lifecycle")) continue;
    const data = parseObject(message.output_json);
    const detail = compactText(firstString(data, ["detail", "summary", "message"]));
    if (detail) {
      const state = firstString(data, ["state"]);
      pushUniqueStatus(entries, {
        id,
        kind: "status",
        text: detail,
        tone: state === "failed" || state === "cancelled" ? "danger" : "normal",
      });
    }
  }
  return entries.slice(-80);
}

function mergeSessionEntries(
  persistedEntries: readonly SessionEntry[],
  liveEntries: readonly SessionEntry[],
): SessionEntry[] {
  const entries = [...persistedEntries];
  for (const live of liveEntries) {
    if (entries.some((entry) => sessionEntriesEquivalent(entry, live))) continue;
    entries.push(live);
  }
  return entries.slice(-80);
}

function sessionEntriesEquivalent(left: SessionEntry, right: SessionEntry): boolean {
  if (left.kind !== right.kind) return false;
  if (left.kind === "tool" && right.kind === "tool") {
    return left.toolName === right.toolName && left.summary === right.summary;
  }
  if (left.kind === "message" && right.kind === "message") {
    return left.text === right.text || left.text.startsWith(right.text) || right.text.startsWith(left.text);
  }
  return left.kind === "status" && right.kind === "status" && left.text === right.text;
}

function pushUniqueStatus(entries: SessionEntry[], entry: SessionStatusEntry) {
  const previous = entries[entries.length - 1];
  if (previous?.kind === "status" && previous.text === entry.text) return;
  entries.push(entry);
}

function normalizeToolInput(raw: string | null | undefined, toolName: string, summary: string): string | null {
  const record = parseObject(raw);
  if (record && Object.keys(record).length === 1 && typeof record.summary === "string") {
    return toolName.includes("命令") ? JSON.stringify({ command: summary }) : JSON.stringify({ summary });
  }
  return raw?.trim() || null;
}

function normalizeToolOutput(raw: string | null | undefined): string | null {
  const record = parseObject(raw);
  if (record && Object.keys(record).every((key) => key === "status")) return null;
  if (record) {
    const output = firstString(record, ["output", "stdout", "content", "text"]);
    if (output && Object.keys(record).every((key) => key === "status" || ["output", "stdout", "content", "text"].includes(key))) {
      return JSON.stringify(output);
    }
  }
  return raw?.trim() || null;
}

function readToolResultText(raw: string | null | undefined): string | null {
  const record = parseObject(raw);
  return firstString(record, ["output", "stdout", "content", "message", "error"])
    ?? (raw?.trim() || null);
}

function visibleText(value: string | null | undefined, limit = 20_000): string | null {
  const normalized = value?.trim();
  if (!normalized) return null;
  return normalized.length > limit ? `${normalized.slice(0, limit - 1)}…` : normalized;
}

function toolStateLabel(state: ToolState): string {
  if (state === "active") return "运行中";
  if (state === "fail") return "失败";
  return "完成";
}

function parseObject(raw: string | null | undefined): Record<string, unknown> | null {
  if (!raw) return null;
  let value: unknown = raw;
  for (let depth = 0; depth < 3; depth += 1) {
    if (typeof value === "string") {
      try {
        value = JSON.parse(value);
      } catch {
        return null;
      }
      continue;
    }
    if (value && typeof value === "object" && !Array.isArray(value)) return value as Record<string, unknown>;
    return null;
  }
  return null;
}

function firstString(record: Record<string, unknown> | null, keys: readonly string[]): string | null {
  if (!record) return null;
  for (const key of keys) {
    const value = record[key];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return null;
}

function compactText(value: string | null | undefined, limit = 220): string | null {
  if (!value) return null;
  const normalized = value.trim().replace(/\s+/g, " ");
  if (!normalized) return null;
  return normalized.length > limit ? `${normalized.slice(0, limit - 1)}…` : normalized;
}

function isActive(status: SubagentStatus): boolean {
  return status === "queued" || status === "running" || status === "waiting_permission";
}

function runStatus(run: AgentRun): SubagentStatus {
  if (run.ended_at == null) return "running";
  if (run.review_state === "failed") return "failed";
  if (run.review_state === "aborted") return "cancelled";
  return "completed";
}

function statusLabel(status: SubagentStatus): string {
  switch (status) {
    case "queued": return "等待中";
    case "running": return "进行中";
    case "waiting_permission": return "等待权限";
    case "completed": return "已完成";
    case "failed": return "失败";
    case "cancelled": return "已停止";
  }
}

function liveStateLabel(status: SubagentStatus): string {
  switch (status) {
    case "queued": return "正在等待调度";
    case "running": return "正在继续运行";
    case "waiting_permission": return "正在等待权限";
    case "completed": return "运行已完成";
    case "failed": return "运行失败";
    case "cancelled": return "运行已停止";
  }
}

function observation(child: ActivitySubagent): string {
  if (child.detail) return child.detail;
  switch (child.status) {
    case "queued": return "等待调度";
    case "running": return "等待第一条公开进度";
    case "waiting_permission": return "需要权限后才能继续";
    case "completed": return "已完成，暂无结果摘要";
    case "failed": return "运行未完成";
    case "cancelled": return "已由用户停止";
  }
}

function elapsedCompact(startedAt: number, endedAt: number): string {
  const seconds = Math.max(0, Math.floor((endedAt - startedAt) / 1000));
  const minutes = Math.floor(seconds / 60);
  return minutes > 0 ? `${minutes}m ${String(seconds % 60).padStart(2, "0")}s` : `${seconds}s`;
}

function parseTimestamp(value: string): number {
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? Date.now() : parsed;
}
