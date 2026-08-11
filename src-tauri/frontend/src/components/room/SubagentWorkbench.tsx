import { useCallback, useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import { subagentSessionMessages } from "../../lib/ipc";
import { usePoll } from "../../lib/poll";
import { useSharedNow } from "../../lib/shared-clock";
import type { AgentRun, SessionMessage, SubagentAccessMode } from "../../lib/types";
import {
  IconActivity,
  IconCheck,
  IconChevronDown,
  IconClose,
  IconFile,
  IconMaximize,
  IconMinimize,
  IconPlus,
  IconSearch,
  IconSidebar,
  IconStop,
  IconTerminal,
} from "../icons";
import { Markdown } from "./Markdown";
import { SubagentAvatar } from "./SubagentIdentity";
import { ToolPayloadDetails } from "./ToolCard";
import type { ToolState } from "./model";
import {
  toolActivityKind,
  toolActivityProgress,
  toolActivityTitle,
  type ToolActivityKind,
} from "./tool-activity";
import { handleWorkbenchTabListKeyDown } from "./workbench-tabs";
import type {
  ActivitySubagent,
  ActivitySubagentEvent,
  SubagentStatus,
} from "./activity";

interface Props {
  taskId: string;
  workspacePath: string | null;
  subagents: readonly ActivitySubagent[];
  selectedSubagentId: string | null;
  openSubagentIds: readonly string[];
  toolTabsBefore: ReactNode;
  toolTabsAfter: ReactNode;
  onSelect: (subagentId: string) => void;
  onCloseTab: (subagentId: string) => void;
  onOpenLauncher: () => void;
  onHide: () => void;
  onToggleFocus: () => void;
  focused: boolean;
  onAbort: (subagentId: string) => Promise<void>;
}

interface SessionMessageEntry {
  id: string;
  kind: "message";
  text: string;
  tone: "normal" | "danger";
}

interface SessionReasoningEntry {
  id: string;
  kind: "reasoning";
  text: string;
}

interface SessionToolEntry {
  id: string;
  kind: "tool";
  callId: string | null;
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

type SessionEntry = SessionMessageEntry | SessionReasoningEntry | SessionToolEntry | SessionStatusEntry;

interface SessionToolGroupEntry {
  id: string;
  kind: "tool_group";
  groupKind: ToolActivityKind;
  tools: SessionToolEntry[];
}

type TranscriptBlock = SessionMessageEntry | SessionReasoningEntry | SessionToolEntry | SessionToolGroupEntry;

interface SubagentPermission {
  accessMode: SubagentAccessMode;
  requireApproval: boolean;
}

type SubagentPermissionMode = SubagentAccessMode | "request_approval";

type SubagentSectionKind = "attention" | "active" | "completed" | "incomplete";

type SubagentSectionExpansion = Record<SubagentSectionKind, boolean>;

const DEFAULT_SUBAGENT_SECTION_EXPANSION: SubagentSectionExpansion = {
  attention: true,
  active: true,
  completed: false,
  incomplete: true,
};

/**
 * 子智能体总览和每个子智能体会话都是独立标签页。
 * 标签以稳定的运行 ID 去重；重新打开已有会话时只切换激活项。
 */
export function SubagentWorkbench({
  taskId,
  workspacePath,
  subagents,
  selectedSubagentId,
  openSubagentIds,
  toolTabsBefore,
  toolTabsAfter,
  onSelect,
  onCloseTab,
  onOpenLauncher,
  onHide,
  onToggleFocus,
  focused,
  onAbort,
}: Props) {
  const [expandedSections, setExpandedSections] = useState<SubagentSectionExpansion>(
    () => ({ ...DEFAULT_SUBAGENT_SECTION_EXPANSION }),
  );
  const childIndexById = useMemo(
    () => new Map(subagents.map((child, index) => [child.id, index])),
    [subagents],
  );
  const selectedIndex = selectedSubagentId == null
    ? -1
    : (childIndexById.get(selectedSubagentId) ?? -1);
  const selected = selectedIndex >= 0 ? subagents[selectedIndex] : undefined;
  const toggleSection = useCallback((kind: SubagentSectionKind) => {
    setExpandedSections((current) => ({
      ...current,
      [kind]: !current[kind],
    }));
  }, []);

  useEffect(() => {
    setExpandedSections({ ...DEFAULT_SUBAGENT_SECTION_EXPANSION });
  }, [taskId]);

  return (
    <div
      className="subagent-workbench"
      data-testid={selected ? "subagent-detail" : "subagent-list"}
      data-subagent-view={selected ? "detail" : "list"}
    >
      <SubagentTabsHeader
        toolTabsBefore={toolTabsBefore}
        toolTabsAfter={toolTabsAfter}
        subagents={subagents}
        openSubagentIds={openSubagentIds}
        selectedSubagentId={selected?.id ?? null}
        onSelect={onSelect}
        onCloseTab={onCloseTab}
        onOpenLauncher={onOpenLauncher}
        onHide={onHide}
        onToggleFocus={onToggleFocus}
        focused={focused}
      />
      {selected
        ? <SubagentInspector taskId={taskId} workspacePath={workspacePath} child={selected} index={selectedIndex} onAbort={onAbort} />
        : (
          <SubagentList
            children={subagents}
            expandedSections={expandedSections}
            onSelect={onSelect}
            onToggleSection={toggleSection}
          />
        )}
    </div>
  );
}

function SubagentTabsHeader({
  toolTabsBefore,
  toolTabsAfter,
  subagents,
  openSubagentIds,
  selectedSubagentId,
  onSelect,
  onCloseTab,
  onOpenLauncher,
  onHide,
  onToggleFocus,
  focused,
}: {
  toolTabsBefore: ReactNode;
  toolTabsAfter: ReactNode;
  subagents: readonly ActivitySubagent[];
  openSubagentIds: readonly string[];
  selectedSubagentId: string | null;
  onSelect: (subagentId: string) => void;
  onCloseTab: (subagentId: string) => void;
  onOpenLauncher: () => void;
  onHide: () => void;
  onToggleFocus: () => void;
  focused: boolean;
}) {
  return (
    <header className="subagent-page-header workbench-head subagent-tabs-header">
      <div className="workbench-tabs" role="tablist" aria-label="任务工作台标签" onKeyDown={handleWorkbenchTabListKeyDown}>
        {toolTabsBefore}
        <SubagentSessionTabs
          subagents={subagents}
          openSubagentIds={openSubagentIds}
          selectedSubagentId={selectedSubagentId}
          onSelect={onSelect}
          onCloseTab={onCloseTab}
        />
        {toolTabsAfter}
      </div>
      <button type="button" className="workbench-head-action workbench-add-button" onClick={onOpenLauncher} aria-label="打开工具启动器" title="新增扩展">
        <IconPlus width={16} height={16} />
      </button>
      <span className="subagent-page-header-spacer" />
      <button type="button" className="subagent-page-icon-button" onClick={onToggleFocus} aria-label={focused ? "退出专注模式" : "专注工作台"} aria-pressed={focused}>
        {focused ? <IconMinimize width={16} height={16} /> : <IconMaximize width={16} height={16} />}
      </button>
      <button type="button" className="subagent-page-icon-button" onClick={onHide} aria-label="隐藏工作台" title="隐藏工作台">
        <IconSidebar width={16} height={16} />
      </button>
    </header>
  );
}

function SubagentList({
  children,
  expandedSections,
  onSelect,
  onToggleSection,
}: {
  children: readonly ActivitySubagent[];
  expandedSections: Readonly<SubagentSectionExpansion>;
  onSelect: (subagentId: string) => void;
  onToggleSection: (kind: SubagentSectionKind) => void;
}) {
  const { attention, active, completed, incomplete, indexById } = useMemo(() => ({
    attention: children.filter((child) => child.status === "waiting_permission"),
    active: children
      .filter((child) => child.status === "queued" || child.status === "running")
      .sort((a, b) => b.startedAt - a.startedAt || a.id.localeCompare(b.id)),
    completed: children
      .filter((child) => child.status === "completed")
      .sort((a, b) => (b.endedAt ?? b.lastEventAt) - (a.endedAt ?? a.lastEventAt)),
    incomplete: children
      .filter((child) => child.status === "failed" || child.status === "cancelled")
      .sort((a, b) => (b.endedAt ?? b.lastEventAt) - (a.endedAt ?? a.lastEventAt)),
    indexById: new Map(children.map((child, index) => [child.id, index])),
  }), [children]);
  const live = active.length + attention.length > 0;
  const now = useSharedNow(children.length === 0 ? null : live ? 1000 : 60_000);

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
      {attention.length > 0 && (
        <SubagentListSection
          key="attention"
          kind="attention"
          title="需要处理"
          children={attention}
          indexById={indexById}
          now={now}
          expanded={expandedSections.attention}
          onSelect={onSelect}
          onToggle={() => onToggleSection("attention")}
        />
      )}
      {active.length > 0 && (
        <SubagentListSection
          key="active"
          kind="active"
          title="进行中"
          children={active}
          indexById={indexById}
          now={now}
          expanded={expandedSections.active}
          onSelect={onSelect}
          onToggle={() => onToggleSection("active")}
        />
      )}
      {completed.length > 0 && (
        <SubagentListSection
          key="completed"
          kind="completed"
          title="已完成"
          children={completed}
          indexById={indexById}
          now={now}
          expanded={expandedSections.completed}
          onSelect={onSelect}
          onToggle={() => onToggleSection("completed")}
        />
      )}
      {incomplete.length > 0 && (
        <SubagentListSection
          key="incomplete"
          kind="incomplete"
          title="未完成"
          children={incomplete}
          indexById={indexById}
          now={now}
          expanded={expandedSections.incomplete}
          onSelect={onSelect}
          onToggle={() => onToggleSection("incomplete")}
        />
      )}
    </div>
  );
}

/**
 * 子代理会话 Tab 独立于当前工作台工具渲染，这样切到文件等工具时仍可一键返回。
 */
export function SubagentSessionTabs({
  subagents,
  openSubagentIds,
  selectedSubagentId,
  onSelect,
  onCloseTab,
}: {
  subagents: readonly ActivitySubagent[];
  openSubagentIds: readonly string[];
  selectedSubagentId: string | null;
  onSelect: (subagentId: string) => void;
  onCloseTab: (subagentId: string) => void;
}) {
  const indexById = useMemo(
    () => new Map(subagents.map((child, index) => [child.id, index])),
    [subagents],
  );

  return (
    <>
      {openSubagentIds.map((id) => {
        const index = indexById.get(id) ?? -1;
        if (index < 0) return null;
        const child = subagents[index];
        const selected = selectedSubagentId === child.id;
        return (
          <div
            key={child.id}
            className={`workbench-tab subagent-session-tab${selected ? " workbench-active-tab" : ""}`}
          >
            <button
              type="button"
              className="workbench-tab-select"
              role="tab"
              tabIndex={selected ? 0 : -1}
              aria-selected={selected}
              aria-label={child.label}
              data-subagent-id={child.id}
              onClick={() => onSelect(child.id)}
            >
              <SubagentAvatar
                index={index}
                identity={child.id}
                runtimeKind={child.runtimeKind}
                size="xs"
              />
              <strong title={child.label}>{child.label}</strong>
            </button>
            <button
              type="button"
              className="workbench-tab-close"
              onClick={(event) => {
                event.stopPropagation();
                onCloseTab(child.id);
              }}
              aria-label={`关闭${child.label}标签页`}
              title={`关闭${child.label}`}
            >
              <IconClose width={13} height={13} />
            </button>
          </div>
        );
      })}
    </>
  );
}

function SubagentListSection({
  kind,
  title,
  children,
  indexById,
  now,
  expanded,
  onSelect,
  onToggle,
}: {
  kind: SubagentSectionKind;
  title: string;
  children: readonly ActivitySubagent[];
  indexById: ReadonlyMap<string, number>;
  now: number;
  expanded: boolean;
  onSelect: (subagentId: string) => void;
  onToggle: () => void;
}) {
  const sectionId = useId();
  const titleId = `${sectionId}-title`;
  const rowsId = `${sectionId}-rows`;

  return (
    <section
      className={`subagent-list-section kind-${kind} ${expanded ? "is-expanded" : "is-collapsed"}`}
      aria-labelledby={titleId}
    >
      <button
        type="button"
        className="subagent-list-section-toggle"
        aria-expanded={expanded}
        aria-controls={rowsId}
        aria-label={`${expanded ? "收起" : "展开"}${title}子代理，当前 ${children.length} 个`}
        title={expanded ? `收起${title}` : `展开${title}`}
        onClick={onToggle}
      >
        <span className="subagent-list-section-indicator" aria-hidden="true" />
        <span className="subagent-list-section-title" id={titleId}>{title}</span>
        <span className="subagent-list-section-count" aria-hidden="true">
          {String(children.length).padStart(2, "0")}
        </span>
        <IconChevronDown className="subagent-list-section-chevron" width={14} height={14} aria-hidden="true" />
      </button>
      <div className="subagent-list-rows" id={rowsId} hidden={!expanded}>
        {children.map((child) => {
          const index = indexById.get(child.id) ?? 0;
          return (
            <button
              type="button"
              className={`subagent-list-row status-${child.status}`}
              key={child.id}
              onClick={() => onSelect(child.id)}
              aria-label={`${child.label}，${statusLabel(child.status)}`}
            >
              <SubagentAvatar
                index={index}
                identity={child.id}
                runtimeKind={child.runtimeKind}
                className={`subagent-list-avatar status-${child.status}`}
              />
              <span className="subagent-list-row-copy">
                <strong title={child.label}>{child.label}</strong>
                <small title={listObservation(child)}>{listObservation(child)}</small>
              </span>
              <span className="subagent-list-row-meta" title={listTimeTitle(child)}>
                <time>{listTime(child, now)}</time>
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
  workspacePath,
  child,
  index,
  onAbort,
}: {
  taskId: string;
  workspacePath: string | null;
  child: ActivitySubagent;
  index: number;
  onAbort: (subagentId: string) => Promise<void>;
}) {
  const [messages, setMessages] = useState<SessionMessage[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(true);
  const [stopping, setStopping] = useState(false);
  const sessionGenerationRef = useRef(0);
  const requestSequenceRef = useRef(0);
  const active = isActive(child.status);
  const now = useSharedNow(active ? 1000 : null);

  const load = useCallback(async () => {
    const generation = sessionGenerationRef.current;
    const requestSequence = ++requestSequenceRef.current;
    try {
      const items = await subagentSessionMessages(taskId, child.id);
      if (
        generation === sessionGenerationRef.current
        && requestSequence === requestSequenceRef.current
      ) {
        setMessages(items);
        setError(null);
        setLoading(false);
      }
    } catch (cause) {
      if (
        generation === sessionGenerationRef.current
        && requestSequence === requestSequenceRef.current
      ) {
        setError(String(cause));
        setLoading(false);
      }
    }
  }, [child.id, taskId]);

  useEffect(() => {
    sessionGenerationRef.current += 1;
    setMessages([]);
    setLoading(true);
    setError(null);
    setExpanded(true);
    return () => {
      sessionGenerationRef.current += 1;
    };
  }, [load]);

  usePoll(load, 1600, active, "子代理会话");

  useEffect(() => {
    if (!active) void load();
  }, [active, load]);

  const persistedEntries = useMemo(() => buildPersistedEntries(messages), [messages]);
  const liveEntries = useMemo(
    () => buildLiveEntries(child.events, child.status),
    [child.events, child.status],
  );
  const entries = useMemo(
    () => mergeSessionEntries(persistedEntries, liveEntries),
    [liveEntries, persistedEntries],
  );
  const { runtimeEntries, transcriptEntries, transcriptBlocks, failedToolCount } = useMemo(() => {
    const runtime = entries.filter((entry): entry is SessionStatusEntry => entry.kind === "status");
    const transcript = entries.filter(
      (entry): entry is SessionMessageEntry | SessionReasoningEntry | SessionToolEntry => entry.kind !== "status",
    );
    return {
      runtimeEntries: runtime,
      transcriptEntries: transcript,
      transcriptBlocks: groupTranscriptEntries(transcript),
      failedToolCount: transcript.filter((entry) => entry.kind === "tool" && entry.state === "fail").length,
    };
  }, [entries]);
  const permission = useMemo(
    () => resolveSessionPermission(messages, {
      accessMode: child.accessMode,
      requireApproval: child.requireApproval,
    }),
    [child.accessMode, child.requireApproval, messages],
  );
  const permissionMode = effectivePermissionMode(permission);

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
          <span className={`subagent-session-permission mode-${permissionMode}`}>{permissionModeLabel(permissionMode)}</span>
          <IconChevronDown width={13} height={13} />
        </button>

        {expanded && (
          <div className="subagent-session-body" id={`subagent-session-${child.id}`}>
            <div className="subagent-session-meta" aria-label="子智能体编排信息">
              <span><b>执行器</b>{runtimeExecutorName(child.runtimeKind)}</span>
              <span><b>权限</b>{permissionModeLabel(permissionMode)}</span>
              {child.routingReason && <span className="subagent-routing-reason"><b>路由</b>{child.routingReason}</span>}
            </div>
            {error && <div className="subagent-session-error">读取子智能体记录失败：{error}</div>}
            {runtimeEntries.length > 0 && <SubagentRuntimeLog entries={runtimeEntries} />}
            {loading && transcriptEntries.length === 0 ? (
              <p className="subagent-session-placeholder">正在读取子智能体记录…</p>
            ) : transcriptEntries.length === 0 ? (
              <p className="subagent-session-placeholder">
                {active
                  ? `${runtimeName(child.runtimeKind)} 正在运行；公开回复和工具输出会实时出现在这里。`
                  : "运行已经结束，没有保存可见回复。"}
              </p>
            ) : (
              <div className="subagent-transcript" aria-label="子智能体公开输出">
                {transcriptBlocks.map((entry) => entry.kind === "tool_group" ? (
                  <SubagentToolGroup entry={entry} key={entry.id} />
                ) : entry.kind === "tool" ? (
                  <SubagentToolEvent entry={entry} key={entry.id} />
                ) : entry.kind === "reasoning" ? (
                  <SubagentReasoningEvent
                    entry={entry}
                    key={entry.id}
                    taskId={taskId}
                    workspacePath={workspacePath}
                  />
                ) : (
                  <article className={`subagent-transcript-message${entry.tone === "danger" ? " is-error" : ""}`} key={entry.id}>
                    <div className="subagent-transcript-speaker">
                      <SubagentAvatar
                        index={index}
                        identity={child.id}
                        runtimeKind={child.runtimeKind}
                        size="xs"
                      />
                      <span>{runtimeName(child.runtimeKind)} 子智能体</span>
                    </div>
                    <Markdown text={entry.text} taskId={taskId} workspacePath={workspacePath} />
                  </article>
                ))}
              </div>
            )}
            <div className={`subagent-session-state status-${child.status}${failedToolCount > 0 ? " has-tool-failures" : ""}`} role="status" aria-live="polite">
              <SubagentStateMark status={child.status} />
              <span>{liveStateLabel(child.status, failedToolCount)}</span>
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

function SubagentReasoningEvent({
  entry,
  taskId,
  workspacePath,
}: {
  entry: SessionReasoningEntry;
  taskId: string;
  workspacePath: string | null;
}) {
  const preview = entry.text.replace(/\s+/g, " ").trim();
  return (
    <details className="subagent-reasoning-event">
      <summary>
        <span className="subagent-runtime-log-icon"><IconActivity width={13} height={13} /></span>
        <span>模型思考</span>
        <small title={preview}>{preview}</small>
        <IconChevronDown width={13} height={13} />
      </summary>
      <div className="subagent-reasoning-detail">
        <Markdown text={entry.text} taskId={taskId} workspacePath={workspacePath} />
      </div>
    </details>
  );
}

export function SubagentToolGroup({ entry }: { entry: SessionToolGroupEntry }) {
  const activeCount = entry.tools.filter((tool) => tool.state === "active").length;
  const failedCount = entry.tools.filter((tool) => tool.state === "fail").length;
  const [open, setOpen] = useState(false);
  const generatedId = useId().replace(/:/g, "");
  const detailId = `subagent-tool-group-${generatedId}`;

  const state = activeCount > 0 ? "active" : failedCount > 0 ? "fail" : "ok";
  const stateText = toolActivityProgress(entry.tools.map((tool) => tool.state));
  const title = toolActivityTitle(
    entry.groupKind,
    entry.tools.length,
    state,
    entry.tools[0]?.summary ?? "",
  );
  const summaries = [...new Set(entry.tools.map((tool) => tool.summary))].slice(0, 3).join(" · ");
  return (
    <section className={`subagent-tool-group kind-${entry.groupKind} state-${state}${open ? " open" : ""}`}>
      <button
        type="button"
        className="subagent-tool-group-head ring-inset"
        aria-expanded={open}
        aria-controls={detailId}
        onClick={() => setOpen((value) => !value)}
      >
        <span className="subagent-transcript-tool-icon"><ToolGroupIcon kind={entry.groupKind} /></span>
        <span className="subagent-tool-group-title">{title}</span>
        <code title={summaries}>{summaries}</code>
        <span className={`subagent-transcript-tool-state state-${state}`}>{stateText}</span>
        <IconChevronDown width={13} height={13} />
      </button>
      {open && (
        <div className="subagent-tool-group-list" id={detailId}>
          {entry.tools.map((tool) => <SubagentToolEvent entry={tool} key={tool.id} />)}
        </div>
      )}
    </section>
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

export function mergeSubagents(current: readonly ActivitySubagent[], runs: readonly AgentRun[]): ActivitySubagent[] {
  const merged = new Map(current.map((child) => [child.id, child]));
  for (const run of runs) {
    if (run.agent_kind !== "subagent" || merged.has(run.id)) continue;
    const startedAt = parseTimestamp(run.started_at);
    const endedAt = run.ended_at ? parseTimestamp(run.ended_at) : null;
    const accessMode = run.access_mode ?? "read_only";
    merged.set(run.id, {
      id: run.id,
      label: run.agent_label?.trim() || "子智能体",
      runtimeKind: run.runtime_kind,
      model: run.model || null,
      accessMode,
      requireApproval: accessMode === "full_access" && (run.require_approval ?? false),
      routingReason: compactText(run.routing_reason),
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

export function buildLiveEntries(
  events: readonly ActivitySubagentEvent[],
  status: SubagentStatus,
): SessionEntry[] {
  const entries: SessionEntry[] = [];
  const toolsByCallId = new Map<string, number>();
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
        callId: event.callId ?? null,
        toolName,
        summary,
        inputJson: JSON.stringify({ command: summary }),
        outputJson: null,
        state: "active",
      });
      if (event.callId) toolsByCallId.set(event.callId, entries.length - 1);
      continue;
    }
    if (event.kind === "tool_result") {
      let toolIndex = event.callId ? toolsByCallId.get(event.callId) : undefined;
      if (toolIndex == null && !event.callId) {
        for (let index = entries.length - 1; index >= 0; index -= 1) {
          const candidate = entries[index];
          if (candidate?.kind === "tool" && candidate.state === "active") {
            toolIndex = index;
            break;
          }
        }
      }
      if (toolIndex != null && entries[toolIndex]?.kind === "tool") {
        const tool = entries[toolIndex] as SessionToolEntry;
        entries[toolIndex] = {
          ...tool,
          outputJson: normalizeToolOutput(event.outputJson),
          state: event.isError ? "fail" : "ok",
        };
      } else if (event.isError) {
        pushUniqueStatus(entries, {
          id: event.id,
          kind: "status",
          text: compactText(readToolResultText(event.outputJson)) ?? "工具执行失败",
          tone: "danger",
        });
      }
      continue;
    }
    if (event.kind === "message") {
      const text = visibleText(event.detail);
      if (text) appendMessageEntry(entries, {
        id: event.id,
        kind: "message",
        text,
        tone: event.isError ? "danger" : "normal",
      });
      continue;
    }
    if (event.kind === "reasoning") {
      const text = visibleText(event.detail);
      if (text) appendReasoningEntry(entries, { id: event.id, kind: "reasoning", text });
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
  if (status === "completed" || status === "failed" || status === "cancelled") {
    const terminalState: ToolState = status === "completed" ? "ok" : "fail";
    for (let index = 0; index < entries.length; index += 1) {
      const entry = entries[index];
      if (entry?.kind === "tool" && entry.state === "active") {
        entries[index] = { ...entry, state: terminalState };
      }
    }
  }
  return entries;
}

function ToolGroupIcon({ kind }: { kind: ToolActivityKind }) {
  if (kind === "command") return <IconTerminal width={13} height={13} />;
  if (kind === "file") return <IconFile width={13} height={13} />;
  if (kind === "lookup") return <IconSearch width={13} height={13} />;
  return <IconActivity width={13} height={13} />;
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
        callId: message.call_id ?? null,
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
      if (text) appendMessageEntry(entries, {
        id,
        kind: "message",
        text,
        tone: text.startsWith("[error]") ? "danger" : "normal",
      });
      continue;
    }
    if (message.kind === "system" && message.text === "r_code_reasoning") {
      const data = parseObject(message.output_json);
      const text = visibleText(firstString(data, ["text"]));
      if (text) appendReasoningEntry(entries, { id, kind: "reasoning", text });
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

export function mergeSessionEntries(
  persistedEntries: readonly SessionEntry[],
  liveEntries: readonly SessionEntry[],
): SessionEntry[] {
  const entries = [...persistedEntries];
  const matchedPersisted = new Set<number>();
  for (const live of liveEntries) {
    let existingIndex = -1;
    if (live.kind === "tool" && live.callId) {
      existingIndex = entries.findIndex((entry, index) => (
        !matchedPersisted.has(index)
        && entry.kind === "tool"
        && entry.callId === live.callId
      ));
    }
    if (existingIndex < 0) {
      existingIndex = entries.findIndex((entry, index) => (
        !matchedPersisted.has(index) && sessionEntriesEquivalent(entry, live)
      ));
    }
    if (existingIndex >= 0) {
      matchedPersisted.add(existingIndex);
      const existing = entries[existingIndex];
      if (existing.kind === "message" && live.kind === "message" && live.text.length > existing.text.length) {
        entries[existingIndex] = live;
      } else if (existing.kind === "reasoning" && live.kind === "reasoning" && live.text.length > existing.text.length) {
        entries[existingIndex] = live;
      } else if (existing.kind === "tool" && live.kind === "tool") {
        entries[existingIndex] = mergeToolEntries(existing, live);
      }
      continue;
    }
    entries.push(live);
  }
  return entries.slice(-80);
}

function mergeToolEntries(
  persisted: SessionToolEntry,
  live: SessionToolEntry,
): SessionToolEntry {
  const state: ToolState = persisted.state === "fail" || live.state === "fail"
    ? "fail"
    : persisted.state === "ok" || live.state === "ok"
      ? "ok"
      : "active";
  return {
    ...persisted,
    callId: persisted.callId ?? live.callId,
    inputJson: persisted.inputJson ?? live.inputJson,
    outputJson: live.outputJson ?? persisted.outputJson,
    state,
  };
}

function appendMessageEntry(entries: SessionEntry[], entry: SessionMessageEntry) {
  const previous = entries[entries.length - 1];
  if (previous?.kind === "message" && previous.tone === entry.tone) {
    entries[entries.length - 1] = {
      ...previous,
      text: joinVisibleMessageText(previous.text, entry.text),
    };
    return;
  }
  entries.push(entry);
}

function appendReasoningEntry(entries: SessionEntry[], entry: SessionReasoningEntry) {
  const previous = entries[entries.length - 1];
  if (previous?.kind === "reasoning") {
    entries[entries.length - 1] = {
      ...previous,
      text: joinVisibleMessageText(previous.text, entry.text),
    };
    return;
  }
  entries.push(entry);
}

function joinVisibleMessageText(left: string, right: string): string {
  if (!left) return right;
  if (!right) return left;
  if (/\s$/.test(left) || /^\s/.test(right)) return `${left}${right}`.trim();
  const leftChar = left[left.length - 1] ?? "";
  const rightChar = right[0] ?? "";
  const cjk = /[\u3400-\u9fff]/;
  const punctuation = /[，。！？；：、,.!?;:)}\]》」』]/;
  if ((cjk.test(leftChar) && cjk.test(rightChar)) || punctuation.test(rightChar)) {
    return `${left}${right}`;
  }
  return `${left} ${right}`;
}

export function groupTranscriptEntries(
  entries: readonly (SessionMessageEntry | SessionReasoningEntry | SessionToolEntry)[],
): TranscriptBlock[] {
  const blocks: TranscriptBlock[] = [];
  let pendingTools: SessionToolEntry[] = [];
  let pendingKind: ToolActivityKind | null = null;
  const flushTools = () => {
    if (pendingTools.length === 1) blocks.push(pendingTools[0]);
    else if (pendingTools.length > 1) {
      blocks.push({
        id: `tool-group-${pendingKind}-${pendingTools[0].id}`,
        kind: "tool_group",
        groupKind: pendingKind ?? "tool",
        tools: pendingTools,
      });
    }
    pendingTools = [];
    pendingKind = null;
  };
  for (const entry of entries) {
    if (entry.kind === "tool") {
      const kind = toolActivityKind(entry.toolName);
      if (pendingTools.length > 0 && pendingKind !== kind) flushTools();
      pendingKind = kind;
      pendingTools.push(entry);
    } else {
      flushTools();
      blocks.push(entry);
    }
  }
  flushTools();
  return blocks;
}

function runtimeName(runtimeKind: AgentRun["runtime_kind"]): "R-Code" | "Codex" {
  return runtimeKind === "native" ? "R-Code" : "Codex";
}

function runtimeExecutorName(runtimeKind: AgentRun["runtime_kind"]): "R-Code" | "Codex CLI" | "Codex MCP" {
  if (runtimeKind === "codex_exec") return "Codex CLI";
  if (runtimeKind === "codex_mcp") return "Codex MCP";
  return "R-Code";
}

function sessionEntriesEquivalent(left: SessionEntry, right: SessionEntry): boolean {
  if (left.kind !== right.kind) return false;
  if (left.kind === "tool" && right.kind === "tool") {
    if (left.callId && right.callId) return left.callId === right.callId;
    return left.toolName === right.toolName && left.summary === right.summary;
  }
  if (left.kind === "message" && right.kind === "message") {
    return left.text === right.text || left.text.startsWith(right.text) || right.text.startsWith(left.text);
  }
  if (left.kind === "reasoning" && right.kind === "reasoning") {
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

function resolveSessionPermission(
  messages: readonly SessionMessage[],
  fallback: SubagentPermission,
): SubagentPermission {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message.kind !== "system" || message.text !== "subagent_lifecycle") continue;
    const data = parseObject(message.output_json);
    const scope = data?.scope && typeof data.scope === "object" && !Array.isArray(data.scope)
      ? data.scope as Record<string, unknown>
      : null;
    const value = typeof data?.access_mode === "string"
      ? data.access_mode
      : scope
        ? scope.access_mode
        : null;
    if (value === "full_access" || value === "read_only") {
      const requireApproval = typeof data?.require_approval === "boolean"
        ? data.require_approval
        : typeof scope?.require_approval === "boolean"
          ? scope.require_approval
          : false;
      return {
        accessMode: value,
        requireApproval: value === "full_access" && requireApproval,
      };
    }
  }
  return fallback;
}

function effectivePermissionMode(permission: SubagentPermission): SubagentPermissionMode {
  if (permission.accessMode === "read_only") return "read_only";
  return permission.requireApproval ? "request_approval" : "full_access";
}

function permissionModeLabel(mode: SubagentPermissionMode): string {
  if (mode === "read_only") return "只读";
  if (mode === "request_approval") return "需审批";
  return "完全访问";
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

function liveStateLabel(status: SubagentStatus, failedToolCount = 0): string {
  switch (status) {
    case "queued": return "正在等待调度";
    case "running": return "正在继续运行";
    case "waiting_permission": return "正在等待权限";
    case "completed": return failedToolCount > 0
      ? `运行已完成 · ${failedToolCount} 项操作失败`
      : "运行已完成";
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

function listObservation(child: ActivitySubagent): string {
  const value = observation(child).replace(/\s+/g, " ").trim();
  if (value.startsWith("{") || value.startsWith("[")) return `${statusLabel(child.status)}，打开详情查看结构化结果`;
  return value.length > 180 ? `${value.slice(0, 179).trimEnd()}…` : value;
}

function listTime(child: ActivitySubagent, now: number): string {
  if (isActive(child.status)) return elapsedCompact(child.startedAt, now);
  return relativeCompact(child.endedAt ?? child.lastEventAt, now);
}

function listTimeTitle(child: ActivitySubagent): string {
  const endedAt = child.endedAt ?? child.lastEventAt;
  const duration = elapsedCompact(child.startedAt, endedAt);
  if (isActive(child.status)) return `已运行 ${duration}`;
  return `${statusLabel(child.status)}于 ${new Date(endedAt).toLocaleString("zh-CN")} · 总耗时 ${duration}`;
}

function relativeCompact(timestamp: number, now: number): string {
  const seconds = Math.max(0, Math.floor((now - timestamp) / 1000));
  if (seconds < 60) return "刚刚";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}分钟前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}小时前`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days}天前`;
  const months = Math.floor(days / 30);
  return months < 12 ? `${months}个月前` : `${Math.floor(months / 12)}年前`;
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
