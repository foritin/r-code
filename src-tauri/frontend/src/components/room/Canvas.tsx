/**
 * Room 右列任务工作台 —— 运行 / 文件 / 终端 / 审核 / Plan 等项目任务工具。
 * 激活页签来自 store.app.canvasTab（页签点击或 视图 菜单切换）。
 * Changes 以本轮记录为可操作范围，并合并当前项目的 Git 未提交变更作为只读上下文。
 */
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import type { FitAddon } from "@xterm/addon-fit";
import type { Terminal as XtermTerminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import {
  acceptTask,
  changeRequest,
  changeDiff,
  fileList,
  fileRead,
  fileWrite,
  gitCommitTask,
  gitDeliveryStatus,
  gitPushTask,
  gitStageAccepted,
  gitSuggestCommitMessage,
  permissionApprove,
  rollbackTask,
  rollbackTaskToCheckpoint,
  reviewAcceptAll,
  reviewAcceptFile,
  reviewAcceptLine,
  reviewGitStatus,
  reviewRejectFile,
  REVIEW_STATUS_CHANGED_EVENT,
  runVerification,
  sessionMessages,
  onTerminalOutput,
  terminalCreate,
  terminalCreateCodex,
  terminalKill,
  terminalList,
  terminalRawSince,
  terminalRawSnapshot,
  terminalResize,
  terminalSend,
  verificationList,
  verificationOutput,
  type FileContent,
  type FileTreeEntry,
} from "../../lib/ipc";
import { guardTripLabel } from "./model";
import { taskDisplayState, taskStateLabel } from "../../lib/presentation";
import type {
  AgentRun,
  ChangeDiff,
  ChangeDiffLine,
  FileChange,
  GitDeliveryStatus,
  GuardTripReason,
  PermissionDecision,
  ProjectAccessMode,
  ReviewGitStatus,
  SessionMessage,
  Task,
  TaskDetail,
  TerminalInfo,
  VerificationRecord,
} from "../../lib/types";
import {
  useAppStore,
  workbenchToolTab,
  type CanvasTab,
  type WorkbenchToolTab,
} from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { usePoll } from "../../lib/poll";
import { useArmedAction } from "../../lib/hooks";
import { isTypingTarget, keyLabel, useGlobalKeys, useSceneKeys } from "../../lib/keys";
import {
  clockSeconds,
  clockTime,
  displayPath,
  elapsedMinutes,
  elapsedSince,
  modeLabel,
  modeShortLabel,
  permissionAttribution,
  permissionRiskLabel,
  relativeAgo,
} from "../../lib/format";
import {
  activityBuckets,
  buildAuditFeed,
  buildKeyEvents,
  isRoutineAuditRow,
  summarizeToolComposition,
} from "./audit";
import type { ActivityTraceState } from "./activity";
import { SubagentAvatar, type SubagentAvatarStatus } from "./SubagentIdentity";
import {
  mergeSubagents,
  SubagentSessionTabs,
  SubagentWorkbench,
} from "./SubagentWorkbench";
import { handleWorkbenchTabListKeyDown } from "./workbench-tabs";
import { isLocalRasterReference, LocalImageArtifact } from "./LocalResource";
import {
  IconActivity,
  IconCheck,
  IconChevronDown,
  IconChevronLeft,
  IconChevronRight,
  IconClose,
  IconEditor,
  IconFile,
  IconMaximize,
  IconPlus,
  IconProjects,
  IconRefresh,
  IconShield,
  IconSidebar,
  IconTerminal,
} from "../icons";
import { Menu, MenuItem } from "../ui/Menu";
import { projectAccessModeLabel } from "../ProjectAccessSelector";
import { useCodexCliGate } from "../codex/CodexCliGate";
import { FileCodePreview } from "../files/FileCodePreview";
import { FileContextMenu, type FileContextMenuTarget } from "../files/FileContextMenu";
import { EnhancedReviewPanel } from "./EnhancedReviewPanel";
import { PlanPanel } from "../plan/PlanPanel";
import type { TaskPlanController } from "../plan/useTaskPlan";

interface Props {
  taskId: string;
  task: Task;
  planController: TaskPlanController;
  running: boolean;
  activity: ActivityTraceState;
  workspacePath: string | null;
  workspaceAttached: boolean;
  subagentPanelOpen: boolean;
  selectedSubagentId: string | null;
  openSubagentIds: readonly string[];
  onInspectSubagent: (subagentId: string) => void;
  onBackToSubagents: () => void;
  onCloseSubagentTab: (subagentId: string) => void;
  onCloseSubagents: () => void;
  onAbortSubagent: (subagentId: string) => Promise<void>;
  onTaskChanged?: () => Promise<void> | void;
}

const shortcutLabel = (action: Parameters<typeof keyLabel>[0]) => keyLabel(action).split(" ").join("+");

const TABS: { id: WorkbenchToolTab; openTab: CanvasTab; label: string; description: string; shortcut: string }[] = [
  { id: "summary", openTab: "summary", label: "运行与子代理", description: "查看运行状态、会话记录和子代理进度", shortcut: shortcutLabel("workbenchSummary") },
  { id: "terminal", openTab: "terminal", label: "终端", description: "打开任务级持久终端会话", shortcut: shortcutLabel("workbenchTerminal") },
  { id: "files", openTab: "files", label: "文件", description: "浏览并编辑当前工作区文件", shortcut: shortcutLabel("workbenchFiles") },
  { id: "review", openTab: "changes", label: "审核", description: "检查差异、运行验证并决定是否接受", shortcut: shortcutLabel("workbenchReview") },
  { id: "plan", openTab: "plan", label: "计划", description: "查看当前对话的步骤、进度与待确认问题", shortcut: "" },
];

const EMPTY_WORKBENCH_TABS: readonly WorkbenchToolTab[] = [];

// 仅保存纯 UI 会话状态；任务数据和终端进程仍由现有 store / IPC 持有。
// 面板切走会卸载，因此在应用生命周期内按 task + tool 恢复选择和未保存草稿。
const panelSessionCache = new Map<string, unknown>();

function readPanelSession<T>(key: string): T | undefined {
  return panelSessionCache.get(key) as T | undefined;
}

function useRememberPanelSession<T>(key: string, value: T): void {
  const latest = useRef(value);
  latest.current = value;
  useEffect(() => () => {
    if (panelSessionCache.size > 120 && !panelSessionCache.has(key)) {
      const oldest = panelSessionCache.keys().next().value as string | undefined;
      if (oldest) panelSessionCache.delete(oldest);
    }
    panelSessionCache.set(key, latest.current);
  }, [key]);
}

function ToolIcon({ tab, ...props }: { tab: CanvasTab; width?: number; height?: number }) {
  if (tab === "summary") return <IconActivity {...props} />;
  if (tab === "files") return <IconFile {...props} />;
  if (tab === "terminal") return <IconTerminal {...props} />;
  if (tab === "plan") return <IconEditor {...props} />;
  return <IconShield {...props} />;
}

// ---------- 列表键盘导航（三个面板共用） ----------
// 三个列表使用 roving tabindex：仅一行 tabIndex=0，方向键移动 DOM 焦点。

// Changes / Terminal 行内含操作按钮；listbox option 的子项会变成 presentational，
// 不适合承载交互控件，因此使用允许交互式 gridcell 的 grid / row 结构。
// Review 没有行内按钮，直接使用真正的 button。

// 焦点落在行本身，所以选择 roving tabindex，不再使用二选一的 aria-activedescendant。

const chgRowId = (index: number) => `chg-row-${index}`;
const termRowId = (index: number) => `term-row-${index}`;
const verRowId = (index: number) => `ver-row-${index}`;

/**
 * ↑ / ↓ 在行间循环移动焦点，Home / End 跳首尾。
 * 行自己用 onFocus 回写 roving 下标，这里只负责把 DOM 焦点挪过去。
 * 返回 true 表示按键已消费，调用方不必再处理。
 */
function moveRowFocus(
  event: React.KeyboardEvent<HTMLElement>,
  index: number,
  count: number,
  rowId: (index: number) => string
): boolean {
  if (count <= 0) return false;
  let next: number;
  if (event.key === "ArrowDown") next = (index + 1) % count;
  else if (event.key === "ArrowUp") next = (index - 1 + count) % count;
  else if (event.key === "Home") next = 0;
  else if (event.key === "End") next = count - 1;
  else return false;
  event.preventDefault();
  // 目标行此刻 tabIndex 还是 -1，但程序化 focus() 对 -1 同样有效。
  document.getElementById(rowId(next))?.focus();
  return true;
}

/** Enter / Space 激活；行内按钮（回滚 / 终止）冒泡上来的按键不算，交给它们自己处理。 */
function isRowActivate(event: React.KeyboardEvent<HTMLElement>): boolean {
  if (event.target !== event.currentTarget) return false;
  return event.key === "Enter" || event.key === " ";
}

/** roving tabindex 的落点：优先跟随最近一次焦点，否则回到选中行（都没有就第一行）。 */
function rovingIndexOf(focusIndex: number, selectedIndex: number, count: number): number {
  if (focusIndex >= 0 && focusIndex < count) return focusIndex;
  return selectedIndex >= 0 && selectedIndex < count ? selectedIndex : 0;
}

export function Canvas({
  taskId,
  task,
  planController,
  running,
  activity,
  workspacePath,
  workspaceAttached,
  subagentPanelOpen,
  selectedSubagentId,
  openSubagentIds,
  onInspectSubagent,
  onBackToSubagents,
  onCloseSubagentTab,
  onCloseSubagents,
  onAbortSubagent,
  onTaskChanged,
}: Props) {
  const tab = useAppStore((s) => s.canvasTab);
  const setTab = useAppStore((s) => s.setCanvasTab);
  const closeTab = useAppStore((s) => s.closeWorkbenchTab);
  const openTabs = useAppStore((s) => s.workbenches[taskId]?.openTabs ?? EMPTY_WORKBENCH_TABS);
  const mode = useAppStore((s) => s.workbenchMode);
  const launcherOpen = useAppStore((s) => s.workbenchLauncherOpen);
  const showLauncher = useAppStore((s) => s.showWorkbenchLauncher);
  const closeLauncher = useAppStore((s) => s.closeWorkbenchLauncher);
  const hideWorkbench = useAppStore((s) => s.hideWorkbench);
  const toggleFocus = useAppStore((s) => s.toggleWorkbenchFocus);
  const expandReview = useAppStore((s) => s.expandReview);
  const detail = useTasksStore((s) => s.details[taskId]);
  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const [reviewChangeCount, setReviewChangeCount] = useState<number | null>(null);
  useEffect(() => setReviewChangeCount(null), [taskId]);
  const displayedReviewChangeCount = reviewChangeCount ?? detail?.changes.length ?? 0;
  const workspace = useTasksStore((s) =>
    workspacePath ? s.workspaces.find((item) => item.canonical_path === workspacePath) : undefined,
  );
  const workingSubagents = activity.subagents.filter(
    (item) => item.status === "queued" || item.status === "running" || item.status === "waiting_permission"
  ).length;
  const launcherTriggerRef = useRef<HTMLButtonElement>(null);
  const launcherButtonsRef = useRef<Array<HTMLButtonElement | null>>([]);
  const workbenchBodyRef = useRef<HTMLDivElement>(null);
  const [launcherIndex, setLauncherIndex] = useState(0);
  const subagents = useMemo(
    () => mergeSubagents(activity.subagents, detail?.runs ?? []),
    [activity.subagents, detail?.runs],
  );
  const subagentRootRunIds = useMemo(
    () => (detail?.runs ?? []).filter((run) => run.agent_kind === "main").map((run) => run.id),
    [detail?.runs],
  );
  const subagentIds = useMemo(
    () => new Set(subagents.map((child) => child.id)),
    [subagents],
  );
  const hasOpenSubagentTabs = openSubagentIds.some((id) => subagentIds.has(id));

  const activateTool = (tool: CanvasTab) => {
    onCloseSubagents();
    setTab(tool);
    requestAnimationFrame(() => workbenchBodyRef.current?.focus());
  };

  const openByShortcut = (event: KeyboardEvent, index: number) => {
    if (!event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
    const target = TABS[index];
    if (!target) return;
    event.preventDefault();
    activateTool(target.openTab);
  };

  useGlobalKeys({
    workbenchSummary: () => activateTool("summary"),
    workbenchTerminal: () => activateTool("terminal"),
    workbenchFiles: () => activateTool("files"),
    workbenchReview: () => activateTool("changes"),
  });

  useSceneKeys({
    Escape: (event) => {
      if (mode !== "focus" || launcherOpen) return;
      event.preventDefault();
      toggleFocus();
    },
    "1": (event) => openByShortcut(event, 0),
    "2": (event) => openByShortcut(event, 1),
    "3": (event) => openByShortcut(event, 2),
    "4": (event) => openByShortcut(event, 3),
  });

  if (mode === "hidden") return null;

  if (mode === "collapsed") {
    return (
      <aside className="workbench-review-rail" data-testid="review-collapsed" aria-label="审核工作台已收起">
        <button type="button" className="workbench-review-rail-button" onClick={expandReview} aria-label="展开审核工作台">
          <span className="workbench-review-rail-icon"><IconShield width={19} height={19} /><b>{displayedReviewChangeCount}</b></span>
          <span>审核</span>
        </button>
        <span className="workbench-review-rail-spacer" />
        <i className="workbench-review-rail-status" aria-label="等待审核" />
      </aside>
    );
  }

  const activeToolId = workbenchToolTab(tab);
  const reviewIsPending = detail?.task.state === "review_ready";
  const subagentPageOpen = subagentPanelOpen && tab === "summary" && !launcherOpen;
  const hideWorkbenchPanel = () => {
    onCloseSubagents();
    hideWorkbench(!launcherOpen && reviewIsPending && (tab === "review" || tab === "changes"));
  };
  const closeTool = (tool: WorkbenchToolTab) => {
    if (tool === "summary") onCloseSubagents();
    closeTab(tool);
    if (
      tool === "files"
      && activeToolId === "files"
      && subagentPanelOpen
      && selectedSubagentId
    ) {
      // 文件深链位于子代理会话之后；关闭文件时回到它的左侧相邻会话。
      onInspectSubagent(selectedSubagentId);
    }
  };
  const openLauncher = () => {
    showLauncher();
    setLauncherIndex(0);
    requestAnimationFrame(() => launcherButtonsRef.current[0]?.focus());
  };
  const dismissLauncher = () => {
    closeLauncher();
    requestAnimationFrame(() => launcherTriggerRef.current?.focus());
  };
  const onLauncherKeyDown = (event: React.KeyboardEvent<HTMLElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      dismissLauncher();
      return;
    }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp" && event.key !== "Home" && event.key !== "End") return;
    event.preventDefault();
    const next = event.key === "Home"
      ? 0
      : event.key === "End"
        ? TABS.length - 1
        : (launcherIndex + (event.key === "ArrowDown" ? 1 : -1) + TABS.length) % TABS.length;
    setLauncherIndex(next);
    launcherButtonsRef.current[next]?.focus();
  };
  const renderToolTab = (toolId: WorkbenchToolTab) => {
    const tool = TABS.find((item) => item.id === toolId);
    if (!tool) return null;
    const selected = !launcherOpen
      && activeToolId === tool.id
      && (!subagentPageOpen || selectedSubagentId == null);
    const selectTool = () => {
      if (tool.id === "summary" && subagentPageOpen) {
        onBackToSubagents();
        return;
      }
      activateTool(tool.openTab);
    };
    return (
      <div
        key={tool.id}
        className={`workbench-tab${selected ? " workbench-active-tab" : ""}`}
      >
        <button
          type="button"
          className="workbench-tab-select"
          role="tab"
          tabIndex={selected ? 0 : -1}
          aria-selected={selected}
          aria-controls="workbench-panel"
          onClick={selectTool}
        >
          <ToolIcon tab={tool.id} width={15} height={15} />
          <strong>{tool.label}</strong>
        </button>
        <button
          type="button"
          className="workbench-tab-close"
          data-testid={selected ? "workbench-close" : undefined}
          onClick={(event) => {
            event.stopPropagation();
            closeTool(tool.id);
          }}
          aria-label={`关闭${tool.label}标签页`}
          title={`关闭${tool.label}`}
        >
          <IconClose width={13} height={13} />
        </button>
      </div>
    );
  };
  // 文件深链属于当前子代理会话的上下文，因此放在所有已打开会话之后。
  // Files 工具仍按唯一 ID 去重；重复点击同一或其他文件只激活现有 Tab。
  const fileTabAfterSubagents = hasOpenSubagentTabs && openTabs.includes("files");
  const toolTabsBeforeSubagents = openTabs
    .filter((toolId) => !fileTabAfterSubagents || toolId !== "files")
    .map(renderToolTab);
  const toolTabsAfterSubagents = fileTabAfterSubagents ? [renderToolTab("files")] : [];

  return (
    <aside
      className={`canvas workbench pane pane-lit${mode === "focus" ? " is-focus" : ""}`}
      data-testid="workbench-panel"
      data-workbench-kind={subagentPageOpen ? (selectedSubagentId ? "subagent-detail" : "subagents") : launcherOpen ? "launcher" : activeToolId}
      data-workbench-section={tab}
      data-workbench-mode={mode}
      aria-label="任务工作台"
    >
      {subagentPageOpen ? (
        <SubagentWorkbench
          taskId={taskId}
          workspacePath={workspacePath}
          subagents={subagents}
          rootRunIds={subagentRootRunIds}
          selectedSubagentId={selectedSubagentId}
          openSubagentIds={openSubagentIds}
          toolTabsBefore={toolTabsBeforeSubagents}
          toolTabsAfter={toolTabsAfterSubagents}
          onSelect={onInspectSubagent}
          onBack={onBackToSubagents}
          onCloseTab={onCloseSubagentTab}
          onOpenLauncher={openLauncher}
          onHide={hideWorkbenchPanel}
          onToggleFocus={toggleFocus}
          focused={mode === "focus"}
          onAbort={onAbortSubagent}
        />
      ) : (
        <>
        <header className={`workbench-head${hasOpenSubagentTabs ? " subagent-tabs-header" : ""}`}>
        <div className="workbench-tabs" role="tablist" aria-label="任务工作台标签" onKeyDown={handleWorkbenchTabListKeyDown}>
          {toolTabsBeforeSubagents}
          <SubagentSessionTabs
            subagents={subagents}
            openSubagentIds={openSubagentIds}
            selectedSubagentId={subagentPageOpen ? selectedSubagentId : null}
            onSelect={onInspectSubagent}
            onCloseTab={onCloseSubagentTab}
          />
          {toolTabsAfterSubagents}
        </div>
        <button ref={launcherTriggerRef} type="button" className="workbench-head-action workbench-add-button" onClick={openLauncher} aria-label="打开任务工具" title="打开任务工具" aria-pressed={launcherOpen}>
          <IconPlus width={16} height={16} />
        </button>
        <span className="workbench-head-spacer" />
        {running && (
          <span className="workbench-live" title={activity.label}>
            <i /> live{workingSubagents > 0 ? ` · ${workingSubagents} 子代理` : ""}
          </span>
        )}
        <button type="button" className="workbench-head-action" onClick={toggleFocus} aria-label={mode === "focus" ? "退出专注模式" : "专注工作台"} aria-pressed={mode === "focus"}>
          {mode === "focus" ? <IconChevronLeft width={15} height={15} /> : <IconMaximize width={15} height={15} />}
        </button>
        <button type="button" className="workbench-head-action" onClick={hideWorkbenchPanel} aria-label="隐藏工作台">
          <IconSidebar width={16} height={16} />
        </button>
        </header>
        <div ref={workbenchBodyRef} className="canvas-body workbench-body" id="workbench-panel" tabIndex={-1}>
        {launcherOpen ? (
          <section className="workbench-launcher" role="dialog" aria-label="工作台工具启动器" onKeyDown={onLauncherKeyDown}>
            <div className="workbench-launcher-intro">
              <span>任务工作台</span>
              <strong>在同一个位置打开任务工具</strong>
              <p>工具按任务保存状态。隐藏工作台不会停止终端或丢失当前上下文。</p>
            </div>
            <ul className="workbench-launcher-list" aria-label="可用工具">
              {TABS.map((tool, index) => (
                <li key={tool.id}>
                  <button
                    ref={(node) => { launcherButtonsRef.current[index] = node; }}
                    type="button"
                    className="workbench-launcher-row"
                    onFocus={() => setLauncherIndex(index)}
                    onClick={() => activateTool(tool.openTab)}
                  >
                    <span className="workbench-launcher-glyph"><ToolIcon tab={tool.id} width={17} height={17} /></span>
                    <span><strong>{tool.label}</strong><small>{tool.description}</small></span>
                    {tool.id === "review" && <em>{displayedReviewChangeCount}</em>}
                    {tool.id === "plan" && planController.view && <em>{planController.view.items.length}</em>}
                    {tool.shortcut && <kbd>{tool.shortcut}</kbd>}
                  </button>
                </li>
              ))}
            </ul>
          </section>
        ) : tab === "summary" ? (
          <SummaryPanel
            detail={detail}
            running={running}
            activity={activity}
            workspacePath={workspacePath}
            workspaceName={workspace?.display_name ?? null}
            workspaceAttached={workspaceAttached}
            workspaceAccessMode={workspace?.access_mode ?? null}
            onShowSubagents={() => {
              if (activity.subagents.length > 0 || detail?.runs.some((run) => run.agent_kind === "subagent")) {
                onBackToSubagents();
              }
            }}
          />
        ) : tab === "files" ? (
          <FilesPanel key={`${taskId}:${workspacePath ?? "none"}:files`} taskId={taskId} workspacePath={workspacePath} workspaceAttached={workspaceAttached} running={running} />
        ) : tab === "terminal" ? (
          <TerminalPanel key={`${taskId}:terminal`} taskId={taskId} workspacePath={workspacePath} workspaceAttached={workspaceAttached} />
        ) : tab === "plan" ? (
          <PlanPanel
            key={`${taskId}:plan`}
            task={task}
            running={running}
            controller={planController}
            subagents={subagents}
            onInspectSubagent={onInspectSubagent}
            onTaskChanged={onTaskChanged}
          />
        ) : (
          <div className="workbench-review-tool">
            <div
              className="workbench-review-switch"
              role="tablist"
              aria-label="审核视图"
              onKeyDown={(event) => {
                if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
                event.preventDefault();
                const next = tab === "changes" ? "review" : "changes";
                setTab(next);
                requestAnimationFrame(() => document.getElementById(`review-view-${next}`)?.focus());
              }}
            >
              <button id="review-view-changes" type="button" role="tab" aria-selected={tab === "changes"} tabIndex={tab === "changes" ? 0 : -1} onClick={() => setTab("changes")}>变更 <span>{displayedReviewChangeCount}</span></button>
              <button id="review-view-review" type="button" role="tab" aria-selected={tab === "review"} tabIndex={tab === "review" ? 0 : -1} onClick={() => setTab("review")}>验证与决策</button>
            </div>
            <div className="workbench-review-panel" role="tabpanel">
              {tab === "changes"
                ? <ChangesPanel key={`${taskId}:changes`} taskId={taskId} running={running} detail={detail} onVisibleCountChange={setReviewChangeCount} />
                : <ReviewPanel key={`${taskId}:review`} taskId={taskId} />}
            </div>
          </div>
        )}
        </div>
        </>
      )}
    </aside>
  );
}

// ---------- Summary ----------

/** 原始审计流展开的条数上限；关键事件与构成统计从同一份 feed/消息流派生。 */
const AUDIT_FEED_LIMIT = 40;
/** 关键事件时间线最多展示的拐点数。 */
const KEY_EVENT_LIMIT = 7;
/** 编队卡片直出的子代理数，超出折叠为「其余 N 个」。 */
const SQUAD_LIMIT = 4;

function SummaryPanel({
  detail,
  running,
  activity,
  workspacePath,
  workspaceName,
  workspaceAttached,
  workspaceAccessMode,
  onShowSubagents,
}: {
  detail: TaskDetail | undefined;
  running: boolean;
  activity: ActivityTraceState;
  workspacePath: string | null;
  workspaceName: string | null;
  workspaceAttached: boolean;
  workspaceAccessMode: ProjectAccessMode | null;
  onShowSubagents: () => void;
}) {
  const setTab = useAppStore((s) => s.setCanvasTab);
  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const [messages, setMessages] = useState<SessionMessage[]>([]);
  const [auditOpen, setAuditOpen] = useState(false);
  const [permBusyId, setPermBusyId] = useState<string | null>(null);
  const [permError, setPermError] = useState<string | null>(null);
  const taskId = detail?.task.id ?? null;
  // 运行中 1s 一跳刷新 LIVE 耗时；空闲时 30s 足够维持相对时间（「3 分钟前」）不失真。
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), running ? 1_000 : 30_000);
    return () => window.clearInterval(timer);
  }, [running]);
  // 审计流的工具明细来自会话 JSONL（tool_call/tool_result 才有工具名、输入、输出），
  // task_events 只提供时间锚点。RoomScene 已经 2s 刷新 detail，事件/变更/验证数量一变
  // 就说明有新动作落盘，据此重取即可，不再叠加一层定时轮询。
  const auditStamp = detail
    ? `${detail.events.length}:${detail.changes.length}:${detail.verifications.length}`
    : "";

  useEffect(() => {
    setMessages([]);
  }, [taskId]);

  useEffect(() => {
    if (!taskId) return;
    let dead = false;
    sessionMessages(taskId)
      .then((list) => {
        if (!dead) setMessages(list);
      })
      .catch(() => {
        /* 审计流是只读视图，取不到就保留上一批，等下一次事件变化重试 */
      });
    return () => {
      dead = true;
    };
  }, [taskId, auditStamp]);

  const feed = useMemo(
    () =>
      detail
        ? buildAuditFeed(
            {
              messages,
              events: detail.events,
              changes: detail.changes,
              verifications: detail.verifications,
              permissions: detail.permissions,
              runs: detail.runs,
            },
            AUDIT_FEED_LIMIT
          )
        : [],
    [detail, messages]
  );
  const composition = useMemo(() => summarizeToolComposition(messages), [messages]);
  const keyEvents = useMemo(() => buildKeyEvents(feed, KEY_EVENT_LIMIT), [feed]);
  const routineCount = useMemo(() => feed.filter(isRoutineAuditRow).length, [feed]);
  const spark = useMemo(() => activityBuckets(detail?.events ?? []), [detail]);
  const failedOps = useMemo(() => feed.filter((row) => row.state === "fail").length, [feed]);

  if (!detail) return <div className="empty">加载中…</div>;
  const { task, runs, changes, permissions, verifications, queued_messages: queuedMessages } = detail;
  const pendingList = permissions.filter((p) => p.decision === "pending");
  const pending = pendingList.length;
  const passed = verifications.filter((v) => v.status === "passed").length;
  const verifying = verifications.some((v) => v.status === "running");
  const queued = queuedMessages.filter((message) => message.state === "queued" || message.state === "dispatching").length;
  const activeMainRun = runs.find((run) => run.agent_kind === "main" && run.ended_at == null);
  const subagentRuns = runs.filter((run) => run.agent_kind === "subagent");
  const squad = buildSquad(subagentRuns, activity, running, now);
  const activeSquad = squad.filter((item) => item.live).length;
  const inconclusiveSquad = squad.filter((item) => !item.live && item.ring === "warn").length;
  const title = task.title.trim() || task.goal.trim() || "未命名会话";
  const hasDistinctGoal = Boolean(task.goal_active && task.goal.trim() && task.goal.trim() !== title);
  const workspaceLabel = workspaceName ?? (workspacePath ? "已附加文件夹" : "用户路径");
  const modelLabel = activeMainRun?.model || task.provider_name || "默认模型服务";

  // 产品只展示 Agent / Plan 两种交互模式。持久层中的 Ask/Edit/Auto 是兼容策略，
  // 项目权限仍独立决定 Agent 在工作区中可以使用的能力。
  const accessLabel =
    workspaceAttached && workspaceAccessMode ? projectAccessModeLabel(workspaceAccessMode) : null;
  const policyLabel = `${modeShortLabel(task.mode)} · ${accessLabel ?? "默认工作区"}`;
  const policyTitle = `${modeLabel(task.mode)}\n项目权限：${accessLabel ?? "用户路径（默认工作区）· 写操作需批准"}`;

  const decidePermission = async (id: string, decision: Exclude<PermissionDecision, "pending">) => {
    setPermBusyId(id);
    setPermError(null);
    try {
      await permissionApprove(id, decision);
      await refreshDetail(task.id);
    } catch (error) {
      setPermError(String(error));
    } finally {
      setPermBusyId(null);
    }
  };

  return (
    <div className="sum-wrap">
      <div className="sum-head">
        <span className={"st-chip " + statusChipClass(detail)}>
          {taskStateLabel(task.state, detail)}
        </span>
        <span className="sum-title" title={title}>{title}</span>
        <span className="sum-age" title={`会话开始于 ${clockSeconds(task.created_at)}`}>
          {elapsedMinutes(task.created_at)}
        </span>
      </div>
      {hasDistinctGoal && <div className="sum-goal" title={task.goal}>{task.goal}</div>}
      <div className="sum-scope">
        <span title={workspacePath ? displayPath(workspacePath) : "用户路径"}>
          {workspaceLabel}
        </span>
        <span title={modelLabel}>模型 · {modelLabel}</span>
        <span className={workspaceAttached ? "scoped" : ""} title={policyTitle}>
          {policyLabel}
        </span>
      </div>

      {running && activeMainRun && (
        <div className="sum-live-card">
          <div className="sum-live-top">
            <span className="sum-live-dot" />
            <span className="sum-live-tag">LIVE · 当前运行</span>
            <span className="sum-live-elapsed">{elapsedSince(activeMainRun.started_at, now)}</span>
          </div>
          <div className="sum-live-action">{activity.label}</div>
          <div className="sum-live-bar" aria-hidden="true"><i /></div>
          <div className="sum-live-sub">
            已读取 {composition.read} 个文件 · {composition.command} 条命令 · {composition.write + composition.edit} 次写入
            {queued > 0 ? ` · 队列 ${queued}` : ""}
          </div>
        </div>
      )}

      {pending > 0 && (
        <div className="sum-perm">
          <div className="sum-perm-top">
            <b>待批权限 · {pending}</b>
            <span className="sum-perm-risk">{permissionRiskLabel(pendingList[0].risk_level)}</span>
          </div>
          <div className="sum-perm-cmd" title={pendingList[0].input_summary}>
            {pendingList[0].input_summary.trim() || pendingList[0].tool_name}
          </div>
          <div className="sum-perm-why">
            {permissionAttribution(pendingList[0], runs).label} · {pendingList[0].tool_name}
            {pending > 1 ? ` · 其余 ${pending - 1} 项在「验证与决策」` : ""}
          </div>
          <div className="sum-perm-actions">
            <button type="button" className="deny" disabled={permBusyId === pendingList[0].id} onClick={() => void decidePermission(pendingList[0].id, "deny")}>
              拒绝
            </button>
            <button type="button" className="ghost" disabled={permBusyId === pendingList[0].id} onClick={() => void decidePermission(pendingList[0].id, "allow_always")}>
              总是允许
            </button>
            <button type="button" className="approve" disabled={permBusyId === pendingList[0].id} onClick={() => void decidePermission(pendingList[0].id, "allow")}>
              允许一次
            </button>
          </div>
          {permError && <div className="sum-perm-error">批复失败：{permError}</div>}
        </div>
      )}

      <div className="zone-head">运行简报</div>
      <div className="sum-brief">
        <div className="sum-brief-outcomes">
          <button type="button" className="sum-oc" onClick={() => setTab("changes")} title="查看变更文件">
            <div className="sum-oc-v">{changes.length}</div>
            <div className="sum-oc-k">变更文件</div>
          </button>
          <button type="button" className="sum-oc" onClick={() => setTab("review")} title="查看验证记录">
            <div className={"sum-oc-v" + (verifications.length === 0 ? " dim" : passed === verifications.length ? " ok" : " bad")}>
              {verifications.length === 0 ? "未运行" : `${passed}/${verifications.length}`}
            </div>
            <div className="sum-oc-k">验证{verifying ? " · 进行中" : ""}</div>
          </button>
          <button type="button" className="sum-oc" onClick={() => setTab("review")} title="查看待批权限">
            <div className={"sum-oc-v" + (pending > 0 ? " warn" : "")}>{pending}</div>
            <div className="sum-oc-k">待批权限</div>
          </button>
          <button type="button" className="sum-oc" onClick={() => setAuditOpen(true)} title="展开原始审计流查看失败详情">
            <div className={"sum-oc-v" + (failedOps > 0 ? " bad" : " ok")}>{failedOps}</div>
            <div className="sum-oc-k">失败操作</div>
          </button>
          {queued > 0 && !running && (
            <div className="sum-oc static">
              <div className="sum-oc-v warn">{queued}</div>
              <div className="sum-oc-k">队列</div>
            </div>
          )}
        </div>
        <div className="sum-brief-activity">
          {composition.total > 0 && (
            <div
              className="sum-bar"
              role="img"
              aria-label={`操作构成：读取 ${composition.read}，命令 ${composition.command}，检索 ${composition.search}，写入 ${composition.write + composition.edit}${composition.other > 0 ? `，其他 ${composition.other}` : ""}`}
            >
              {composition.read > 0 && <span className="b-read" style={{ width: `${(composition.read / composition.total) * 100}%` }} />}
              {composition.command > 0 && <span className="b-cmd" style={{ width: `${(composition.command / composition.total) * 100}%` }} />}
              {composition.search > 0 && <span className="b-search" style={{ width: `${(composition.search / composition.total) * 100}%` }} />}
              {composition.write + composition.edit > 0 && <span className="b-write" style={{ width: `${((composition.write + composition.edit) / composition.total) * 100}%` }} />}
              {composition.other > 0 && <span className="b-other" style={{ width: `${(composition.other / composition.total) * 100}%` }} />}
            </div>
          )}
          <div className="sum-legend">
            <span><i className="d-read" />读取 {composition.read}</span>
            <span><i className="d-cmd" />命令 {composition.command}</span>
            <span><i className="d-search" />检索 {composition.search}</span>
            <span><i className="d-write" />写入 {composition.write + composition.edit}</span>
            <span className="sum-legend-total">
              {spark.length > 0 && (
                <span className="sum-spark" aria-hidden="true">
                  {spark.map((value, index) => (
                    <s key={index} className={value >= 0.85 ? "hot" : ""} style={{ height: `${Math.max(18, Math.round(value * 100))}%` }} />
                  ))}
                </span>
              )}
              {composition.total} 次操作{running ? " · 进行中" : ""}
            </span>
          </div>
        </div>
      </div>

      {squad.length > 0 && (
        <>
          <div className="zone-head">
            子代理编队
            <span className={"zone-hint" + (inconclusiveSquad > 0 ? " warn" : "")}>
              {squad.length} 个
              {activeSquad > 0 ? ` · ${activeSquad} 运行中` : ""}
              {inconclusiveSquad > 0 ? ` · ${inconclusiveSquad} 个无结果摘要` : ""}
            </span>
          </div>
          <div className="sum-squad">
            {squad.slice(0, SQUAD_LIMIT).map((item, index) => (
              <button type="button" key={item.id} className="sum-sq" onClick={onShowSubagents} title="打开子智能体列表查看运行过程">
                <SubagentAvatar index={index} identity={item.id} runtimeKind={item.runtimeKind} size="sm" status={item.ring} />
                <span className="sum-sq-main">
                  <span className="sum-sq-top"><b>{item.label}</b><em>{item.meta}</em></span>
                  <span className={`sum-sq-sub ${item.tone}`}>{item.outcome}</span>
                </span>
                <IconChevronRight width={13} height={13} />
              </button>
            ))}
            {squad.length > SQUAD_LIMIT && (
              <button type="button" className="sum-sq-more" onClick={onShowSubagents}>
                其余 {squad.length - SQUAD_LIMIT} 个子代理 →
              </button>
            )}
          </div>
        </>
      )}

      <div className="zone-head">
        关键事件
        <span className="zone-hint">
          {routineCount > 0 ? `${routineCount} 次常规读取/检索已折叠` : "命令 · 验证 · 权限 · 子代理"}
        </span>
      </div>
      {keyEvents.length === 0 ? (
        <div className="sum-empty">
          {feed.length === 0 ? "会话尚未产生可审计的动作。" : "暂无关键事件，只有常规读取/检索。"}
        </div>
      ) : (
        <div className="sum-tl">
          {keyEvents.map((row) => (
            <div className={`sum-tl-row is-${row.state}`} key={row.id} title={row.title}>
              <span className="sum-tl-dot" />
              <div className="sum-tl-main">
                <div className="sum-tl-top">
                  <span className="sum-tl-kind">{row.tag}</span>
                  <b className={row.kind === "tool" || row.kind === "verify" ? "" : "plain"}>{row.text}</b>
                  <em>{row.atIso ? relativeAgo(row.atIso, now) : ""}</em>
                </div>
                {row.result && <div className="sum-tl-sub">{row.result}</div>}
              </div>
            </div>
          ))}
        </div>
      )}

      {feed.length > 0 && (
        <button
          type="button"
          className="sum-audit-toggle"
          aria-expanded={auditOpen}
          onClick={() => setAuditOpen((open) => !open)}
        >
          <span className="tri">{auditOpen ? "▾" : "▸"}</span> 原始审计流
          <em>读取 {composition.read} · 检索 {composition.search} · 共 {feed.length} 条</em>
        </button>
      )}
      {auditOpen && (
        <div className="audit-list">
          {feed.map((row) => (
            <div
              className={`audit-row kind-${row.kind} state-${row.state}`}
              key={row.id}
              title={row.title}
            >
              <span className="audit-tag">{row.tag}</span>
              <span className="audit-text">{row.text}</span>
              {row.result && <span className="audit-result">{row.result}</span>}
              <span className="audit-at">{clockSeconds(row.atIso)}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

interface SquadItem {
  id: string;
  label: string;
  runtimeKind: AgentRun["runtime_kind"];
  ring: SubagentAvatarStatus;
  /** 行右元信息：已完成为耗时，运行中为已进行时长。 */
  meta: string;
  outcome: string;
  tone: "ok" | "warn" | "live";
  live: boolean;
  startedAtMs: number;
}

/**
 * 子代理编队 —— 以持久化的 AgentRun 为准（有 summary/review_state/耗时），
 * 用 activity 快照补充运行中的实时状态与当前动作。运行中置顶，其余按开始时间倒序。
 */
function buildSquad(
  runs: readonly AgentRun[],
  activity: ActivityTraceState,
  mainRunning: boolean,
  now: number,
): SquadItem[] {
  const liveById = new Map(activity.subagents.map((child) => [child.id, child]));
  const items: SquadItem[] = runs.map((run) => {
    const label = run.agent_label?.trim() || "只读调查";
    const live = liveById.get(run.id);
    const startedAtMs = Date.parse(run.started_at);
    const liveStatus = live && live.status !== "completed" && live.status !== "failed" && live.status !== "cancelled"
      ? live.status
      : null;
    if (run.ended_at == null && (mainRunning || liveStatus != null)) {
      return {
        id: run.id,
        label,
        runtimeKind: run.runtime_kind,
        ring: "run",
        meta: Number.isNaN(startedAtMs) ? "" : elapsedSince(run.started_at, now),
        outcome: liveStatus === "queued"
          ? "排队中"
          : liveStatus === "waiting_permission"
            ? "等待权限批准"
            : live?.detail ?? "运行中",
        tone: "live",
        live: true,
        startedAtMs,
      };
    }
    const endedMs = run.ended_at != null ? Date.parse(run.ended_at) : Number.NaN;
    const duration = !Number.isNaN(startedAtMs) && !Number.isNaN(endedMs)
      ? elapsedSince(run.started_at, endedMs)
      : "";
    if (run.review_state === "failed" || run.review_state === "aborted" || run.review_state === "rolled_back") {
      return {
        id: run.id,
        label,
        runtimeKind: run.runtime_kind,
        ring: "warn",
        meta: duration,
        outcome: run.review_state === "failed" ? "✗ 执行失败" : "✗ 已中止",
        tone: "warn",
        live: false,
        startedAtMs,
      };
    }
    if (run.ended_at == null) {
      return {
        id: run.id,
        label,
        runtimeKind: run.runtime_kind,
        ring: "warn",
        meta: duration,
        outcome: "已中断 · 未收到完成回传",
        tone: "warn",
        live: false,
        startedAtMs,
      };
    }
    const summary = run.summary?.trim() ?? "";
    return {
      id: run.id,
      label,
      runtimeKind: run.runtime_kind,
      ring: summary ? "ok" : "warn",
      meta: duration,
      outcome: summary ? `✓ ${cutSquadSummary(summary)}` : "已完成 · 无结果摘要",
      tone: summary ? "ok" : "warn",
      live: false,
      startedAtMs,
    };
  });
  items.sort((a, b) => Number(b.live) - Number(a.live) || b.startedAtMs - a.startedAtMs);
  return items;
}

function cutSquadSummary(value: string): string {
  const text = value.replace(/\s+/g, " ");
  return text.length > 40 ? `${text.slice(0, 39)}…` : text;
}

function statusChipClass(detail: TaskDetail): string {
  switch (taskDisplayState(detail.task, detail)) {
    case "waiting_for_approval":
    case "waiting_for_question":
    case "review_ready":
    case "verification_required":
      return "warn";
    case "failed":
    case "interrupted":
    case "workspace_binding_invalid":
      return "bad";
    case "verifying":
    case "running":
    case "queued":
      return "run";
    case "archived":
    case "idle":
      return "";
  }
}

// ---------- Changes ----------

function ChangesPanel(props: {
  taskId: string;
  running: boolean;
  detail: TaskDetail | undefined;
  onVisibleCountChange: (count: number) => void;
}) {
  const sessionKey = `${props.taskId}:review-mode`;
  const session = readPanelSession<{ mode: "normal" | "enhanced" }>(sessionKey);
  const [mode, setMode] = useState<"normal" | "enhanced">(session?.mode ?? "normal");
  useRememberPanelSession(sessionKey, { mode });
  return (
    <div className="review-mode-shell">
      <div
        className="review-mode-toolbar"
        role="tablist"
        aria-label="变更审核模式"
        onKeyDown={(event) => {
          if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
          event.preventDefault();
          const next = mode === "normal" ? "enhanced" : "normal";
          setMode(next);
          requestAnimationFrame(() => document.getElementById(`review-mode-${next}`)?.focus());
        }}
      >
        <button id="review-mode-normal" type="button" role="tab" aria-selected={mode === "normal"} aria-controls="review-mode-panel" tabIndex={mode === "normal" ? 0 : -1} onClick={() => setMode("normal")}>普通</button>
        <button id="review-mode-enhanced" type="button" role="tab" aria-selected={mode === "enhanced"} aria-controls="review-mode-panel" tabIndex={mode === "enhanced" ? 0 : -1} onClick={() => setMode("enhanced")}>增强</button>
        <span>{mode === "normal" ? "Git 工作区" : "当前 Plan 功能点"}</span>
      </div>
      <div id="review-mode-panel" className="review-mode-content" role="tabpanel" aria-labelledby={`review-mode-${mode}`}>
        {mode === "normal"
          ? <NormalChangesPanel {...props} />
          : <EnhancedReviewPanel taskId={props.taskId} running={props.running} onVisibleCountChange={props.onVisibleCountChange} />}
      </div>
    </div>
  );
}

function NormalChangesPanel({
  taskId,
  running,
  detail,
  onVisibleCountChange,
}: {
  taskId: string;
  running: boolean;
  detail: TaskDetail | undefined;
  onVisibleCountChange: (count: number) => void;
}) {
  const changes = useMemo(() => detail?.changes ?? [], [detail]);
  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const sessionKey = `${taskId}:changes`;
  const session = readPanelSession<{ selectedPath: string | null; f7Index: number }>(sessionKey);
  const [sel, setSel] = useState<string | null>(session?.selectedPath ?? null);
  const [diff, setDiff] = useState<ChangeDiff | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reviewStatusError, setReviewStatusError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [confirmPath, setConfirmPath] = useState<string | null>(null);
  const [rowFocus, setRowFocus] = useState(-1);
  const [gitStatus, setGitStatus] = useState<ReviewGitStatus | null>(null);
  const [pendingAccepts, setPendingAccepts] = useState<Set<string>>(new Set());
  const [pendingRejects, setPendingRejects] = useState<Set<string>>(new Set());
  const [exitingPaths, setExitingPaths] = useState<Set<string>>(new Set());
  const [confirmRejectAll, setConfirmRejectAll] = useState(false);
  const [acceptedLineKeys, setAcceptedLineKeys] = useState<Set<string>>(new Set());
  const pendingAcceptKeysRef = useRef<Set<string>>(new Set());
  const pendingRejectKeysRef = useRef<Set<string>>(new Set());
  const statusRefreshSequenceRef = useRef(0);
  const confirmTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const confirmAllTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const exitTimersRef = useRef<Set<ReturnType<typeof setTimeout>>>(new Set());

  const refreshGitStatus = useCallback(async () => {
    const sequence = ++statusRefreshSequenceRef.current;
    try {
      const next = await reviewGitStatus(taskId);
      if (sequence === statusRefreshSequenceRef.current) {
        setGitStatus(next);
        setReviewStatusError(null);
      }
      return next;
    } catch (cause) {
      if (sequence === statusRefreshSequenceRef.current) {
        setGitStatus(null);
        setReviewStatusError(`无法刷新审核范围：${String(cause)}`);
      }
      return null;
    }
  }, [taskId]);

  // FX-15：git status 是真实子进程；运行中 2s 跟手，空闲降到 8s。
  usePoll(async () => {
    await refreshGitStatus();
  }, running ? 2000 : 8000, true, "审核变更");

  useEffect(() => () => {
    if (confirmTimer.current) clearTimeout(confirmTimer.current);
    if (confirmAllTimer.current) clearTimeout(confirmAllTimer.current);
    for (const timer of exitTimersRef.current) clearTimeout(timer);
    exitTimersRef.current.clear();
  }, []);

  const visibleChanges = useMemo(() => {
    // Fail closed until the authoritative Git-filtered scope is available. Raw task events may
    // contain ignored logs/temp files and must never become actionable review rows.
    if (!gitStatus) return [];
    const recordedByPath = new Map(changes.map((change) => [change.path, change]));
    const visible = gitStatus.paths
      .filter((status) => status.scope === "workspace" || !status.rejected || exitingPaths.has(status.path))
      .map((status): FileChange => recordedByPath.get(status.path) ?? {
        id: `workspace-git:${status.path}`,
        task_id: taskId,
        tool_call_id: null,
        path: status.path,
        change_type: status.change_type ?? "modify",
        before_hash: null,
        after_hash: null,
        old_path: null,
        created_at: "",
      });
    // A fast status refresh may already omit a successfully rejected path. Keep its old row for
    // the short exit animation, then let it leave the active list.
    for (const exitingPath of exitingPaths) {
      if (visible.some((change) => change.path === exitingPath)) continue;
      const recorded = recordedByPath.get(exitingPath);
      if (recorded) visible.push(recorded);
    }
    return visible;
  }, [changes, exitingPaths, gitStatus, taskId]);
  useEffect(() => onVisibleCountChange(visibleChanges.length), [onVisibleCountChange, visibleChanges.length]);
  const path = sel && visibleChanges.some((change) => change.path === sel)
    ? sel
    : visibleChanges[0]?.path ?? null;
  const selIndex = visibleChanges.findIndex((item) => item.path === path);
  const rovingIndex = rovingIndexOf(rowFocus, selIndex, visibleChanges.length);

  const pathStatus = useMemo(
    () => new Map(gitStatus?.paths.map((item) => [item.path, item]) ?? []),
    [gitStatus]
  );

  const fileAcceptKey = (targetPath: string) => `file:${targetPath}`;
  const lineAcceptKey = (targetPath: string, lineId: string) => `line:${targetPath}:${lineId}`;
  const isPathFullyAccepted = (targetPath: string) => {
    const status = pathStatus.get(targetPath);
    return status?.accepted === true && status.remaining === false;
  };
  const isPathRejected = (targetPath: string) => pathStatus.get(targetPath)?.rejected === true;
  const beginAccept = (key: string) => {
    if (pendingAcceptKeysRef.current.has(key)) return false;
    pendingAcceptKeysRef.current.add(key);
    setPendingAccepts(new Set(pendingAcceptKeysRef.current));
    return true;
  };
  const finishAccept = (key: string) => {
    pendingAcceptKeysRef.current.delete(key);
    setPendingAccepts(new Set(pendingAcceptKeysRef.current));
  };
  const beginReject = (key: string) => {
    if (pendingRejectKeysRef.current.has(key)) return false;
    pendingRejectKeysRef.current.add(key);
    setPendingRejects(new Set(pendingRejectKeysRef.current));
    return true;
  };
  const finishReject = (key: string) => {
    pendingRejectKeysRef.current.delete(key);
    setPendingRejects(new Set(pendingRejectKeysRef.current));
  };
  const animateRejectedPaths = useCallback((paths: string[]) => new Promise<void>((resolve) => {
    if (paths.length === 0) {
      resolve();
      return;
    }
    setExitingPaths((current) => new Set([...current, ...paths]));
    const reducedMotion = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches === true;
    const timer = setTimeout(() => {
      exitTimersRef.current.delete(timer);
      setExitingPaths((current) => {
        const next = new Set(current);
        for (const path of paths) next.delete(path);
        return next;
      });
      resolve();
    }, reducedMotion ? 0 : 180);
    exitTimersRef.current.add(timer);
  }), []);
  const runAccept = (
    key: string,
    operation: () => Promise<void>,
    onFailure?: () => void,
  ) => {
    void (async () => {
      try {
        await operation();
        // SQLite decisions are independent, so unrelated accepts never wait behind this one.
        // Reconciliation is authoritative but stays off the interaction critical path.
        void refreshGitStatus();
      } catch (cause) {
        onFailure?.();
        setError(String(cause));
      } finally {
        finishAccept(key);
      }
    })();
  };

  const acceptFileChange = async (targetPath: string) => {
    const key = fileAcceptKey(targetPath);
    if (!beginAccept(key)) return;
    setError(null);
    setNotice(null);
    setGitStatus((current) => current ? optimisticallyResolvePath(current, targetPath, "accepted") : current);
    runAccept(key, async () => {
      const result = await reviewAcceptFile(taskId, targetPath);
      setNotice(`已接受 ${targetPath}；还有 ${result.remaining_count} 个文件未完全接受。`);
    }, () => void refreshGitStatus());
  };

  const acceptAllChanges = async () => {
    const key = "all";
    if (!beginAccept(key)) return;
    setError(null);
    setNotice(null);
    setGitStatus((current) => current ? optimisticallyAcceptAll(current) : current);
    runAccept(key, async () => {
      const result = await reviewAcceptAll(taskId);
      setNotice(result.fully_accepted ? "已接受本任务的全部文件。" : `仍有 ${result.remaining_count} 个文件待处理。`);
    }, () => void refreshGitStatus());
  };

  const acceptDiffLine = async (targetPath: string, lineId: string) => {
    const key = lineAcceptKey(targetPath, lineId);
    if (!beginAccept(key)) return;
    setAcceptedLineKeys((current) => new Set(current).add(key));
    setError(null);
    setNotice(null);
    setDiff((current) => current ? {
      ...current,
      lines: current.lines?.map((line) => line.line_id === lineId ? { ...line, review_state: "accepted" } : line),
    } : current);
    runAccept(key, async () => {
      const result = await reviewAcceptLine(taskId, targetPath, lineId);
      setNotice(result.fully_accepted ? `已接受 ${targetPath} 的全部变更。` : "已接受这一行。" );
    }, () => {
      setAcceptedLineKeys((current) => {
        const next = new Set(current);
        next.delete(key);
        return next;
      });
    });
  };

  // detail 每 2s 刷新 → diff 跟随(运行中即 live following)
  useEffect(() => {
    if (!path) {
      setDiff(null);
      return;
    }
    let dead = false;
    changeDiff(taskId, path)
      .then((d) => {
        if (!dead) {
          setDiff(d);
          setError(null);
        }
      })
      .catch((e) => {
        if (!dead) setError(String(e));
      });
    return () => {
      dead = true;
    };
  }, [taskId, path, changes, gitStatus]);

  const doRollback = async (p: string) => {
    if (confirmPath !== p) {
      setConfirmPath(p);
      if (confirmTimer.current) clearTimeout(confirmTimer.current);
      confirmTimer.current = setTimeout(() => setConfirmPath(null), 3000);
      return;
    }
    const key = `file:${p}`;
    if (!beginReject(key)) return;
    setConfirmPath(null);
    setError(null);
    setNotice(null);
    try {
      const result = await reviewRejectFile(taskId, p);
      setGitStatus((current) => current ? optimisticallyResolvePath(current, p, "rejected") : current);
      setNotice(`已拒绝 ${p}；文件已安全恢复。`);
      await animateRejectedPaths([p]);
      await Promise.all([refreshDetail(taskId), refreshGitStatus()]);
    } catch (e) {
      setError(String(e));
    } finally {
      finishReject(key);
    }
  };

  const rejectAllChanges = async () => {
    if (!confirmRejectAll) {
      setConfirmRejectAll(true);
      if (confirmAllTimer.current) clearTimeout(confirmAllTimer.current);
      confirmAllTimer.current = setTimeout(() => setConfirmRejectAll(false), 3500);
      return;
    }
    if (!beginReject("all")) return;
    setConfirmRejectAll(false);
    setError(null);
    setNotice(null);
    const targets = (gitStatus?.paths ?? [])
      .filter((status) => status.scope !== "workspace" && !status.rejected && status.remaining)
      .map((status) => status.path);
    try {
      await rollbackTask(taskId);
      setGitStatus((current) => {
        if (!current) return current;
        return targets.reduce(
          (next, targetPath) => optimisticallyResolvePath(next, targetPath, "rejected"),
          current,
        );
      });
      setNotice(`已拒绝本轮 ${targets.length} 个文件；工作区原有变更未受影响。`);
      await animateRejectedPaths(targets);
      await Promise.all([refreshDetail(taskId), refreshGitStatus()]);
    } catch (cause) {
      setError(String(cause));
    } finally {
      finishReject("all");
    }
  };

  const taskPathStatuses = gitStatus?.paths.filter((status) => status.scope !== "workspace") ?? [];
  const selectedPathStatus = path ? pathStatus.get(path) : undefined;
  const selectedWorkspaceOnly = selectedPathStatus?.scope === "workspace";
  const selectedPathExiting = path ? exitingPaths.has(path) : false;
  // 已接受的文件是终态决定，不能再被「拒绝本轮全部」恢复；只有仍未处理（含冲突）的路径可拒绝。
  const rejectableTaskCount = taskPathStatuses.filter((status) => !status.rejected && status.remaining).length;
  const pendingTaskCount = taskPathStatuses.filter((status) => status.remaining).length;
  const globalDecisionPending = pendingAccepts.has("all") || pendingRejects.has("all");

  const lines = diff?.supported ? (diff.lines ?? []) : [];
  const adds = lines.filter((l) => l.kind === "add").length;
  const dels = lines.filter((l) => l.kind === "del").length;

  // F7/⇧F7 在 add/del 行间循环跳转。
  const changePoints = useMemo(
    () =>
      lines
        .map((l, i) => (l.kind === "add" || l.kind === "del" ? i : -1))
        .filter((i) => i >= 0),
    [lines]
  );
  const [f7Idx, setF7Idx] = useState(session?.f7Index ?? -1);
  const diffBodyRef = useRef<HTMLDivElement>(null);
  useRememberPanelSession(sessionKey, { selectedPath: sel, f7Index: f7Idx });
  // 已经跳过一次的 f7Idx。detail 每 2s 轮询 → changes/lines/changePoints 全是新引用，
  // 下面那个 effect 会跟着重跑；只认引用的话 scrollIntoView 会每 2 秒把视图
  // 拉回 diff 行。用它把「effect 重跑」和「用户真的按了 F7」区分开。
  const f7DoneRef = useRef(-1);

  useEffect(() => {
    if (changePoints.length === 0) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "F7" || isTypingTarget(e.target)) return;
      e.preventDefault();
      setF7Idx((cur) =>
        e.shiftKey
          ? cur <= 0
            ? changePoints.length - 1
            : cur - 1
          : cur >= changePoints.length - 1
            ? 0
            : cur + 1
      );
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [changePoints.length]);

  useEffect(() => {
    // 只有 f7Idx 真的变了才跳。挂载时 f7Idx 与 f7DoneRef 同为 -1，天然不触发；
    // 轮询刷新只换引用不换 f7Idx，走到这里就直接返回，焦点留在用户自己那儿。
    if (f7Idx === f7DoneRef.current) return;
    if (f7Idx < 0 || f7Idx >= changePoints.length) return;
    // 按 lines 下标定位，避免原先按
    // querySelectorAll(".dl") 序号定位在 truncated 时整体偏一行的问题。
    const target = diffBodyRef.current?.querySelector<HTMLElement>(
      `[data-dline="${changePoints[f7Idx]}"]`
    );
    // 目标行还没渲染出来（切文件那一帧）：不记账，等下一次 render 再跳。
    if (!target) return;
    f7DoneRef.current = f7Idx;
    target.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [f7Idx, changePoints]);

  // 最新一段连续 add 行 → .fresh shimmer(仅运行中)
  const freshFrom = useMemo(() => {
    if (!running || lines.length === 0) return -1;
    let end = -1;
    for (let i = lines.length - 1; i >= 0; i--) {
      if (lines[i].kind === "add") {
        end = i;
        break;
      }
    }
    if (end < 0) return -1;
    let start = end;
    while (start - 1 >= 0 && lines[start - 1].kind === "add") start--;
    return start;
  }, [lines, running]);
  const freshTo = useMemo(() => {
    if (freshFrom < 0) return -1;
    let end = freshFrom;
    while (end + 1 < lines.length && lines[end + 1].kind === "add") end++;
    return end;
  }, [freshFrom, lines]);

  const reviewActions = path ? (
    <span className="review-accept-actions">
      {selectedWorkspaceOnly ? (
        <span className="chg-workspace-note" title={selectedPathStatus?.blocker ?? undefined}>
          工作区未提交 · 仅查看
        </span>
      ) : (
        <button
          className="btn sm"
          disabled={
            globalDecisionPending
            || pendingAccepts.has(fileAcceptKey(path))
            || pendingRejects.has(`file:${path}`)
            || selectedPathStatus?.safe_to_accept === false
            || isPathFullyAccepted(path)
            || isPathRejected(path)
            || selectedPathExiting
          }
          onClick={() => void acceptFileChange(path)}
        >
          {selectedPathExiting || isPathRejected(path)
            ? "已拒绝文件"
            : isPathFullyAccepted(path)
              ? "已接受文件"
              : pendingAccepts.has(fileAcceptKey(path))
                ? "接受中…"
                : "接受文件"}
        </button>
      )}
      {pendingTaskCount > 0 && (
        <>
          <button
            className="btn accent sm"
            disabled={globalDecisionPending || pendingRejects.size > 0 || gitStatus?.can_accept_all === false}
            onClick={() => void acceptAllChanges()}
          >
            {pendingAccepts.has("all") ? "接受中…" : "接受本轮全部"}
          </button>
          <button
            className={"btn sm review-reject-all" + (confirmRejectAll ? " confirm" : "")}
            disabled={globalDecisionPending || pendingAccepts.size > 0 || rejectableTaskCount === 0}
            aria-label={confirmRejectAll ? "再次确认，拒绝本轮全部文件" : "拒绝并恢复本轮全部文件"}
            onClick={() => void rejectAllChanges()}
          >
            {pendingRejects.has("all") ? "恢复中…" : confirmRejectAll ? "确认拒绝本轮?" : "拒绝本轮全部"}
          </button>
        </>
      )}
    </span>
  ) : null;

  return (
    <div className="changes-wrap">
      <div className="changes-list">
        {visibleChanges.length === 0 && (
          <div className="empty">
            {!gitStatus
              ? reviewStatusError
                ? "审核范围暂不可用，请稍后重试。"
                : "正在确认可审核范围…"
              : gitStatus.accepted_count + gitStatus.rejected_count > 0
              ? "本轮变更已处理完，工作区没有其他未提交变更。"
              : "工作区没有未提交变更。"}
          </div>
        )}
        {visibleChanges.length > 0 && (
          <div className="chg-options" role="grid" aria-label="变更文件">
            {visibleChanges.map((c, index) => {
              const status = pathStatus.get(c.path);
              const workspaceOnly = status?.scope === "workspace";
              const exiting = exitingPaths.has(c.path);
              return (
                <div
                  key={c.id}
                  id={chgRowId(index)}
                  role="row"
                  aria-selected={path === c.path}
                  aria-busy={exiting || pendingRejects.has(`file:${c.path}`)}
                  tabIndex={index === rovingIndex ? 0 : -1}
                  className={
                    "chg-row ring-inset"
                    + (path === c.path ? " sel" : "")
                    + (workspaceOnly ? " workspace-only" : "")
                    + (exiting ? " is-exiting" : "")
                  }
                  onFocus={() => setRowFocus(index)}
                  onClick={() => setSel(c.path)}
                  onKeyDown={(event) => {
                    if (moveRowFocus(event, index, visibleChanges.length, chgRowId)) return;
                    if (!isRowActivate(event)) return;
                    event.preventDefault();
                    setSel(c.path);
                  }}
                >
                  <span className="rcell rcell-main" role="gridcell">
                    {/* new/mod/del/ren 三字母缩写只有视觉意义，读屏走后面的完整中文 */}
                    <span className={"chg-type t-" + c.change_type} aria-hidden="true">
                      {typeLabel(c)}
                    </span>
                    <span className="sr-only">{typeFullLabel(c)}</span>
                    <span className="chg-path" title={c.path}>
                      {c.path}
                    </span>
                  </span>
                  <span className="rcell" role="gridcell">
                    {exiting ? (
                      <span className="chg-rejected">已拒绝</span>
                    ) : workspaceOnly ? (
                      <span className={"chg-workspace" + (status?.conflict ? " conflict" : "")} title={status?.blocker ?? undefined}>
                        {status?.conflict ? "Git 冲突" : "工作区"}
                      </span>
                    ) : status?.accepted && !status.remaining ? (
                      <span className="chg-accepted">已接受</span>
                    ) : status?.rejected ? (
                      <span className="chg-rejected">已拒绝</span>
                    ) : status?.blocker ? (
                      <span className="chg-blocked" title={status.blocker}>需处理</span>
                    ) : (
                      <button
                        className="chg-accept"
                        tabIndex={index === rovingIndex ? 0 : -1}
                        disabled={globalDecisionPending || pendingAccepts.has(fileAcceptKey(c.path)) || pendingRejects.has(`file:${c.path}`)}
                        title={`接受 ${c.path}`}
                        onClick={(event) => { event.stopPropagation(); void acceptFileChange(c.path); }}
                      >{pendingAccepts.has(fileAcceptKey(c.path)) ? "…" : "接受"}</button>
                    )}
                    {!workspaceOnly && !exiting && !isPathFullyAccepted(c.path) && !isPathRejected(c.path) && (
                      <button
                        className={"chg-rb" + (confirmPath === c.path ? " confirm" : "")}
                        // 只有 roving 落点那一行的行内按钮进 tab 序，否则 Tab 会把整列按钮走一遍
                        tabIndex={index === rovingIndex ? 0 : -1}
                        disabled={globalDecisionPending || pendingAccepts.has(fileAcceptKey(c.path)) || pendingRejects.has(`file:${c.path}`)}
                        title={confirmPath === c.path ? "再次点击确认拒绝" : "拒绝并恢复此文件"}
                        aria-label={
                          confirmPath === c.path ? `再次确认，拒绝 ${c.path}` : `拒绝并恢复 ${c.path}`
                        }
                        onClick={(event) => {
                          event.stopPropagation();
                          void doRollback(c.path);
                        }}
                      >
                        {pendingRejects.has(`file:${c.path}`) ? "…" : confirmPath === c.path ? "确认?" : "拒绝"}
                      </button>
                    )}
                  </span>
                </div>
              );
            })}
          </div>
        )}
      </div>
      <div className="changes-view">
        {reviewStatusError && <div className="panel-error" role="alert">{reviewStatusError}</div>}
        {error && <div className="panel-error">{error}</div>}
        {notice && <div className="panel-note">{notice}</div>}
        {!path && <div className="empty">没有可查看的变更。</div>}
        {path && diff && !diff.supported && (
          <div className="chg-meta">
            <div className="canvas-head">
              <span className="path">{diff.path}</span>
              {reviewActions}
            </div>
            <div className="empty">
              此文件不支持行级 diff(blob 缺失或二进制)。
              <br />
              类型:{diff.change_type ?? "—"} · before {shortHash(diff.before_hash)} → after{" "}
              {shortHash(diff.after_hash)}
            </div>
            {!selectedWorkspaceOnly && !selectedPathExiting && !isPathFullyAccepted(path) && !isPathRejected(path) && <div className="chg-meta-actions">
              <button
                className={"btn danger sm" + (confirmPath === path ? " confirm" : "")}
                disabled={globalDecisionPending || pendingAccepts.has(fileAcceptKey(path)) || pendingRejects.has(`file:${path}`)}
                onClick={() => void doRollback(path)}
              >
                {pendingRejects.has(`file:${path}`) ? "恢复中…" : confirmPath === path ? "确认拒绝?" : "拒绝并恢复"}
              </button>
            </div>}
          </div>
        )}
        {path && diff?.supported && (
          <>
            <div className="canvas-head">
              <span className="path">{diff.path}</span>
              <span className="stat diffstat">
                <span className="add">+{adds}</span> <span className="del">−{dels}</span>
              </span>
              {changePoints.length > 0 && (
                <span
                  className="kact"
                  title="F7 下一个变更，Shift + F7 上一个"
                  aria-live="polite"
                >
                  F7 导航 {f7Idx >= 0 ? `${f7Idx + 1}/${changePoints.length}` : changePoints.length}
                </span>
              )}
              {running && (
                <span className="following">
                  <i />
                  live following
                </span>
              )}
              {reviewActions}
            </div>
            <div
              className="diff-body"
              ref={diffBodyRef}
            >
              {diff.truncated && <div className="dl hunk">diff 过大,已截断(全删全增)</div>}
              {lines.map((l, i) =>
                l.kind === "hunk" ? (
                  <div className="dl hunk" key={i} data-dline={i}>
                    {l.text}
                  </div>
                ) : (
                  <div
                    className={
                      "dl " +
                      l.kind +
                      (i >= freshFrom && i <= freshTo && freshFrom >= 0 ? " fresh" : "") +
                      (f7Idx >= 0 && changePoints[f7Idx] === i ? " f7-cur" : "")
                    }
                    key={i}
                    data-dline={i}
                  >
                    <span className="no">{l.new_no ?? l.old_no ?? ""}</span>
                    <span className="code">{l.text}</span>
                    {(l.kind === "add" || l.kind === "del") && l.line_id && !selectedWorkspaceOnly && !selectedPathExiting && (
                      <button className="diff-line-accept" disabled={globalDecisionPending || pendingAccepts.has(fileAcceptKey(path)) || pendingRejects.has(`file:${path}`) || acceptedLineKeys.has(lineAcceptKey(path, l.line_id)) || l.review_state === "accepted" || l.review_state === "rejected" || isPathFullyAccepted(path) || isPathRejected(path) || pathStatus.get(path)?.safe_to_accept === false} onClick={() => void acceptDiffLine(path, l.line_id!)}>{l.review_state === "rejected" || isPathRejected(path) ? "已拒绝" : acceptedLineKeys.has(lineAcceptKey(path, l.line_id)) || l.review_state === "accepted" || isPathFullyAccepted(path) ? "已接受" : "接受行"}</button>
                    )}
                  </div>
                )
              )}
              {lines.length === 0 && <div className="empty">无 diff 内容。</div>}
            </div>
          </>
        )}
        {path && !diff && !error && <div className="empty">加载 diff…</div>}
      </div>
    </div>
  );
}

function typeLabel(c: FileChange): string {
  switch (c.change_type) {
    case "create":
      return "new";
    case "modify":
      return "mod";
    case "delete":
      return "del";
    case "rename":
      return "ren";
  }
}

function withReviewCounts(status: ReviewGitStatus, paths: ReviewGitStatus["paths"]): ReviewGitStatus {
  const taskPaths = paths.filter((path) => path.scope !== "workspace");
  const acceptedCount = taskPaths.filter((path) => path.accepted).length;
  const rejectedCount = taskPaths.filter((path) => path.rejected).length;
  const remainingCount = taskPaths.filter((path) => path.remaining).length;
  const conflictCount = taskPaths.filter((path) => path.conflict).length;
  return {
    ...status,
    paths,
    accepted_count: acceptedCount,
    rejected_count: rejectedCount,
    remaining_count: remainingCount,
    conflict_count: conflictCount,
    can_accept_all: remainingCount > 0 && conflictCount === 0,
  };
}

function optimisticallyResolvePath(
  status: ReviewGitStatus,
  targetPath: string,
  decision: "accepted" | "rejected",
): ReviewGitStatus {
  return withReviewCounts(status, status.paths.map((path) => path.path === targetPath ? {
    ...path,
    accepted: decision === "accepted",
    rejected: decision === "rejected",
    remaining: false,
    conflict: false,
    blocker: null,
    accepted_items: decision === "accepted" ? path.accepted_items + path.remaining_items : 0,
    rejected_items: decision === "rejected" ? path.accepted_items + path.rejected_items + path.remaining_items : path.rejected_items,
    remaining_items: 0,
  } : path));
}

function optimisticallyAcceptAll(status: ReviewGitStatus): ReviewGitStatus {
  let next = status;
  for (const path of status.paths) {
    if (path.scope !== "workspace" && path.remaining && !path.conflict) {
      next = optimisticallyResolvePath(next, path.path, "accepted");
    }
  }
  return next;
}

/** 读屏用的完整中文，替掉只有视觉意义的三字母缩写 + 颜色。 */
function typeFullLabel(c: FileChange): string {
  switch (c.change_type) {
    case "create":
      return "新增文件";
    case "modify":
      return "修改文件";
    case "delete":
      return "删除文件";
    case "rename":
      return "重命名文件";
  }
}

function shortHash(h: string | null | undefined): string {
  return h ? h.slice(0, 8) : "—";
}

// ---------- Files ----------

interface DirectoryState {
  entries: FileTreeEntry[];
  truncated: boolean;
  loading: boolean;
  error: string | null;
}

interface FilesPanelSession {
  workspacePath: string | null;
  directories: Record<string, DirectoryState>;
  expanded: Set<string>;
  selectedPath: string | null;
  file: FileContent | null;
  draft: string;
  dirty: boolean;
  editing: boolean;
}

function FilesPanel({
  taskId,
  workspacePath,
  workspaceAttached,
  running,
}: {
  taskId: string;
  workspacePath: string | null;
  workspaceAttached: boolean;
  running: boolean;
}) {
  const sessionKey = `${taskId}:${workspacePath ?? "none"}:files`;
  const session = readPanelSession<FilesPanelSession>(sessionKey);
  const [directories, setDirectories] = useState<Record<string, DirectoryState>>(session?.directories ?? {});
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set(session?.expanded ?? []));
  const [selectedPath, setSelectedPath] = useState<string | null>(session?.selectedPath ?? null);
  const [file, setFile] = useState<FileContent | null>(session?.file ?? null);
  const [draft, setDraft] = useState(session?.draft ?? "");
  const [dirty, setDirty] = useState(session?.dirty ?? false);
  const [editing, setEditing] = useState(session?.editing ?? false);
  const [fileError, setFileError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [reloadToken, setReloadToken] = useState(0);
  const [contextMenuTarget, setContextMenuTarget] = useState<FileContextMenuTarget | null>(null);
  const navigation = useAppStore((state) => state.workbenchFiles[taskId] ?? null);
  const taskTitle = useTasksStore((state) =>
    state.details[taskId]?.task.title
    ?? state.tasks.find((task) => task.id === taskId)?.title
    ?? "当前任务"
  );
  const handledNavigationRef = useRef(0);
  const selectedRowRef = useRef<HTMLButtonElement | null>(null);
  const textAreaRef = useRef<HTMLTextAreaElement | null>(null);
  const initializedWorkspaceRef = useRef<{ path: string | null; attached: boolean } | null>(null);
  const restoredSessionRef = useRef(Boolean(session && session.workspacePath === workspacePath));
  const preserveDirtyDraftRef = useRef(Boolean(session?.dirty && session.file && session.selectedPath));
  useRememberPanelSession<FilesPanelSession>(sessionKey, {
    workspacePath,
    directories,
    expanded,
    selectedPath,
    file,
    draft,
    dirty,
    editing,
  });
  // 未保存时的二次确认走项目自研的 armed 模式：window.confirm 在 Tauri WebView 里
  // 抢焦点、无法主题化，也拿不到统一焦点环。
  const [pendingPath, setPendingPath] = useState<string | null>(null);
  const switchGuard = useArmedAction(() => {
    if (!pendingPath) return;
    setSelectedPath(pendingPath);
    setPendingPath(null);
  });
  const reloadGuard = useArmedAction(() => setReloadToken((value) => value + 1));
  const selectedIsImage = Boolean(selectedPath && isLocalRasterReference(selectedPath));

  const loadDirectory = useCallback(
    async (path: string) => {
      if (!workspacePath || !workspaceAttached) return;
      setDirectories((current) => ({
        ...current,
        [path]: {
          entries: current[path]?.entries ?? [],
          truncated: current[path]?.truncated ?? false,
          loading: true,
          error: null,
        },
      }));
      try {
        const listing = await fileList(workspacePath, path || null);
        setDirectories((current) => ({
          ...current,
          [path]: { ...listing, loading: false, error: null },
        }));
      } catch (cause) {
        setDirectories((current) => ({
          ...current,
          [path]: {
            entries: current[path]?.entries ?? [],
            truncated: false,
            loading: false,
            error: String(cause),
          },
        }));
      }
    },
    [workspacePath, workspaceAttached],
  );

  useEffect(() => {
    const initialized = initializedWorkspaceRef.current;
    if (initialized?.path === workspacePath && initialized.attached === workspaceAttached) return;
    initializedWorkspaceRef.current = { path: workspacePath, attached: workspaceAttached };
    if (restoredSessionRef.current) {
      restoredSessionRef.current = false;
      if (workspacePath && workspaceAttached && Object.keys(directories).length === 0) void loadDirectory("");
      return;
    }
    setDirectories({});
    setExpanded(new Set());
    setSelectedPath(null);
    setFile(null);
    setDraft("");
    setDirty(false);
    setEditing(false);
    setFileError(null);
    setSaveError(null);
    setPendingPath(null);
    setContextMenuTarget(null);
    if (workspacePath && workspaceAttached) void loadDirectory("");
  }, [workspacePath, workspaceAttached, loadDirectory]);

  useEffect(() => {
    if (!workspacePath || !workspaceAttached || !selectedPath) {
      setFile(null);
      return;
    }
    if (selectedIsImage) {
      setFile(null);
      setDraft("");
      setDirty(false);
      setEditing(false);
      setFileError(null);
      setSaveError(null);
      return;
    }
    if (preserveDirtyDraftRef.current) {
      preserveDirtyDraftRef.current = false;
      return;
    }
    let disposed = false;
    setFile(null);
    setFileError(null);
    setSaveError(null);
    void fileRead(workspacePath, selectedPath)
      .then((next) => {
        if (disposed) return;
        setFile(next);
        setDraft(next.content);
        setDirty(false);
        setEditing(false);
      })
      .catch((cause) => {
        if (!disposed) setFileError(String(cause));
      });
    return () => {
      disposed = true;
    };
  }, [workspacePath, workspaceAttached, selectedPath, selectedIsImage, reloadToken]);

  useEffect(() => {
    if (!navigation || navigation.requestId === handledNavigationRef.current) return;
    handledNavigationRef.current = navigation.requestId;
    const path = navigation.path.replace(/\\/g, "/").replace(/^\/+|\/+$/g, "");
    if (!path) return;

    const pieces = path.split("/");
    const parents = pieces.slice(0, -1).map((_, index) => pieces.slice(0, index + 1).join("/"));
    setExpanded((current) => {
      const next = new Set(current);
      parents.forEach((parent) => next.add(parent));
      return next;
    });
    void Promise.all(["", ...parents].map((parent) => loadDirectory(parent)));

    if (dirty && selectedPath && selectedPath !== path) {
      switchGuard.disarm();
      setPendingPath(path);
      switchGuard.trigger();
      return;
    }
    switchGuard.disarm();
    setPendingPath(null);
    setSelectedPath(path);
  }, [dirty, loadDirectory, navigation, selectedPath, switchGuard]);

  useEffect(() => {
    if (!selectedPath) return;
    const frame = requestAnimationFrame(() => {
      selectedRowRef.current?.scrollIntoView({ block: "nearest" });
    });
    return () => cancelAnimationFrame(frame);
  }, [directories, expanded, selectedPath]);

  useEffect(() => {
    if (
      !file?.is_editable
      || !editing
      || !navigation
      || navigation.path.replace(/\\/g, "/") !== selectedPath
      || !navigation.line
    ) return;
    const textarea = textAreaRef.current;
    if (!textarea) return;
    const lines = file.content.split("\n");
    const lineIndex = Math.min(Math.max(navigation.line - 1, 0), Math.max(lines.length - 1, 0));
    let offset = 0;
    for (let index = 0; index < lineIndex; index++) offset += lines[index].length + 1;
    offset += Math.min(Math.max((navigation.column ?? 1) - 1, 0), lines[lineIndex]?.length ?? 0);
    textarea.focus();
    textarea.setSelectionRange(offset, offset);
    const lineHeight = Number.parseFloat(getComputedStyle(textarea).lineHeight) || 20;
    textarea.scrollTop = Math.max(0, lineIndex * lineHeight - textarea.clientHeight / 3);
  }, [editing, file, navigation, selectedPath]);

  const toggleDirectory = (path: string) => {
    const willOpen = !expanded.has(path);
    setExpanded((current) => {
      const next = new Set(current);
      if (willOpen) next.add(path);
      else next.delete(path);
      return next;
    });
    if (willOpen && !directories[path]) void loadDirectory(path);
  };

  const refreshTree = useCallback(async () => {
    await Promise.all(["", ...Array.from(expanded)].map((path) => loadDirectory(path)));
  }, [expanded, loadDirectory]);

  const openFileContextMenu = (path: string, event: React.MouseEvent<HTMLButtonElement>) => {
    event.preventDefault();
    setContextMenuTarget({ workspacePath: workspacePath!, path, x: event.clientX, y: event.clientY });
  };

  const suppressFolderContextMenu = (event: React.MouseEvent<HTMLButtonElement>) => {
    event.preventDefault();
    setContextMenuTarget(null);
  };

  const selectFile = (path: string) => {
    if (path === selectedPath) return;
    if (!dirty) {
      switchGuard.disarm();
      setPendingPath(null);
      setSelectedPath(path);
      return;
    }
    // 换了目标文件就重新计时，避免上一个待确认的目标被误提交
    if (pendingPath !== path) {
      switchGuard.disarm();
      setPendingPath(path);
    }
    switchGuard.trigger();
  };

  const reloadFile = () => {
    if (!dirty) {
      reloadGuard.disarm();
      setReloadToken((value) => value + 1);
      return;
    }
    reloadGuard.trigger();
  };

  const saveFile = async () => {
    if (
      !workspacePath ||
      !selectedPath ||
      !file ||
      !file.is_editable ||
      !dirty ||
      saving
    ) {
      return;
    }
    setSaving(true);
    setSaveError(null);
    try {
      const saved = await fileWrite(workspacePath, selectedPath, draft, file.revision);
      setFile(saved);
      setDraft(saved.content);
      setDirty(false);
    } catch (cause) {
      setSaveError(String(cause));
    } finally {
      setSaving(false);
    }
  };

  const renderDirectory = (path: string, depth: number): React.ReactNode => {
    const directory = directories[path];
    if (!directory) return null;
    if (directory.loading && directory.entries.length === 0) {
      return <div className="files-tree-note" style={{ paddingInlineStart: 10 + depth * 14 }}>读取中…</div>;
    }
    if (directory.error) {
      return <div className="files-tree-note error" style={{ paddingInlineStart: 10 + depth * 14 }}>{directory.error}</div>;
    }
    return (
      <>
        {directory.entries.map((entry) => {
          const isOpen = entry.is_directory && expanded.has(entry.path);
          return (
            <div className="files-tree-node" key={entry.path}>
              <button
                ref={selectedPath === entry.path ? selectedRowRef : undefined}
                type="button"
                className={`files-tree-row${selectedPath === entry.path ? " selected" : ""}`}
                style={{ paddingInlineStart: 8 + depth * 14 }}
                aria-expanded={entry.is_directory ? isOpen : undefined}
                title={entry.path}
                onClick={() => {
                  if (entry.is_directory) toggleDirectory(entry.path);
                  else selectFile(entry.path);
                }}
                onContextMenu={(event) => {
                  if (entry.is_directory) suppressFolderContextMenu(event);
                  else openFileContextMenu(entry.path, event);
                }}
              >
                <span
                  className={`files-tree-arrow${isOpen ? " is-open" : ""}`}
                  aria-hidden="true"
                >
                  {entry.is_directory ? <IconChevronRight width={11} height={11} /> : null}
                </span>
                {entry.is_directory ? <IconProjects width={13} height={13} /> : <IconFile width={13} height={13} />}
                <span>{entry.name}</span>
              </button>
              {entry.is_directory && isOpen && renderDirectory(entry.path, depth + 1)}
            </div>
          );
        })}
        {directory.truncated && (
          <div className="files-tree-note" style={{ paddingInlineStart: 10 + depth * 14 }}>
            此目录条目过多，请用搜索定位文件。
          </div>
        )}
      </>
    );
  };

  if (!workspacePath) {
    return <div className="empty">当前为用户路径（默认工作区）。文件与终端以主目录为根，写操作与命令需要批准。</div>;
  }
  if (!workspaceAttached) {
    return <div className="empty">工作区尚未就绪，暂时无法浏览或编辑本地文件。</div>;
  }

  return (
    <div className="files-wrap">
      <div className="files-tree" aria-label="工作区文件">
        <div className="files-tree-head">
          <IconProjects width={13} height={13} />
          <span>文件</span>
          <button
            type="button"
            className="files-tree-refresh"
            aria-label="刷新文件树"
            title="刷新文件树"
            aria-busy={Object.values(directories).some((directory) => directory.loading)}
            disabled={Object.values(directories).some((directory) => directory.loading)}
            onClick={() => void refreshTree()}
          >
            <IconRefresh width={13} height={13} />
          </button>
        </div>
        {renderDirectory("", 0)}
      </div>
      <div className="files-editor">
        {selectedPath ? (
          <>
            <div className="files-editor-head">
              <span className="files-path" title={selectedPath}>{selectedPath}</span>
              {file && (
                <span className="files-meta">
                  {file.total_lines} 行{file.truncated ? " · 已截断" : ""}{file.is_editable ? "" : " · 只读"}
                </span>
              )}
              {selectedIsImage && <span className="files-meta">图片预览</span>}
              {file?.is_editable && (
                <button className="btn ghost sm" onClick={() => setEditing((value) => !value)}>
                  {editing ? "取消编辑" : "编辑"}
                </button>
              )}
              <button
                className={"btn ghost sm" + (reloadGuard.armed ? " confirm" : "")}
                disabled={(!file && !selectedIsImage) || saving}
                onClick={reloadFile}
              >
                {reloadGuard.armed ? "确认放弃修改?" : "重新加载"}
              </button>
              <button
                className="btn accent sm"
                disabled={!editing || !file?.is_editable || !dirty || saving}
                onClick={() => void saveFile()}
              >
                {saving ? "保存中…" : "保存"}
              </button>
            </div>
            {switchGuard.armed && pendingPath && (
              <div className="files-guard" role="status">
                <span>
                  当前文件有未保存修改。再次点击 <strong>{pendingPath}</strong> 将放弃修改并打开它。
                </span>
                <button
                  className="btn ghost sm"
                  onClick={() => {
                    switchGuard.disarm();
                    setPendingPath(null);
                  }}
                >
                  取消
                </button>
              </div>
            )}
            {running && <div className="files-running">智能体正在运行；保存时会检测磁盘是否已被改动。</div>}
            {fileError && <div className="panel-error">{fileError}</div>}
            {saveError && <div className="panel-error">{saveError}</div>}
            {!file && !fileError && !selectedIsImage && <div className="empty">读取文件…</div>}
            {selectedIsImage && selectedPath && (
              <div className="files-image-preview md">
                <LocalImageArtifact
                  key={`${selectedPath}:${reloadToken}`}
                  href={selectedPath}
                  alt={selectedPath.split("/").pop() ?? "图片"}
                  label={selectedPath.split("/").pop() ?? "图片"}
                  taskId={taskId}
                  workspacePath={workspacePath}
                />
              </div>
            )}
            {file && !file.is_editable && (
              <div className="files-readonly">
                <IconFile width={17} height={17} />
                <strong>此文件仅可预览</strong>
                <span>{file.truncated ? "文件超过 512 KiB。" : "文件包含二进制或非 UTF-8 内容。"}</span>
              </div>
            )}
            {file?.is_editable && editing && (
              <textarea
                ref={textAreaRef}
                className="files-textarea"
                value={draft}
                spellCheck={false}
                onChange={(event) => {
                  setDraft(event.target.value);
                  setDirty(event.target.value !== file.content);
                }}
                onKeyDown={(event) => {
                  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
                    event.preventDefault();
                    void saveFile();
                  }
                }}
              />
            )}
            {file?.is_editable && !editing && (
              <FileCodePreview
                path={selectedPath}
                content={file.content}
                activeLine={navigation?.path.replace(/\\/g, "/") === selectedPath ? navigation.line : null}
                ariaLabel={`${selectedPath} 只读预览`}
                className="files-code-preview"
              />
            )}
          </>
        ) : (
          <div className="files-empty">
            <IconEditor width={20} height={20} />
            <strong>选择一个文件</strong>
            <span>从左侧目录树打开文本文件后，可直接编辑并显式保存。</span>
          </div>
        )}
      </div>
      <FileContextMenu
        target={contextMenuTarget}
        tasks={[{ id: taskId, title: taskTitle }]}
        onDismiss={() => setContextMenuTarget(null)}
      />
    </div>
  );
}

// ---------- Terminal ----------

const TERMINAL_SIDEBAR_STORAGE_KEY = "r-code.terminal.sidebar-collapsed";

function initialTerminalSidebarCollapsed(): boolean {
  try {
    return window.localStorage.getItem(TERMINAL_SIDEBAR_STORAGE_KEY) === "true";
  } catch {
    return false;
  }
}

function rememberTerminalSidebarCollapsed(collapsed: boolean): void {
  try {
    window.localStorage.setItem(TERMINAL_SIDEBAR_STORAGE_KEY, String(collapsed));
  } catch {
    // Restricted WebViews may reject localStorage. The current panel still keeps its state.
  }
}

function defaultShell(): string {
  const ua = navigator.userAgent;
  if (/windows/i.test(ua)) return "auto";
  if (/mac/i.test(ua)) return "zsh";
  return "bash";
}

/** .lamp 只有颜色，读屏得靠这段文本。 */
function terminalStateLabel(t: TerminalInfo): string {
  if (t.state === "exited") return "已退出";
  if (t.state === "agent") return "外部 Agent 运行中";
  return t.is_busy ? "运行中" : "空闲";
}

function shellName(shell: string): string {
  const parts = shell.split(/[\\/]/).filter(Boolean);
  const name = parts[parts.length - 1] ?? shell;
  return name.replace(/\.exe$/i, "") || "shell";
}

function terminalTheme() {
  const style = getComputedStyle(document.documentElement);
  const token = (name: string, fallback: string) => style.getPropertyValue(name).trim() || fallback;
  return {
    background: token("--bg-inset", "#131417"),
    foreground: token("--fg", "#e4e6ea"),
    cursor: token("--accent", "#7aa2f7"),
    cursorAccent: token("--bg-inset", "#131417"),
    selectionBackground: token("--tint-accent-hi", "rgba(122, 162, 247, .28)"),
    black: token("--bg-app", "#16171b"),
    brightBlack: token("--fg-faint", "#767b84"),
    red: token("--danger", "#d97066"),
    brightRed: token("--danger", "#d97066"),
    green: token("--success", "#63b183"),
    brightGreen: token("--success", "#63b183"),
    yellow: token("--warning", "#cfa05c"),
    brightYellow: token("--warning", "#cfa05c"),
    blue: token("--accent", "#7aa2f7"),
    brightBlue: token("--accent", "#7aa2f7"),
    magenta: token("--accent-2", "#9d7cd8"),
    brightMagenta: token("--accent-2", "#9d7cd8"),
    cyan: token("--accent", "#7aa2f7"),
    brightCyan: token("--accent", "#7aa2f7"),
    white: token("--fg-muted", "#9ba0a8"),
    brightWhite: token("--fg", "#e4e6ea"),
  };
}

const TERMINAL_INPUT_BATCH_MS = 4;

/**
 * 严格保序、单请求在途的终端输入缓冲。
 *
 * 人工键入最多只增加 4ms；粘贴、按键连发以及 IPC 在途期间到达的数据会合并成
 * 下一次写入，避免“一字符一个 Promise”形成不断增长的队列。控制键立即冲刷，
 * 且不做伪本地回显——PowerShell 行编辑、密码提示和 TUI 必须以真实 PTY 为准。
 */
export function createTerminalInputBuffer(
  send: (chunk: string) => Promise<void>,
  onError: (cause: unknown) => void,
  delayMs = TERMINAL_INPUT_BATCH_MS,
) {
  let pending = "";
  let timer: number | null = null;
  let inFlight: Promise<void> | null = null;
  let disposed = false;

  const clearTimer = () => {
    if (timer == null) return;
    window.clearTimeout(timer);
    timer = null;
  };

  const pump = (): Promise<void> | null => {
    if (inFlight || !pending) return inFlight;
    clearTimer();
    const chunk = pending;
    pending = "";
    const current = send(chunk)
      .catch(onError)
      .then(() => undefined)
      .finally(() => {
        if (inFlight !== current) return;
        inFlight = null;
        // 数据已经等待过一次 IPC 往返，不再额外施加批处理延迟。
        if (pending) void pump();
      });
    inFlight = current;
    return current;
  };

  const schedule = () => {
    if (timer != null || inFlight || !pending) return;
    timer = window.setTimeout(() => {
      timer = null;
      void pump();
    }, delayMs);
  };

  const flush = async () => {
    clearTimer();
    while (pending || inFlight) {
      if (!inFlight) pump();
      const current = inFlight;
      if (current) await current;
    }
  };

  return {
    push(data: string) {
      if (disposed || !data) return;
      pending += data;
      if (/[\r\n\x03]/.test(data) || data.startsWith("\x1b")) {
        clearTimer();
        void pump();
      } else {
        schedule();
      }
    },
    flush,
    dispose() {
      if (disposed) return;
      disposed = true;
      clearTimer();
      void flush();
    },
  };
}

/**
 * 真正的 PTY viewport。
 *
 * 后端为 agent 保留 ANSI-free 的 terminal.read，同时为这个唯一的本机渲染器提供
 * 原始字节快照 + 游标增量。这样 Ctrl+C、方向键、TUI、颜色和光标移动都不会再被
 * 降级成“命令输入框 + 日志”。
 */
function TerminalViewport({
  taskId,
  terminalId,
  onError,
}: {
  taskId: string;
  terminalId: string;
  onError: (message: string | null) => void;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const terminalRef = useRef<XtermTerminal | null>(null);
  const activeIdRef = useRef<string | null>(null);
  const cursorRef = useRef(0);
  const writeChainRef = useRef(Promise.resolve());
  const pullStateRef = useRef({ terminalId, running: false, requested: false });
  const [readyForId, setReadyForId] = useState<string | null>(null);
  const ready = readyForId === terminalId;

  const enqueueWrite = useCallback(
    (targetId: string, output: string, reset: boolean) => {
      writeChainRef.current = writeChainRef.current
        .then(
          () =>
            new Promise<void>((resolve) => {
              const terminal = terminalRef.current;
              if (!terminal || activeIdRef.current !== targetId) {
                resolve();
                return;
              }
              if (reset) terminal.reset();
              if (!output) {
                resolve();
                return;
              }
              terminal.write(output, resolve);
            })
        )
        .catch((cause) => {
          onError(String(cause));
        });
      return writeChainRef.current;
    },
    [onError]
  );

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    let disposed = false;
    let resizeFrame: number | null = null;
    let resizeRetryTimer: ReturnType<typeof setTimeout> | null = null;
    let terminal: XtermTerminal | null = null;
    let fit: FitAddon | null = null;
    let desiredSize = { cols: 0, rows: 0 };
    let confirmedSize = { cols: 0, rows: 0 };
    let resizeInFlight = false;
    let resizeFailures = 0;
    let inputDisposable: { dispose: () => void } | null = null;
    let inputBuffer: ReturnType<typeof createTerminalInputBuffer> | null = null;
    setReadyForId(null);
    activeIdRef.current = terminalId;
    cursorRef.current = 0;
    writeChainRef.current = Promise.resolve();
    pullStateRef.current = { terminalId, running: false, requested: false };

    const report = (cause: unknown) => {
      if (!disposed) onError(String(cause));
    };
    const sameSize = (
      left: { cols: number; rows: number },
      right: { cols: number; rows: number },
    ) => left.cols === right.cols && left.rows === right.rows;
    const flushResize = () => {
      if (
        disposed
        || resizeInFlight
        || resizeRetryTimer != null
        || sameSize(desiredSize, confirmedSize)
      ) return;

      const target = { ...desiredSize };
      resizeInFlight = true;
      void terminalResize(taskId, terminalId, target.cols, target.rows)
        .then(() => {
          if (disposed) return;
          confirmedSize = target;
          resizeFailures = 0;
        })
        .catch(() => {
          if (disposed) return;
          resizeFailures += 1;
          const retryDelay = Math.min(1_000, 120 * (2 ** Math.min(resizeFailures - 1, 3)));
          resizeRetryTimer = setTimeout(() => {
            resizeRetryTimer = null;
            flushResize();
          }, retryDelay);
        })
        .finally(() => {
          resizeInFlight = false;
          if (!disposed && resizeRetryTimer == null && !sameSize(desiredSize, confirmedSize)) {
            flushResize();
          }
        });
    };
    const resize = () => {
      if (disposed || !terminal || !fit || terminalRef.current !== terminal) return;
      try {
        fit.fit();
        const { cols, rows } = terminal;
        if (cols <= 0 || rows <= 0 || sameSize(desiredSize, { cols, rows })) return;
        desiredSize = { cols, rows };
        flushResize();
      } catch {
        // 面板刚挂载或被隐藏时 xterm 可能尚无可测尺寸；下一帧会重试。
      }
    };
    const scheduleResize = () => {
      if (resizeFrame != null) cancelAnimationFrame(resizeFrame);
      resizeFrame = requestAnimationFrame(() => {
        resizeFrame = null;
        resize();
      });
    };
    const observer = new ResizeObserver(scheduleResize);
    observer.observe(host);
    const themeObserver = new MutationObserver(() => {
      if (terminal && terminalRef.current === terminal) {
        terminal.options.theme = terminalTheme();
      }
    });
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });

    // xterm 仅在真正打开 Terminal 页时下载，避免把终端模拟器塞进首屏包。
    void Promise.all([import("@xterm/xterm"), import("@xterm/addon-fit")])
      .then(async ([xterm, fitModule]) => {
        if (disposed) return;
        terminal = new xterm.Terminal({
          fontFamily: getComputedStyle(document.documentElement).getPropertyValue("--font-mono").trim() || "monospace",
          fontSize: 12,
          lineHeight: 1.42,
          cursorBlink: true,
          scrollback: 10_000,
          theme: terminalTheme(),
        });
        fit = new fitModule.FitAddon();
        terminal.loadAddon(fit);
        terminal.open(host);
        terminalRef.current = terminal;
        // PTY 必须尽早拿到真实 viewport，不能被较慢的历史快照读取阻塞。
        scheduleResize();
        void document.fonts?.ready.then(() => {
          if (!disposed) scheduleResize();
        });
        inputBuffer = createTerminalInputBuffer(
          (data) => terminalSend(taskId, terminalId, data, false),
          report,
        );
        inputDisposable = terminal.onData((data) => {
          inputBuffer?.push(data);
        });

        const snapshot = await terminalRawSnapshot(taskId, terminalId);
        if (disposed) return;
        cursorRef.current = snapshot.cursor;
        await enqueueWrite(terminalId, snapshot.output, true);
        if (disposed) return;
        onError(null);
        setReadyForId(terminalId);
        scheduleResize();
      })
      .catch(report);

    return () => {
      disposed = true;
      if (resizeFrame != null) cancelAnimationFrame(resizeFrame);
      if (resizeRetryTimer != null) clearTimeout(resizeRetryTimer);
      observer.disconnect();
      themeObserver.disconnect();
      inputDisposable?.dispose();
      inputBuffer?.dispose();
      if (terminal && terminalRef.current === terminal) terminalRef.current = null;
      if (activeIdRef.current === terminalId) activeIdRef.current = null;
      terminal?.dispose();
    };
  }, [enqueueWrite, onError, taskId, terminalId]);

  const pullOutput = useCallback(async () => {
    const state = pullStateRef.current;
    if (!ready || state.terminalId !== terminalId || activeIdRef.current !== terminalId) return;
    if (state.running) {
      state.requested = true;
      return;
    }

    state.running = true;
    try {
      do {
        state.requested = false;
        const batch = await terminalRawSince(taskId, terminalId, cursorRef.current);
        if (pullStateRef.current !== state || activeIdRef.current !== terminalId) return;
        cursorRef.current = batch.cursor;
        if (batch.reset || batch.output) {
          await enqueueWrite(terminalId, batch.output, batch.reset);
        }
      } while (state.requested);
    } catch (cause) {
      if (pullStateRef.current === state) onError(String(cause));
    } finally {
      if (pullStateRef.current === state) state.running = false;
    }
  }, [enqueueWrite, onError, ready, taskId, terminalId]);

  useEffect(() => {
    if (!ready) return;
    let disposed = false;
    let unlisten: (() => void) | null = null;
    void onTerminalOutput((outputTerminalId) => {
      if (outputTerminalId === terminalId) void pullOutput();
    })
      .then((nextUnlisten) => {
        if (disposed) nextUnlisten();
        else {
          unlisten = nextUnlisten;
          // Snapshot 与事件订阅之间可能恰好到达一段输出；监听真正建立后补读一次。
          void pullOutput();
        }
      })
      .catch((cause) => onError(String(cause)));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [onError, pullOutput, ready, terminalId]);

  usePoll(
    pullOutput,
    1_000,
    ready
  );

  return (
    <div
      ref={hostRef}
      className="term-viewport"
      aria-label="交互式终端"
      role="application"
    />
  );
}

function TerminalPanel({
  taskId,
  workspacePath,
  workspaceAttached,
}: {
  taskId: string;
  workspacePath: string | null;
  workspaceAttached: boolean;
}) {
  const { runWithCodexCli } = useCodexCliGate();
  const sessionKey = `${taskId}:terminal`;
  const session = readPanelSession<{ selectedTerminalId: string | null }>(sessionKey);
  const sidebarId = useId();
  const [terms, setTerms] = useState<TerminalInfo[]>([]);
  const [selId, setSelId] = useState<string | null>(session?.selectedTerminalId ?? null);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(initialTerminalSidebarCollapsed);
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [rowFocus, setRowFocus] = useState(-1);
  useRememberPanelSession(sessionKey, { selectedTerminalId: selId });

  const toggleSidebar = () => {
    setSidebarCollapsed((collapsed) => {
      const next = !collapsed;
      rememberTerminalSidebarCollapsed(next);
      return next;
    });
  };

  const list = useCallback(async () => {
    try {
      const ts = await terminalList(taskId);
      setTerms(ts);
      setSelId((current) =>
        current != null && ts.some((terminal) => terminal.id === current)
          ? current
          : ts[0]?.id ?? null
      );
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, [taskId]);

  useEffect(() => {
    void list();
  }, [list]);

  // 状态由真实 PTY 输出和 shell integration 推进；定期刷新列表才能及时显示
  // Busy / Agent / Exited，而不是只在第一次打开画布时读到一份静态状态。
  // FX-15：侧栏折叠时看不到状态行，降到 8s 省 IPC。
  usePoll(list, sidebarCollapsed ? 8000 : 1200, true);

  const sel = selId;
  const selIndex = terms.findIndex((item) => item.id === sel);
  const rovingIndex = rovingIndexOf(rowFocus, selIndex, terms.length);

  const create = async (shell = defaultShell()) => {
    if (!workspacePath || !workspaceAttached) {
      setError("先为这个会话附加一个文件夹，才能打开终端。");
      return;
    }
    setCreating(true);
    setError(null);
    try {
      const id = await terminalCreate(taskId, shell);
      await list();
      setSelId(id);
    } catch (e) {
      setError(String(e));
    } finally {
      setCreating(false);
    }
  };

  const createCodex = async () => {
    if (!workspacePath || !workspaceAttached) {
      setError("先为这个会话附加一个文件夹，才能打开 Codex CLI。");
      return;
    }
    setCreating(true);
    setError(null);
    try {
      await runWithCodexCli({ feature: "Codex CLI 终端" }, async () => {
        const id = await terminalCreateCodex(taskId);
        await list();
        setSelId(id);
      });
    } catch (e) {
      setError(String(e));
    } finally {
      setCreating(false);
    }
  };

  const kill = async (id: string) => {
    setError(null);
    try {
      await terminalKill(taskId, id);
      await list();
    } catch (e) {
      setError(String(e));
    }
  };

  const reportViewportError = useCallback((message: string | null) => {
    setError(message);
  }, []);

  return (
    <div
      className={`term-wrap${sidebarCollapsed ? " is-sidebar-collapsed" : ""}`}
      data-terminal-sidebar={sidebarCollapsed ? "collapsed" : "expanded"}
    >
      <div className="term-side" id={sidebarId} aria-hidden={sidebarCollapsed || undefined}>
        <div className="term-new-wrap">
          <button className="btn sm term-new" disabled={creating || !workspacePath || !workspaceAttached} onClick={() => void create()}>
            <IconPlus width={11} height={11} /> 新建终端
          </button>
          <Menu
            className="term-launcher-root"
            label="新建终端类型"
            placement="down"
            align="left"
            menuClassName="term-launcher"
            disabled={creating || !workspacePath || !workspaceAttached}
            trigger={
              <button className="btn sm term-new-more" aria-label="选择终端类型">
                <IconChevronDown width={11} height={11} />
              </button>
            }
          >
            {({ close }) => <>
              <MenuItem close={close} hint="工作区根目录" onSelect={() => void create(defaultShell())}>系统 Shell</MenuItem>
              <MenuItem close={close} hint="使用它自己的登录与权限" onSelect={() => void createCodex()}>Codex CLI</MenuItem>
            </>}
          </Menu>
        </div>
        {terms.length > 0 && (
          <div className="term-options" role="grid" aria-label="终端列表">
            {terms.map((t, index) => (
              <div
                key={t.id}
                id={termRowId(index)}
                role="row"
                aria-selected={sel === t.id}
                tabIndex={index === rovingIndex ? 0 : -1}
                className={"term-row ring-inset" + (sel === t.id ? " sel" : "")}
                onFocus={() => setRowFocus(index)}
                onClick={() => setSelId(t.id)}
                onKeyDown={(event) => {
                  if (moveRowFocus(event, index, terms.length, termRowId)) return;
                  if (!isRowActivate(event)) return;
                  event.preventDefault();
                  setSelId(t.id);
                }}
              >
                <span className="rcell rcell-main" role="gridcell">
                  <IconTerminal width={12} height={12} aria-hidden="true" />
                  <span className="t-id" title={t.id}>
                    {shellName(t.shell)}
                  </span>
                  {/* 状态灯是纯颜色编码，读屏走后面的文本 */}
                  <span
                    className={"lamp" + (t.state === "exited" ? " done" : t.is_busy ? " run" : "")}
                    aria-hidden="true"
                  />
                  <span className="sr-only">{terminalStateLabel(t)}</span>
                </span>
                <span className="rcell" role="gridcell">
                  <button
                    className="t-kill"
                    tabIndex={index === rovingIndex ? 0 : -1}
                    title="终止终端"
                    aria-label={`终止终端 ${t.id.slice(0, 8)}`}
                    onClick={(e) => {
                      e.stopPropagation();
                      void kill(t.id);
                    }}
                  >
                    ✕
                  </button>
                </span>
              </div>
            ))}
          </div>
        )}
        {terms.length === 0 && <div className="term-hint">无终端</div>}
      </div>
      <button
        type="button"
        className="term-side-toggle ring-inset"
        onClick={toggleSidebar}
        aria-controls={sidebarId}
        aria-expanded={!sidebarCollapsed}
        aria-label={sidebarCollapsed ? "展开终端列表" : "收起终端列表"}
        title={sidebarCollapsed ? "展开终端列表" : "收起终端列表"}
      >
        {sidebarCollapsed
          ? <IconChevronRight width={13} height={13} aria-hidden="true" />
          : <IconChevronLeft width={13} height={13} aria-hidden="true" />}
      </button>
      <div className="term-main">
        {error && <div className="panel-error" role="alert">{error}</div>}
        {sel ? (
          <TerminalViewport taskId={taskId} terminalId={sel} onError={reportViewportError} />
        ) : (
          <div className="empty">
            {workspacePath && workspaceAttached
              ? "还没有终端 — 点击左侧「新建终端」。终端运行在此设备，cwd 为工作区根。"
              : "附加一个工作区后，才能使用终端。"}
          </div>
        )}
      </div>
    </div>
  );
}

// ---------- Review ----------

const VER_STATUS: Record<string, { cls: string; label: string }> = {
  running: { cls: "run", label: "运行中" },
  passed: { cls: "ok", label: "通过" },
  failed: { cls: "bad", label: "失败" },
  timeout: { cls: "bad", label: "超时" },
  stale: { cls: "", label: "已过期" },
  superseded: { cls: "", label: "被取代" },
};

function ReviewPanel({ taskId }: { taskId: string }) {
  const sessionKey = `${taskId}:review`;
  const session = readPanelSession<{
    command: string;
    requestingChange: boolean;
    feedback: string;
    openVerificationId: string | null;
    outputs: Record<string, string>;
  }>(sessionKey);
  const [records, setRecords] = useState<VerificationRecord[]>([]);
  const [cmd, setCmd] = useState(session?.command ?? "");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [runningCmd, setRunningCmd] = useState(false);
  const [confirm, setConfirm] = useState<null | "accept" | "rollback">(null);
  const [requestingChange, setRequestingChange] = useState(session?.requestingChange ?? false);
  const [feedback, setFeedback] = useState(session?.feedback ?? "");
  const confirmTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const taskState = useTasksStore((s) => s.details[taskId]?.task.state);
  const latestRun = useTasksStore((s) => {
    const runs = s.details[taskId]?.runs;
    return runs && runs.length > 0 ? runs[runs.length - 1] : null;
  });
  const guardTrip = useMemo(() => parseGuardTrip(latestRun?.guard_trip ?? null), [latestRun?.guard_trip]);
  const checkpointSha = latestRun?.checkpoint_sha ?? null;
  // 输出查看（点击记录行展开，懒加载 + 缓存）
  const [openId, setOpenId] = useState<string | null>(session?.openVerificationId ?? null);
  const [outputs, setOutputs] = useState<Record<string, string>>(session?.outputs ?? {});
  const [rowFocus, setRowFocus] = useState(-1);
  const [delivery, setDelivery] = useState<GitDeliveryStatus | null>(null);
  const [commitMessage, setCommitMessage] = useState("");
  const [gitBusy, setGitBusy] = useState<null | "stage" | "suggest" | "commit" | "push">(null);
  const [confirmCommit, setConfirmCommit] = useState(false);
  const [pushCountdown, setPushCountdown] = useState<number | null>(null);
  useRememberPanelSession(sessionKey, {
    command: cmd,
    requestingChange,
    feedback,
    openVerificationId: openId,
    outputs,
  });

  const toggleOutput = async (id: string) => {
    if (openId === id) {
      setOpenId(null);
      return;
    }
    setOpenId(id);
    if (outputs[id] !== undefined) return;
    try {
      const text = await verificationOutput(id);
      setOutputs((o) => ({ ...o, [id]: text || "（无输出）" }));
    } catch (e) {
      setOutputs((o) => ({ ...o, [id]: `读取输出失败：${String(e)}` }));
    }
  };

  usePoll(
    async () => {
      try {
        const rs = await verificationList(taskId);
        setRecords([...rs].sort((a, b) => b.started_at.localeCompare(a.started_at)));
        setError(null);
      } catch (e) {
        setError(String(e));
      }
    },
    // FX-15：验证记录只在任务推进时高频有意义；空闲降到 8s。
    taskState === "in_progress" || taskState === "exploring" ? 2000 : 8000,
    true
  );

  const refreshDelivery = useCallback(async () => {
    try {
      setDelivery(await gitDeliveryStatus(taskId));
    } catch {
      setDelivery(null);
    }
  }, [taskId]);

  // Git delivery runs several subprocesses. Review decisions announce themselves immediately,
  // so a slow fallback poll is enough and avoids repeatedly waking Git while the panel is idle.
  usePoll(refreshDelivery, 10000, true);

  useEffect(() => {
    const onReviewStatusChanged = (event: Event) => {
      const changedTaskId = (event as CustomEvent<{ taskId?: string }>).detail?.taskId;
      if (!changedTaskId || changedTaskId === taskId) void refreshDelivery();
    };
    window.addEventListener(REVIEW_STATUS_CHANGED_EVENT, onReviewStatusChanged);
    return () => window.removeEventListener(REVIEW_STATUS_CHANGED_EVENT, onReviewStatusChanged);
  }, [refreshDelivery, taskId]);

  useEffect(() => {
    if (pushCountdown == null || pushCountdown <= 0) return;
    const timer = window.setTimeout(
      () => setPushCountdown((value) => (value == null ? null : value - 1)),
      1000
    );
    return () => window.clearTimeout(timer);
  }, [pushCountdown]);

  const suggestCommit = async () => {
    setGitBusy("suggest");
    setError(null);
    try {
      setCommitMessage(await gitSuggestCommitMessage(taskId));
    } catch (cause) {
      setError(String(cause));
    } finally {
      setGitBusy(null);
    }
  };

  const stageAccepted = async () => {
    setGitBusy("stage");
    setError(null);
    try {
      const status = await gitStageAccepted(taskId);
      setDelivery(status);
      setNotice(`已将 ${status.staged_task_paths.length} 个审核保留文件加入暂存区。`);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setGitBusy(null);
    }
  };

  const commitAccepted = async () => {
    if (!confirmCommit) {
      setConfirmCommit(true);
      window.setTimeout(() => setConfirmCommit(false), 5000);
      return;
    }
    setConfirmCommit(false);
    setGitBusy("commit");
    setError(null);
    try {
      const result = await gitCommitTask(taskId, commitMessage);
      setNotice(`已提交 ${result.sha.slice(0, 8)}。`);
      await refreshDelivery();
    } catch (cause) {
      setError(String(cause));
    } finally {
      setGitBusy(null);
    }
  };

  const pushCommitted = async () => {
    if (pushCountdown == null) {
      setPushCountdown(5);
      return;
    }
    if (pushCountdown > 0) return;
    setPushCountdown(null);
    setGitBusy("push");
    setError(null);
    try {
      const result = await gitPushTask(taskId);
      setNotice(`已推送 ${result.branch} → ${result.upstream}（${result.sha.slice(0, 8)}）。`);
      await refreshDelivery();
    } catch (cause) {
      setError(String(cause));
    } finally {
      setGitBusy(null);
    }
  };

  const run = async () => {
    const command = cmd.trim();
    if (!command || runningCmd) return;
    setRunningCmd(true);
    setError(null);
    setNotice(null);
    try {
      await runVerification(taskId, command);
      setRecords(await verificationList(taskId));
    } catch (e) {
      setError(String(e));
    } finally {
      setRunningCmd(false);
    }
  };

  const act = async (kind: "accept" | "rollback") => {
    if (confirm !== kind) {
      setConfirm(kind);
      if (confirmTimer.current) clearTimeout(confirmTimer.current);
      confirmTimer.current = setTimeout(() => setConfirm(null), 3000);
      return;
    }
    setConfirm(null);
    setError(null);
    setNotice(null);
    try {
      if (kind === "accept") {
        await acceptTask(taskId);
        setNotice("已接受全部变更,任务关闭。");
      } else if (checkpointSha) {
        await rollbackTaskToCheckpoint(taskId);
        setNotice("已回滚到最近绿灯检查点。");
      } else {
        const results = await rollbackTask(taskId);
        setNotice(`已回滚 ${results.length} 个文件。`);
      }
      await refreshDetail(taskId);
      await refreshTasks();
    } catch (e) {
      setError(String(e));
    }
  };

  const requestChanges = async () => {
    const message = feedback.trim();
    if (!message) {
      setError("请先说明希望修改的内容。");
      return;
    }
    setError(null);
    setNotice(null);
    try {
      await changeRequest(taskId, message);
      setFeedback("");
      setRequestingChange(false);
      setNotice("已发送修改请求，正在启动下一轮处理。");
      await refreshDetail(taskId);
      await refreshTasks();
    } catch (e) {
      setError(String(e));
    }
  };

  // 没有「选中」概念，roving 落点退回已展开的那一条，都没有就第一条
  const rovingIndex = rovingIndexOf(rowFocus, records.findIndex((r) => r.id === openId), records.length);

  return (
    <div className="review-wrap">
      <div className="review-gate">
        <input
          className="input"
          value={cmd}
          placeholder="验证命令，例如 cargo test 或 npm test"
          onChange={(e) => setCmd(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              void run();
            }
          }}
        />
        <button className="btn accent" disabled={!cmd.trim() || runningCmd} onClick={() => void run()}>
          {runningCmd ? "运行中…" : "运行验证"}
        </button>
      </div>
      {error && <div className="panel-error">{error}</div>}
      {notice && <div className="panel-note">{notice}</div>}
      <div className="review-list">
        {records.length === 0 && <div className="empty">还没有验证记录 — 审阅门是显式的:先跑一条命令。</div>}
        {records.length > 0 && (
          <div className="ver-items" role="list" aria-label="验证记录">
            {records.map((r, index) => {
              const st = VER_STATUS[r.status] ?? { cls: "", label: r.status };
              const open = openId === r.id;
              return (
                <div key={r.id} role="listitem">
                  {/* 行内没有嵌套按钮，可以直接用真 button（同时是输出的 disclosure） */}
                  <button
                    type="button"
                    id={verRowId(index)}
                    className={"ver-row ring-inset" + (open ? " open" : "")}
                    tabIndex={index === rovingIndex ? 0 : -1}
                    aria-expanded={open}
                    aria-controls={open ? `ver-out-${index}` : undefined}
                    onFocus={() => setRowFocus(index)}
                    onClick={() => void toggleOutput(r.id)}
                    onKeyDown={(event) => {
                      moveRowFocus(event, index, records.length, verRowId);
                    }}
                    title="展开或收起输出"
                  >
                    <span className={"st-chip " + st.cls}>{st.label}</span>
                    <span className="ver-cmd" title={r.command}>
                      {r.command}
                    </span>
                    <span className="ver-meta">
                      exit {r.exit_code ?? "—"} · {dur(r)} · {clockTime(r.started_at)}
                    </span>
                  </button>
                  {open && (
                    <pre className="ver-output" id={`ver-out-${index}`}>
                      {outputs[r.id] ?? "读取中…"}
                    </pre>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>
      {delivery && (
        <section className="review-git" aria-label="Git 提交与推送">
          <div className="review-git-head">
            <div><span>Git 交付</span><strong>{delivery.branch ?? "detached HEAD"}</strong></div>
            <small>{delivery.staged_task_paths.length} 个任务文件已暂存 · ahead {delivery.ahead} / behind {delivery.behind}</small>
          </div>
          {delivery.blockers.length > 0 && <div className="review-git-blockers">{delivery.blockers.map((blocker) => <span key={blocker}>{blocker}</span>)}</div>}
          <div className="review-git-stage">
            <button className="btn" disabled={gitBusy !== null || !delivery.can_stage} onClick={() => void stageAccepted()}>{gitBusy === "stage" ? "暂存中…" : "暂存已接受文件"}</button>
            <span>审核与 Git 分离；只有这里会改动暂存区。</span>
          </div>
          <div className="review-git-message">
            <input className="input" value={commitMessage} onChange={(event) => { setCommitMessage(event.target.value); setConfirmCommit(false); }} placeholder="提交信息（可编辑）" />
            <button className="btn" disabled={gitBusy !== null || delivery.staged_task_paths.length === 0} onClick={() => void suggestCommit()}>{gitBusy === "suggest" ? "生成中…" : "自动生成"}</button>
          </div>
          <div className="review-git-actions">
            <button className={"btn accent" + (confirmCommit ? " confirm" : "")} disabled={gitBusy !== null || !delivery.can_commit || !commitMessage.trim()} onClick={() => void commitAccepted()}>{gitBusy === "commit" ? "提交中…" : confirmCommit ? "再次点击确认提交" : "提交已暂存变更"}</button>
            <button className="btn" disabled={gitBusy !== null || !delivery.can_push || (pushCountdown != null && pushCountdown > 0)} onClick={() => void pushCommitted()}>{gitBusy === "push" ? "推送中…" : pushCountdown == null ? "推送到 upstream" : pushCountdown > 0 ? `${pushCountdown}s 后可确认` : "确认推送"}</button>
            <span>{delivery.upstream ? `upstream · ${delivery.upstream}` : "未配置 upstream，审核页不会自动创建"}</span>
          </div>
        </section>
      )}
      {requestingChange && (
        <div className="review-request">
          <label htmlFor={`review-feedback-${taskId}`}>修改说明</label>
          <textarea id={`review-feedback-${taskId}`} value={feedback} onChange={(event) => setFeedback(event.target.value)} placeholder="说明需要调整的实现、边界或测试。" autoFocus />
          <div><button className="btn accent" disabled={!feedback.trim()} onClick={() => void requestChanges()}>发送修改请求</button><button className="btn" onClick={() => setRequestingChange(false)}>取消</button></div>
        </div>
      )}
      {guardTrip && (
        <div className="guard-trip-banner" role="status">
          <strong>{guardTripLabel(guardTrip.reason)}</strong>
          <span>{guardTrip.detail}</span>
        </div>
      )}
      <div className="review-actions">
        <button
          className={"btn accent" + (confirm === "accept" ? " confirm" : "")}
          onClick={() => void act("accept")}
        >
          <IconCheck width={12} height={12} /> {confirm === "accept" ? "确认接受?" : "Accept all"}
        </button>
        <button
          className={"btn danger" + (confirm === "rollback" ? " confirm" : "")}
          onClick={() => void act("rollback")}
        >
          {confirm === "rollback"
            ? (checkpointSha ? "确认回滚到检查点?" : "确认回滚?")
            : (checkpointSha ? "回滚到检查点" : "Rollback")}
        </button>
        {taskState === "review_ready" && <button className="btn" onClick={() => setRequestingChange((open) => !open)}>{requestingChange ? "收起修改说明" : "请求修改"}</button>}
        <span className="review-hint">
          审阅门永远显式 — 接受或回滚,不留中间态。
          {guardTrip && !checkpointSha ? " 本 run 没有可用检查点,回滚退回逐文件恢复。" : ""}
          {checkpointSha ? ` 最近绿灯检查点 ${checkpointSha.slice(0, 8)}。` : ""}
        </span>
      </div>
    </div>
  );
}

function parseGuardTrip(raw: string | null | undefined): { reason: GuardTripReason; detail: string } | null {
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as { reason?: unknown; detail?: unknown };
    if (typeof parsed?.reason !== "string" || typeof parsed?.detail !== "string") return null;
    return { reason: parsed.reason as GuardTripReason, detail: parsed.detail };
  } catch {
    return null;
  }
}

function dur(r: VerificationRecord): string {
  if (!r.ended_at) return "…";
  const ms = Date.parse(r.ended_at) - Date.parse(r.started_at);
  if (Number.isNaN(ms) || ms < 0) return "—";
  return `${(ms / 1000).toFixed(1)}s`;
}
