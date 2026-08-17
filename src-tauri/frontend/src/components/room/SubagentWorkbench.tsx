import { memo, useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { subagentSessionMessagePage } from "../../lib/ipc";
import { usePoll } from "../../lib/poll";
import { useSharedNow } from "../../lib/shared-clock";
import type {
  AgentRun,
  SessionMessage,
  SubagentAccessMode,
  SubagentSessionCallIdUpdate,
} from "../../lib/types";
import {
  IconActivity,
  IconCheck,
  IconChevronDown,
  IconChevronLeft,
  IconClose,
  IconFile,
  IconHistory,
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
  /** 主运行 ID 用来区分正常顶层委派与父节点已丢失；旧数据可传空数组。 */
  rootRunIds: readonly string[];
  selectedSubagentId: string | null;
  openSubagentIds: readonly string[];
  toolTabsBefore: ReactNode;
  toolTabsAfter: ReactNode;
  onSelect: (subagentId: string) => void;
  onBack: () => void;
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

export type SubagentTreeAnomaly = "orphan" | "cycle" | null;

export interface SubagentTreeNode {
  child: ActivitySubagent;
  children: SubagentTreeNode[];
  /** 顶层子代理为 1；仅由已验证无环的 parentRunId 关系推导。 */
  depth: number;
  descendantCount: number;
  activeDescendantCount: number;
  anomaly: SubagentTreeAnomaly;
  missingParentId: string | null;
}

const DEFAULT_SUBAGENT_SECTION_EXPANSION: SubagentSectionExpansion = {
  attention: true,
  active: true,
  completed: false,
  incomplete: true,
};

/**
 * 将持久化运行关系变成可安全显示的森林。
 * 未知父节点提升为 root；self-parent / 多节点环的每个环成员都断边。
 */
export function buildSubagentForest(
  subagents: readonly ActivitySubagent[],
  rootRunIds: readonly string[] = [],
): SubagentTreeNode[] {
  const ordered: ActivitySubagent[] = [];
  const byId = new Map<string, ActivitySubagent>();
  for (const child of subagents) {
    if (!child.id || byId.has(child.id)) continue;
    byId.set(child.id, child);
    ordered.push(child);
  }

  const knownRoots = new Set(rootRunIds.filter(Boolean));
  const parentById = new Map<string, string | null>();
  const anomalyById = new Map<string, SubagentTreeAnomaly>();
  const missingParentById = new Map<string, string | null>();
  for (const child of ordered) {
    const parentId = child.parentRunId?.trim() || null;
    anomalyById.set(child.id, null);
    missingParentById.set(child.id, null);
    if (!parentId || knownRoots.has(parentId)) {
      parentById.set(child.id, null);
    } else if (!byId.has(parentId)) {
      parentById.set(child.id, null);
      anomalyById.set(child.id, "orphan");
      missingParentById.set(child.id, parentId);
    } else {
      parentById.set(child.id, parentId);
    }
  }

  // 每个节点最多一个 parent；局部 path map 可确定性找出全部环成员。
  const completed = new Set<string>();
  for (const child of ordered) {
    if (completed.has(child.id)) continue;
    const path: string[] = [];
    const position = new Map<string, number>();
    let current: string | null = child.id;
    while (current && !completed.has(current)) {
      const cycleStart = position.get(current);
      if (cycleStart != null) {
        for (const cycleId of path.slice(cycleStart)) {
          parentById.set(cycleId, null);
          anomalyById.set(cycleId, "cycle");
        }
        break;
      }
      position.set(current, path.length);
      path.push(current);
      current = parentById.get(current) ?? null;
    }
    for (const id of path) completed.add(id);
  }

  const nodeById = new Map<string, SubagentTreeNode>();
  for (const child of ordered) {
    nodeById.set(child.id, {
      child,
      children: [],
      depth: 1,
      descendantCount: 0,
      activeDescendantCount: 0,
      anomaly: anomalyById.get(child.id) ?? null,
      missingParentId: missingParentById.get(child.id) ?? null,
    });
  }

  const roots: SubagentTreeNode[] = [];
  for (const child of ordered) {
    const node = nodeById.get(child.id);
    if (!node) continue;
    const parent = parentById.get(child.id);
    const parentNode = parent ? nodeById.get(parent) : undefined;
    if (parentNode) parentNode.children.push(node);
    else roots.push(node);
  }

  // 迭代计算深度与后代数，避免异常长链触发 JS 递归栈溢出。
  const traversal: SubagentTreeNode[] = [];
  const stack = roots.slice().reverse().map((node) => ({ node, depth: 1 }));
  while (stack.length > 0) {
    const next = stack.pop();
    if (!next) break;
    next.node.depth = next.depth;
    traversal.push(next.node);
    for (let index = next.node.children.length - 1; index >= 0; index -= 1) {
      stack.push({ node: next.node.children[index], depth: next.depth + 1 });
    }
  }
  for (let index = traversal.length - 1; index >= 0; index -= 1) {
    const node = traversal[index];
    node.descendantCount = node.children.reduce(
      (count, descendant) => count + 1 + descendant.descendantCount,
      0,
    );
    node.activeDescendantCount = node.children.reduce(
      (count, descendant) => count
        + (isActive(descendant.child.status) ? 1 : 0)
        + descendant.activeDescendantCount,
      0,
    );
  }
  return roots;
}

export function flattenSubagentForest(roots: readonly SubagentTreeNode[]): SubagentTreeNode[] {
  const flattened: SubagentTreeNode[] = [];
  const stack = [...roots].reverse();
  while (stack.length > 0) {
    const node = stack.pop();
    if (!node) break;
    flattened.push(node);
    for (let index = node.children.length - 1; index >= 0; index -= 1) {
      stack.push(node.children[index]);
    }
  }
  return flattened;
}

/**
 * 子智能体总览和每个子智能体会话都是独立标签页。
 * 标签以稳定的运行 ID 去重；重新打开已有会话时只切换激活项。
 */
export function SubagentWorkbench({
  taskId,
  workspacePath,
  subagents,
  rootRunIds,
  selectedSubagentId,
  openSubagentIds,
  toolTabsBefore,
  toolTabsAfter,
  onSelect,
  onBack,
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
  const forest = useMemo(
    () => buildSubagentForest(subagents, rootRunIds),
    [rootRunIds, subagents],
  );
  const treeNodeById = useMemo(
    () => new Map(flattenSubagentForest(forest).map((node) => [node.child.id, node])),
    [forest],
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
        ? (
          <SubagentInspector
            key={`${taskId}:${selected.id}`}
            taskId={taskId}
            workspacePath={workspacePath}
            child={selected}
            treeNode={treeNodeById.get(selected.id) ?? null}
            index={selectedIndex}
            onBack={onBack}
            onAbort={onAbort}
          />
        )
        : (
          <SubagentList
            forest={forest}
            allChildren={subagents}
            expandedSections={expandedSections}
            onSelect={onSelect}
            onAbort={onAbort}
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
  forest,
  allChildren,
  expandedSections,
  onSelect,
  onAbort,
  onToggleSection,
}: {
  forest: readonly SubagentTreeNode[];
  allChildren: readonly ActivitySubagent[];
  expandedSections: Readonly<SubagentSectionExpansion>;
  onSelect: (subagentId: string) => void;
  onAbort: (subagentId: string) => Promise<void>;
  onToggleSection: (kind: SubagentSectionKind) => void;
}) {
  const [stoppingTreeId, setStoppingTreeId] = useState<string | null>(null);
  const [abortError, setAbortError] = useState<string | null>(null);
  const { attention, active, completed, incomplete, indexById } = useMemo(() => {
    const sections: Record<SubagentSectionKind, SubagentTreeNode[]> = {
      attention: [],
      active: [],
      completed: [],
      incomplete: [],
    };
    for (const root of forest) sections[treeSectionKind(root)].push(root);
    sections.active.sort((a, b) => b.child.startedAt - a.child.startedAt || a.child.id.localeCompare(b.child.id));
    sections.completed.sort((a, b) => treeLatestAt(b) - treeLatestAt(a));
    sections.incomplete.sort((a, b) => treeLatestAt(b) - treeLatestAt(a));
    return {
      ...sections,
      indexById: new Map(allChildren.map((child, index) => [child.id, index])),
    };
  }, [allChildren, forest]);
  const live = active.length + attention.length > 0;
  const now = useSharedNow(allChildren.length === 0 ? null : live ? 1000 : 60_000);
  const abortTree = useCallback(async (node: SubagentTreeNode) => {
    if (stoppingTreeId) return;
    setStoppingTreeId(node.child.id);
    setAbortError(null);
    try {
      await onAbort(node.child.id);
    } catch (cause) {
      setAbortError(String(cause));
    } finally {
      setStoppingTreeId(null);
    }
  }, [onAbort, stoppingTreeId]);

  if (allChildren.length === 0) {
    return (
      <div className="subagent-list-empty">
        <strong>还没有子智能体</strong>
        <p>主代理委派并行任务后，运行状态和结果会出现在这里。</p>
      </div>
    );
  }

  return (
    <div className="subagent-workbench-list" aria-label="子智能体列表">
      {abortError && <div className="subagent-tree-error" role="alert">停止子代理分支失败：{abortError}</div>}
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
          onAbort={abortTree}
          stoppingTreeId={stoppingTreeId}
          onToggle={() => onToggleSection("attention")}
        />
      )}
      {active.length > 0 && (
        <SubagentListSection
          key="active"
          kind="active"
          title="正在运行"
          children={active}
          indexById={indexById}
          now={now}
          expanded={expandedSections.active}
          onSelect={onSelect}
          onAbort={abortTree}
          stoppingTreeId={stoppingTreeId}
          onToggle={() => onToggleSection("active")}
        />
      )}
      {completed.length > 0 && (
        <SubagentListSection
          key="completed"
          kind="completed"
          title="已结束"
          children={completed}
          indexById={indexById}
          now={now}
          expanded={expandedSections.completed}
          onSelect={onSelect}
          onAbort={abortTree}
          stoppingTreeId={stoppingTreeId}
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
          onAbort={abortTree}
          stoppingTreeId={stoppingTreeId}
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
  onAbort,
  stoppingTreeId,
  onToggle,
}: {
  kind: SubagentSectionKind;
  title: string;
  children: readonly SubagentTreeNode[];
  indexById: ReadonlyMap<string, number>;
  now: number;
  expanded: boolean;
  onSelect: (subagentId: string) => void;
  onAbort: (node: SubagentTreeNode) => Promise<void>;
  stoppingTreeId: string | null;
  onToggle: () => void;
}) {
  const sectionId = useId();
  const titleId = `${sectionId}-title`;
  const rowsId = `${sectionId}-rows`;
  const flatChildren = useMemo(() => flattenSubagentForest(children), [children]);

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
        aria-label={`${expanded ? "收起" : "展开"}${title}子代理，当前 ${flatChildren.length} 个`}
        title={expanded ? `收起${title}` : `展开${title}`}
        onClick={onToggle}
      >
        <span className="subagent-list-section-indicator" aria-hidden="true" />
        <span className="subagent-list-section-title" id={titleId}>{title}</span>
        <span className="subagent-list-section-count" aria-hidden="true">
          {String(flatChildren.length)}
        </span>
        <IconChevronDown className="subagent-list-section-chevron" width={14} height={14} aria-hidden="true" />
      </button>
      <div className="subagent-list-rows" id={rowsId} hidden={!expanded}>
        {flatChildren.map((node) => {
          const child = node.child;
          const branchActive = isActive(child.status) || node.activeDescendantCount > 0;
          const canAbortBranch = node.descendantCount > 0 && branchActive;
          const stopping = stoppingTreeId === child.id;
          const anomaly = treeAnomalyLabel(node);
          return (
            <div
              className={`subagent-list-row status-${child.status}${node.depth > 1 ? " is-descendant" : ""}${node.anomaly ? ` has-${node.anomaly}` : ""}`}
              key={child.id}
              data-tree-depth={node.depth}
              data-tree-anomaly={node.anomaly ?? undefined}
              style={{ "--subagent-tree-depth": Math.max(0, node.depth - 1) } as CSSProperties}
              onClick={() => onSelect(child.id)}
            >
              <button
                type="button"
                className="subagent-list-row-select"
                onClick={(event) => {
                  event.stopPropagation();
                  onSelect(child.id);
                }}
                aria-label={`${child.label}，深度 ${node.depth}，${statusLabel(child.status)}${anomaly ? `，${anomaly}` : ""}`}
              >
                <SubagentStateMark status={child.status} />
                <span className="subagent-list-row-copy">
                  <strong title={child.label}>{child.label}</strong>
                  <small title={listObservation(child)}>{listObservation(child)}</small>
                  <span className="subagent-tree-facts" aria-label={treeFactsAriaLabel(node)}>
                    <span>深度 {node.depth}</span>
                    <span>{subagentSlotLabel(child)}</span>
                    <span>{runtimeExecutorName(child.runtimeKind)}</span>
                    <span>{child.model || "模型未记录"}</span>
                    {subagentCapabilityLabels(child).map((capability) => (
                      <span className="is-capability" key={capability}>{capability}</span>
                    ))}
                    {peerMessageActivity(child) && (
                      <span className="is-peer">PeerMessage {peerMessageActivity(child)}</span>
                    )}
                    {anomaly && <span className="is-anomaly">{anomaly}</span>}
                  </span>
                </span>
                <span className="subagent-list-row-meta" title={listTimeTitle(child)}>
                  <time>{listTime(child, now)}</time>
                </span>
              </button>
              {canAbortBranch && (
                <button
                  type="button"
                  className="subagent-tree-abort"
                  disabled={stoppingTreeId != null}
                  aria-label={`停止${child.label}及其 ${node.descendantCount} 个后代`}
                  title={`递归停止此节点及 ${node.descendantCount} 个后代；兄弟节点不受影响`}
                  onClick={(event) => {
                    event.stopPropagation();
                    void onAbort(node);
                  }}
                >
                  <IconStop width={11} height={11} aria-hidden="true" />
                  {stopping ? "停止中…" : "停止分支"}
                </button>
              )}
            </div>
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
  treeNode,
  index,
  onBack,
  onAbort,
}: {
  taskId: string;
  workspacePath: string | null;
  child: ActivitySubagent;
  treeNode: SubagentTreeNode | null;
  index: number;
  onBack: () => void;
  onAbort: (subagentId: string) => Promise<void>;
}) {
  const [messages, setMessages] = useState<SessionMessage[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(true);
  const [stopping, setStopping] = useState(false);
  const [hasMoreBefore, setHasMoreBefore] = useState(false);
  const [loadingEarlier, setLoadingEarlier] = useState(false);
  const sessionGenerationRef = useRef(0);
  const nextCursorRef = useRef<string | null>(null);
  const previousCursorRef = useRef<string | null>(null);
  const requestTailRef = useRef<Promise<void>>(Promise.resolve());
  const scrollRef = useRef<HTMLDivElement>(null);
  const prependAnchorRef = useRef<{ height: number; top: number } | null>(null);
  const active = isActive(child.status);
  const branchActive = active || (treeNode?.activeDescendantCount ?? 0) > 0;
  const descendantCount = treeNode?.descendantCount ?? 0;

  const enqueuePageRequest = useCallback(<T,>(operation: () => Promise<T>): Promise<T> => {
    const result = requestTailRef.current.then(operation, operation);
    requestTailRef.current = result.then(() => undefined, () => undefined);
    return result;
  }, []);

  const load = useCallback(async () => {
    const generation = sessionGenerationRef.current;
    try {
      await enqueuePageRequest(async () => {
        if (generation !== sessionGenerationRef.current) return;
        const afterCursor = nextCursorRef.current;
        const page = await subagentSessionMessagePage(
          taskId,
          child.id,
          afterCursor ? { after_cursor: afterCursor, limit: 80 } : { limit: 80 },
        );
        if (generation !== sessionGenerationRef.current) return;

        const initial = afterCursor == null;
        if (page.reset || initial) {
          setMessages((current) => mergeSessionMessagePage(
            current,
            page.messages,
            "replace",
            page.call_id_updates,
          ));
          previousCursorRef.current = page.previous_cursor ?? null;
          setHasMoreBefore(page.has_more_before);
        } else {
          if (!page.unchanged || page.call_id_updates.length > 0 || page.messages.length > 0) {
            setMessages((current) => mergeSessionMessagePage(
              current,
              page.messages,
              "append",
              page.call_id_updates,
            ));
          }
          // An append cursor can retain a wider loaded history window than the original page.
          // Refresh the historical cursor too, otherwise “load earlier” would keep requesting the
          // same boundary after new records arrive.
          if (page.previous_cursor !== undefined) {
            previousCursorRef.current = page.previous_cursor;
            setHasMoreBefore(page.has_more_before);
          }
        }
        nextCursorRef.current = page.next_cursor ?? (page.reset ? null : afterCursor);
        setError(null);
        setLoading(false);
      });
    } catch (cause) {
      if (generation === sessionGenerationRef.current) {
        setError(String(cause));
        setLoading(false);
      }
    }
  }, [child.id, enqueuePageRequest, taskId]);

  const loadEarlier = useCallback(async () => {
    const generation = sessionGenerationRef.current;
    const beforeCursor = previousCursorRef.current;
    if (!beforeCursor || loadingEarlier) return;
    setLoadingEarlier(true);
    setError(null);
    try {
      await enqueuePageRequest(async () => {
        if (generation !== sessionGenerationRef.current) return;
        const page = await subagentSessionMessagePage(taskId, child.id, {
          before_cursor: beforeCursor,
          limit: 80,
        });
        if (generation !== sessionGenerationRef.current) return;

        if (page.reset) {
          prependAnchorRef.current = null;
          setMessages((current) => mergeSessionMessagePage(
            current,
            page.messages,
            "replace",
            page.call_id_updates,
          ));
          nextCursorRef.current = page.next_cursor ?? null;
        } else if (!page.unchanged || page.call_id_updates.length > 0 || page.messages.length > 0) {
          const element = scrollRef.current;
          prependAnchorRef.current = element
            ? { height: element.scrollHeight, top: element.scrollTop }
            : null;
          setMessages((current) => mergeSessionMessagePage(
            current,
            page.messages,
            "prepend",
            page.call_id_updates,
          ));
        }
        previousCursorRef.current = page.previous_cursor ?? null;
        setHasMoreBefore(page.has_more_before);
        setError(null);
      });
    } catch (cause) {
      if (generation === sessionGenerationRef.current) setError(String(cause));
    } finally {
      if (generation === sessionGenerationRef.current) setLoadingEarlier(false);
    }
  }, [child.id, enqueuePageRequest, loadingEarlier, taskId]);

  useEffect(() => {
    sessionGenerationRef.current += 1;
    requestTailRef.current = Promise.resolve();
    nextCursorRef.current = null;
    previousCursorRef.current = null;
    prependAnchorRef.current = null;
    setMessages([]);
    setLoading(true);
    setLoadingEarlier(false);
    setHasMoreBefore(false);
    setError(null);
    setExpanded(true);
    return () => {
      sessionGenerationRef.current += 1;
    };
  }, [child.id, taskId]);

  useLayoutEffect(() => {
    const anchor = prependAnchorRef.current;
    const element = scrollRef.current;
    if (!anchor || !element) return;
    // Assign an absolute target. This is correct whether Chromium's native scroll anchoring has
    // already fired or not, and keeps the first visible record stationary while history prepends.
    element.scrollTop = anchor.top + (element.scrollHeight - anchor.height);
    prependAnchorRef.current = null;
  }, [messages]);

  usePoll(load, 1600, active, "子代理会话");

  useEffect(() => {
    // usePoll performs the initial active read. A completed run is read once here, and the active →
    // terminal transition gets one final incremental read so the durable tail cannot be missed.
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
    if (stopping || !branchActive) return;
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
    <div className="subagent-detail-body" ref={scrollRef}>
      <nav className="subagent-detail-navigation" aria-label="子代理详情导航">
        <button
          type="button"
          className="subagent-detail-back"
          onClick={onBack}
          aria-label="返回运行与子代理"
          title="返回运行与子代理"
        >
          <IconChevronLeft width={14} height={14} aria-hidden="true" />
          <span>运行与子代理</span>
        </button>
      </nav>
      <article className="subagent-session">
        <button
          type="button"
          className="subagent-session-summary"
          aria-expanded={expanded}
          aria-controls={`subagent-session-${child.id}`}
          onClick={() => setExpanded((value) => !value)}
        >
          <SubagentElapsedTime
            active={active}
            endedAt={child.endedAt}
            startedAt={child.startedAt}
          />
          <span className={`subagent-session-permission mode-${permissionMode}`}>{permissionModeLabel(permissionMode)}</span>
          <IconChevronDown width={13} height={13} />
        </button>

        {expanded && (
          <div className="subagent-session-body" id={`subagent-session-${child.id}`}>
            <div className="subagent-session-meta" aria-label="子智能体编排信息">
              <span><b>深度</b>{treeNode?.depth ?? 1}</span>
              <span><b>槽位</b>{subagentSlotLabel(child).replace(/^槽位\s*/, "")}</span>
              <span><b>Provider</b>{runtimeExecutorName(child.runtimeKind)}</span>
              <span><b>模型</b>{child.model || "未记录"}</span>
              <span><b>能力</b>{subagentCapabilityLabels(child).join(" · ")}</span>
              <span><b>权限</b>{permissionModeLabel(permissionMode)}</span>
              {peerMessageActivity(child) && <span><b>PeerMessage</b>{peerMessageActivity(child)}</span>}
              {treeNode?.anomaly && (
                <span className="subagent-tree-anomaly"><b>运行关系</b>{treeAnomalyLabel(treeNode)}</span>
              )}
              {child.routingReason && <span className="subagent-routing-reason"><b>路由</b>{child.routingReason}</span>}
            </div>
            {error && <div className="subagent-session-error">读取子智能体记录失败：{error}</div>}
            {hasMoreBefore && (
              <button
                type="button"
                className="subagent-load-earlier"
                disabled={loadingEarlier}
                onClick={() => void loadEarlier()}
              >
                <IconHistory width={13} height={13} aria-hidden="true" />
                {loadingEarlier ? "正在加载…" : "加载更早记录"}
              </button>
            )}
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
              <SubagentTranscript
                blocks={transcriptBlocks}
                childId={child.id}
                index={index}
                runtimeKind={child.runtimeKind}
                taskId={taskId}
                workspacePath={workspacePath}
              />
            )}
            <div className={`subagent-session-state status-${child.status}${failedToolCount > 0 ? " has-tool-failures" : ""}`} role="status" aria-live="polite">
              <SubagentStateMark status={child.status} />
              <span>{liveStateLabel(child.status, failedToolCount)}</span>
              {branchActive && (
                <button type="button" disabled={stopping} onClick={() => void stop()}>
                  <IconStop width={11} height={11} /> {stopping
                    ? "停止中…"
                    : descendantCount > 0
                      ? `停止此节点及 ${descendantCount} 个后代`
                      : "停止"}
                </button>
              )}
            </div>
          </div>
        )}
      </article>
    </div>
  );
}

function treeSectionKind(root: SubagentTreeNode): SubagentSectionKind {
  const nodes = flattenSubagentForest([root]);
  if (nodes.some((node) => node.child.status === "waiting_permission")) return "attention";
  if (nodes.some((node) => node.child.status === "queued" || node.child.status === "running")) return "active";
  if (root.child.status === "completed") return "completed";
  return "incomplete";
}

function treeLatestAt(root: SubagentTreeNode): number {
  return flattenSubagentForest([root]).reduce(
    (latest, node) => Math.max(latest, node.child.endedAt ?? node.child.lastEventAt),
    0,
  );
}

const SubagentElapsedTime = memo(function SubagentElapsedTime({
  active,
  endedAt,
  startedAt,
}: {
  active: boolean;
  endedAt: number | null;
  startedAt: number;
}) {
  const now = useSharedNow(active ? 1000 : null);
  return <span>已处理 {elapsedCompact(startedAt, endedAt ?? now)}</span>;
});

const SubagentTranscript = memo(function SubagentTranscript({
  blocks,
  childId,
  index,
  runtimeKind,
  taskId,
  workspacePath,
}: {
  blocks: readonly TranscriptBlock[];
  childId: string;
  index: number;
  runtimeKind: AgentRun["runtime_kind"];
  taskId: string;
  workspacePath: string | null;
}) {
  return (
    <div className={`subagent-transcript${blocks.length >= 24 ? " is-long" : ""}`} aria-label="子智能体公开输出">
      {blocks.map((entry) => entry.kind === "tool_group" ? (
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
              identity={childId}
              runtimeKind={runtimeKind}
              size="xs"
            />
            <span>{runtimeName(runtimeKind)} 子智能体</span>
          </div>
          <Markdown text={entry.text} taskId={taskId} workspacePath={workspacePath} />
        </article>
      ))}
    </div>
  );
});

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
  const [open, setOpen] = useState(false);
  const preview = reasoningPreview(entry.text);
  return (
    <details
      className="subagent-reasoning-event"
      open={open}
      onToggle={(event) => {
        const nextOpen = event.currentTarget.open;
        if (nextOpen !== open) setOpen(nextOpen);
      }}
    >
      <summary>
        <span className="subagent-runtime-log-icon"><IconActivity width={13} height={13} /></span>
        <span>思考过程</span>
        <small title={preview}>{preview}</small>
        <IconChevronDown width={13} height={13} />
      </summary>
      {open && (
        <div className="subagent-reasoning-detail">
          <Markdown text={entry.text} taskId={taskId} workspacePath={workspacePath} />
        </div>
      )}
    </details>
  );
}

function reasoningPreview(value: string, sourceLimit = 320): string {
  // A summary is only a navigation hint. Bound the source before normalizing whitespace so a
  // collapsed multi-megabyte reasoning record does not get scanned and copied on every render.
  const source = value.slice(0, sourceLimit);
  const preview = source.replace(/\s+/g, " ").trim();
  return value.length > sourceLimit ? `${preview || "思考内容"}…` : preview;
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
          <ToolPayloadDetails
            toolName={entry.toolName}
            inputJson={entry.inputJson}
            outputJson={entry.outputJson}
            state={entry.state}
          />
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
    if (run.agent_kind !== "subagent") continue;
    const prior = merged.get(run.id);
    if (prior) {
      const parentRunId = run.parent_run_id?.trim() || null;
      if (prior.parentRunId !== parentRunId) merged.set(run.id, { ...prior, parentRunId });
      continue;
    }
    const startedAt = parseTimestamp(run.started_at);
    const endedAt = run.ended_at ? parseTimestamp(run.ended_at) : null;
    const accessMode = run.access_mode ?? "read_only";
    merged.set(run.id, {
      id: run.id,
      parentRunId: run.parent_run_id?.trim() || null,
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
    if (event.kind === "peer_message") {
      const sender = compactText(event.peerSenderAgentId, 80);
      const recipient = compactText(event.peerRecipientAgentId, 80);
      const status = event.peerStatus === "delivered" ? "已送达" : "已排队";
      pushUniqueStatus(entries, {
        id: event.id,
        kind: "status",
        text: `发送消息${event.peerContentChars != null ? `（${event.peerContentChars} 字符）` : ""} · ${sender ?? "当前子代理"} → ${recipient ?? "目标子代理"} · ${status}`,
        tone: "normal",
      });
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
    if (message.kind === "system" && message.text === "r_code_interim") {
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
  return entries;
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
  return entries;
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

function sessionMessageEqual(a: SessionMessage, b: SessionMessage): boolean {
  return a.id === b.id
    && a.branch_id === b.branch_id
    && a.kind === b.kind
    && a.role === b.role
    && a.text === b.text
    && a.image_count === b.image_count
    && stringListsEqual(a.image_media_types, b.image_media_types)
    && sessionAttachmentsEqual(a.attachments, b.attachments)
    && a.call_id === b.call_id
    && a.tool_name === b.tool_name
    && a.input_json === b.input_json
    && a.output_json === b.output_json
    && a.is_error === b.is_error
    && a.timestamp === b.timestamp;
}

function stringListsEqual(left?: readonly string[], right?: readonly string[]): boolean {
  if (left === right) return true;
  if (!left || !right || left.length !== right.length) return false;
  return left.every((value, index) => value === right[index]);
}

function sessionAttachmentsEqual(
  left: SessionMessage["attachments"],
  right: SessionMessage["attachments"],
): boolean {
  if (left === right) return true;
  if (!left || !right || left.length !== right.length) return false;
  return left.every((value, index) => {
    const other = right[index];
    return value.name === other?.name
      && value.media_type === other.media_type
      && value.kind === other.kind;
  });
}

function sessionMessageListsEqual(
  left: readonly SessionMessage[],
  right: readonly SessionMessage[],
): boolean {
  if (left === right) return true;
  if (left.length !== right.length) return false;
  for (let index = 0; index < left.length; index += 1) {
    if (!sessionMessageEqual(left[index], right[index])) return false;
  }
  return true;
}

/** Merge one cursor page without replacing an unchanged array or duplicating boundary records. */
export function mergeSessionMessagePage(
  current: readonly SessionMessage[],
  incoming: readonly SessionMessage[],
  direction: "replace" | "prepend" | "append",
  callIdUpdates: readonly SubagentSessionCallIdUpdate[] = [],
): SessionMessage[] {
  const source = direction === "replace"
    ? incoming
    : direction === "prepend"
      ? [...incoming, ...current]
      : [...current, ...incoming];
  const merged: SessionMessage[] = [];
  const positions = new Map<string, number>();

  for (const message of source) {
    // Host projections always have physical-line ids. Preserve anonymous records independently so
    // two identical legacy messages are never collapsed merely because their payloads match.
    if (!message.id) {
      merged.push(message);
      continue;
    }
    const existing = positions.get(message.id);
    if (existing == null) {
      positions.set(message.id, merged.length);
      merged.push(message);
    } else if (!sessionMessageEqual(merged[existing], message)) {
      merged[existing] = message;
    }
  }

  for (const update of callIdUpdates) {
    const position = positions.get(update.id);
    if (position == null || merged[position].call_id === update.call_id) continue;
    merged[position] = { ...merged[position], call_id: update.call_id };
  }

  return sessionMessageListsEqual(current, merged) ? current as SessionMessage[] : merged;
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

function runtimeName(runtimeKind: AgentRun["runtime_kind"]): string {
  switch (runtimeKind) {
    case "native": return "R-Code";
    case "codex_exec":
    case "codex_mcp": return "Codex";
  }
}

function runtimeExecutorName(runtimeKind: AgentRun["runtime_kind"]): string {
  switch (runtimeKind) {
    case "native": return "R-Code";
    case "codex_exec": return "Codex CLI";
    case "codex_mcp": return "Codex MCP";
  }
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

function visibleText(value: string | null | undefined): string | null {
  const normalized = value?.trim();
  return normalized || null;
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

function subagentSlotLabel(child: ActivitySubagent): string {
  const match = child.routingReason?.match(/(?:槽位|slot)\s*(?:[:#：=\-]\s*)?([\w\u4e00-\u9fff-]{1,32})/i);
  return match?.[1] ? `槽位 ${match[1]}` : "槽位 未记录";
}

function subagentCapabilityLabels(child: ActivitySubagent): string[] {
  return child.runtimeKind === "native"
    ? ["可继续委派", "可实时消息"]
    : ["叶节点"];
}

function peerMessageActivity(child: ActivitySubagent): string | null {
  const peerEvents = child.events.filter((event) => event.kind === "peer_message");
  if (peerEvents.length === 0) return null;
  const latest = peerEvents.reduce((current, event) => event.at >= current.at ? event : current);
  const sender = compactText(latest.peerSenderAgentId, 36) ?? "当前子代理";
  const recipient = compactText(latest.peerRecipientAgentId, 36) ?? "目标子代理";
  const size = latest.peerContentChars != null ? ` · ${latest.peerContentChars} 字符` : "";
  return `${peerEvents.length} 条 · ${sender} → ${recipient} · 最近${latest.peerStatus === "delivered" ? "已送达" : "已排队"}${size}`;
}

function treeAnomalyLabel(node: SubagentTreeNode): string | null {
  if (node.anomaly === "cycle") return "运行关系异常，循环已隔离";
  if (node.anomaly === "orphan") {
    const missingParent = compactText(node.missingParentId, 48);
    return missingParent ? `父节点不可用：${missingParent}` : "父节点不可用";
  }
  return null;
}

function treeFactsAriaLabel(node: SubagentTreeNode): string {
  const child = node.child;
  return [
    `深度 ${node.depth}`,
    subagentSlotLabel(child),
    `Provider ${runtimeExecutorName(child.runtimeKind)}`,
    `模型 ${child.model || "未记录"}`,
    ...subagentCapabilityLabels(child),
    peerMessageActivity(child) ? `PeerMessage ${peerMessageActivity(child)}` : null,
    treeAnomalyLabel(node),
  ].filter((value): value is string => Boolean(value)).join("，");
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
    case "running": return "运行中";
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
