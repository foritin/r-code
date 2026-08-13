/**
 * 单任务对话：聊天不依赖工作区；工作区范围可以在对话过程中按需附加。
 */
import { useCallback, useEffect, useMemo, useReducer, useRef, useState, type CSSProperties } from "react";
import { useAppStore } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { usePoll } from "../../lib/poll";
import {
  agentAbort,
  agentAbortSubagent,
  prepareWorkbenchWindow,
  taskChooseWorkspace,
  taskSetWorkspace,
  workspaceSetAccessMode,
} from "../../lib/ipc";
import { useProviders } from "../../lib/provider";
import { StatusBar } from "../ui/StatusBar";
import type { AgentEvent, AgentSendMode, ProjectAccessMode, SessionBranch } from "../../lib/types";
import { Timeline, type TimelineHandle } from "../room/Timeline";
import { Composer } from "../room/Composer";
import { PendingPermissions } from "../room/Permissions";
import { Canvas } from "../room/Canvas";
import { TaskActionsMenu } from "../TaskActionsMenu";
import { activityTraceReducer, createActivityTraceState } from "../room/activity";
import { IconAttach, IconHome, IconProjects, IconSidebar } from "../icons";
import { projectAccessModeLabel } from "../ProjectAccessSelector";
import { PlanShortcut } from "../plan/PlanPanel";
import { useTaskPlan } from "../plan/useTaskPlan";

const ROOM_SPLIT_STORAGE_KEY = "r-code.room.split-pct";
const DEFAULT_ROOM_SPLIT_PCT = 55;
const ROOM_SPLITTER_WIDTH = 11;
const MIN_CONVERSATION_WIDTH = 360;
const MIN_CANVAS_WIDTH = 300;
interface TaskSubagentView {
  open: boolean;
  selectedId: string | null;
  openIds: string[];
}
const taskSubagentViews = new Map<string, TaskSubagentView>();

function historicalBranchLabel(branch: SessionBranch, index: number): string {
  const timestamp = Date.parse(branch.created_at);
  const date = Number.isNaN(timestamp)
    ? "时间未知"
    : new Date(timestamp).toLocaleString([], {
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
      });
  return `历史 ${index + 1} · ${date}`;
}

interface RoomSplitBounds {
  min: number;
  max: number;
}

function getRoomSplitBounds(width: number): RoomSplitBounds {
  if (!Number.isFinite(width) || width <= ROOM_SPLITTER_WIDTH) {
    return { min: 0, max: 100 };
  }

  const available = Math.max(width - ROOM_SPLITTER_WIDTH, 1);
  const desiredMin = (MIN_CONVERSATION_WIDTH / available) * 100;
  const desiredMax = 100 - (MIN_CANVAS_WIDTH / available) * 100;

  if (desiredMin <= desiredMax) {
    return { min: desiredMin, max: desiredMax };
  }

  // 窄窗口无法同时满足理想像素宽度时，仍保证两列都有可用空间。
  return { min: 48, max: 52 };
}

function clampRoomSplit(raw: number, width: number): number {
  const { min, max } = getRoomSplitBounds(width);
  return Math.min(Math.max(raw, min), max);
}

function initialRoomSplit(): number {
  try {
    const stored = window.localStorage.getItem(ROOM_SPLIT_STORAGE_KEY);
    if (stored == null) return DEFAULT_ROOM_SPLIT_PCT;
    const value = Number(stored);
    return Number.isFinite(value) ? value : DEFAULT_ROOM_SPLIT_PCT;
  } catch {
    return DEFAULT_ROOM_SPLIT_PCT;
  }
}

export function RoomScene() {
  const currentTaskId = useAppStore((s) => s.currentTaskId);
  const goHome = useAppStore((s) => s.goHome);
  const workbenchMode = useAppStore((s) => s.workbenchMode);
  const canvasTab = useAppStore((s) => s.canvasTab);
  const setCanvasTab = useAppStore((s) => s.setCanvasTab);
  const hideWorkbench = useAppStore((s) => s.hideWorkbench);
  const restoreWorkbench = useAppStore((s) => s.restoreWorkbench);
  const expandReview = useAppStore((s) => s.expandReview);
  const detail = useTasksStore((s) => (currentTaskId ? s.details[currentTaskId] : undefined));
  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const refreshWorkspaces = useTasksStore((s) => s.refreshWorkspaces);
  const setCurrentProject = useTasksStore((s) => s.setCurrentProject);
  const workspaces = useTasksStore((s) => s.workspaces);
  const listedTask = useTasksStore((s) => currentTaskId ? s.tasks.find((task) => task.id === currentTaskId) : undefined);
  const taskSnapshot = detail?.task ?? listedTask ?? null;
  const planController = useTaskPlan(taskSnapshot?.id ?? null, taskSnapshot?.mode ?? null);

  const [scopeBusy, setScopeBusy] = useState(false);
  const [scopeError, setScopeError] = useState<string | null>(null);
  const [historyBranchId, setHistoryBranchId] = useState<string | null>(null);
  const boundProvider = useTasksStore((s) =>
    currentTaskId ? s.details[currentTaskId]?.task.provider_name ?? null : null
  );
  const providers = useProviders([currentTaskId, boundProvider]);
  const [activity, dispatchActivity] = useReducer(activityTraceReducer, createActivityTraceState());
  const [subagentView, setSubagentView] = useState<TaskSubagentView>({ open: false, selectedId: null, openIds: [] });
  const subagentPanelOpen = subagentView.open;
  const selectedSubagentId = subagentView.selectedId;
  const tlRef = useRef<TimelineHandle>(null);
  const roomRef = useRef<HTMLElement>(null);
  const convoRef = useRef<HTMLDivElement>(null);
  const splitDraggingRef = useRef(false);
  const roomSplitRef = useRef(initialRoomSplit());
  const [roomWidth, setRoomWidth] = useState(0);
  const [roomSplitPct, setRoomSplitPct] = useState(() => roomSplitRef.current);
  const [isSplitDragging, setIsSplitDragging] = useState(false);
  const autoOpenedPlanSignals = useRef(new Set<string>());
  const {
    running,
    queuedMessages,
    pendingPermissions,
    queueStamp,
    permissionStamp,
    runsStamp,
  } = useMemo(() => {
    const runs = detail?.runs ?? [];
    const queuedMessages = detail?.queued_messages ?? [];
    const pendingPermissions = detail?.permissions.filter(
      (permission) => permission.decision === "pending",
    ) ?? [];
    return {
      running: runs.some((run) => run.ended_at === null),
      queuedMessages,
      pendingPermissions,
      queueStamp: queuedMessages.map((message) => `${message.id}:${message.state}`).join("|"),
      permissionStamp: pendingPermissions.map((permission) => permission.id).join("|"),
      runsStamp: runs
        .map(
          (run) =>
            `${run.id}:${run.agent_kind}:${run.agent_label ?? ""}:${run.review_state}:${run.ended_at ?? ""}:${run.summary ?? ""}`,
        )
        .join("|"),
    };
  }, [detail]);
  const historicalBranches = useMemo(
    () => [...(detail?.branches ?? [])]
      .filter((branch) => branch.id !== detail?.active_branch.id)
      .sort((left, right) => right.created_at.localeCompare(left.created_at)),
    [detail?.active_branch.id, detail?.branches],
  );

  useEffect(() => {
    setHistoryBranchId(null);
  }, [currentTaskId]);

  useEffect(() => {
    if (!taskSnapshot) return;
    setCurrentProject(taskSnapshot.workspace_path ?? null);
  }, [setCurrentProject, taskSnapshot?.id, taskSnapshot?.workspace_path]);

  useEffect(() => {
    if (
      historyBranchId &&
      !historicalBranches.some((branch) => branch.id === historyBranchId)
    ) {
      setHistoryBranchId(null);
    }
  }, [historicalBranches, historyBranchId]);

  // A docked tool should gain horizontal room first. The host preserves the left edge whenever
  // the active monitor has space on the right and no-ops for maximized/fullscreen windows.
  useEffect(() => {
    if (workbenchMode !== "docked") return;
    void prepareWorkbenchWindow().catch(() => {
      // Window growth is an ergonomic enhancement; overlay layout remains the safe fallback.
    });
  }, [workbenchMode]);

  const updateSubagentView = useCallback((update: (current: TaskSubagentView) => TaskSubagentView) => {
    setSubagentView((current) => {
      const next = update(current);
      if (currentTaskId) taskSubagentViews.set(currentTaskId, next);
      return next;
    });
  }, [currentTaskId]);

  const inspectSubagent = useCallback((subagentId: string) => {
    updateSubagentView((current) => ({
      open: true,
      selectedId: subagentId,
      openIds: current.openIds.includes(subagentId) ? current.openIds : [...current.openIds, subagentId],
    }));
    setCanvasTab("summary");
  }, [setCanvasTab, updateSubagentView]);

  const showSubagentList = useCallback(() => {
    updateSubagentView((current) => ({ ...current, open: true, selectedId: null }));
    setCanvasTab("summary");
  }, [setCanvasTab, updateSubagentView]);

  const closeSubagentView = useCallback(() => {
    updateSubagentView((current) => ({ ...current, open: false, selectedId: null }));
  }, [updateSubagentView]);

  const backToSubagentList = useCallback(() => {
    updateSubagentView((current) => ({ ...current, open: true, selectedId: null }));
  }, [updateSubagentView]);

  const closeSubagentTab = useCallback((subagentId: string) => {
    updateSubagentView((current) => {
      const closingIndex = current.openIds.indexOf(subagentId);
      if (closingIndex < 0) return current;
      const openIds = current.openIds.filter((id) => id !== subagentId);
      if (current.selectedId !== subagentId) return { ...current, openIds };
      const selectedId = openIds[Math.min(Math.max(closingIndex - 1, 0), openIds.length - 1)] ?? null;
      return { ...current, openIds, selectedId };
    });
  }, [updateSubagentView]);

  useEffect(() => {
    dispatchActivity({ type: "reset" });
    const saved = currentTaskId ? taskSubagentViews.get(currentTaskId) : undefined;
    setSubagentView(saved ?? { open: false, selectedId: null, openIds: [] });
  }, [currentTaskId]);

  useEffect(() => {
    const view = planController.view;
    if (!currentTaskId || !view || view.plan.task_id !== currentTaskId) return;

    const signal = view.pending_question_set
      ? `${currentTaskId}:question:${view.pending_question_set.id}`
      : view.plan.state === "ready" && view.items.length > 0
        ? `${currentTaskId}:ready:${view.plan.revision}`
        : null;
    if (!signal || autoOpenedPlanSignals.current.has(signal)) return;

    autoOpenedPlanSignals.current.add(signal);
    setCanvasTab("plan");
  }, [currentTaskId, planController.view, setCanvasTab]);

  useEffect(() => {
    const workbenchVisible = workbenchMode === "docked" || workbenchMode === "focus";
    // 工具切换只改变激活 Tab；仅在整个工作台离开可见区域时结束子代理视图态。
    if (!subagentPanelOpen || workbenchVisible) return;
    closeSubagentView();
  }, [closeSubagentView, subagentPanelOpen, workbenchMode]);

  useEffect(() => {
    if (workbenchMode === "focus") convoRef.current?.setAttribute("inert", "");
    else convoRef.current?.removeAttribute("inert");
  }, [workbenchMode]);

  usePoll(
    () => (currentTaskId ? refreshDetail(currentTaskId) : undefined),
    2000,
    currentTaskId != null,
  );

  useEffect(() => {
    dispatchActivity({
      type: "snapshot",
      running,
      runs: detail?.runs ?? [],
      queuedMessages,
      pendingPermissions,
      at: Date.now(),
    });
  }, [running, runsStamp, queueStamp, permissionStamp]);

  const observeAgentEvent = useCallback((event: AgentEvent) => {
    dispatchActivity({ type: "event", event, at: Date.now() });
  }, []);

  const observeSend = useCallback((mode: AgentSendMode) => {
    dispatchActivity({ type: "sent", mode, at: Date.now() });
  }, []);

  const updateRoomSplit = useCallback((raw: number, persist = false) => {
    const width = roomRef.current?.getBoundingClientRect().width ?? roomWidth;
    const next = clampRoomSplit(raw, width);
    roomSplitRef.current = next;
    setRoomSplitPct(next);
    if (persist) {
      try {
        window.localStorage.setItem(ROOM_SPLIT_STORAGE_KEY, String(next));
      } catch {
        // 私密模式或受限环境下无法持久化时仍保持本次会话可用。
      }
    }
  }, [roomWidth]);

  useEffect(() => {
    const room = roomRef.current;
    if (!room) return;

    const syncRoomWidth = () => {
      const width = room.getBoundingClientRect().width;
      setRoomWidth(width);
      const next = clampRoomSplit(roomSplitRef.current, width);
      // 尺寸收窄时仅临时钳制显示值；恢复宽度后仍回到用户保存的偏好比例。
      setRoomSplitPct((current) => current === next ? current : next);
    };

    const observer = new ResizeObserver(syncRoomWidth);
    observer.observe(room);
    syncRoomWidth();
    return () => observer.disconnect();
  }, [currentTaskId]);

  const updateRoomSplitFromPointer = useCallback((clientX: number, persist = false) => {
    const rect = roomRef.current?.getBoundingClientRect();
    if (!rect || rect.width <= ROOM_SPLITTER_WIDTH) return;
    updateRoomSplit(((clientX - rect.left - ROOM_SPLITTER_WIDTH / 2) / rect.width) * 100, persist);
  }, [updateRoomSplit]);

  const beginSplitDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    event.preventDefault();
    splitDraggingRef.current = true;
    setIsSplitDragging(true);
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const moveSplitDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!splitDraggingRef.current) return;
    updateRoomSplitFromPointer(event.clientX);
  };

  const endSplitDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!splitDraggingRef.current) return;
    splitDraggingRef.current = false;
    setIsSplitDragging(false);
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    updateRoomSplit(roomSplitRef.current, true);
  };

  const onSplitKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    const width = roomRef.current?.getBoundingClientRect().width ?? roomWidth;
    const bounds = getRoomSplitBounds(width);
    const step = event.shiftKey ? 8 : 2;
    let next: number | null = null;

    if (event.key === "ArrowLeft") next = roomSplitRef.current - step;
    if (event.key === "ArrowRight") next = roomSplitRef.current + step;
    if (event.key === "Home") next = bounds.min;
    if (event.key === "End") next = bounds.max;
    if (next == null) return;

    event.preventDefault();
    updateRoomSplit(next, true);
  };

  const abortRun = useCallback(async () => {
    if (!currentTaskId) return;
    await agentAbort(currentTaskId);
    await refreshDetail(currentTaskId);
  }, [currentTaskId, refreshDetail]);

  const abortSubagent = useCallback(async (subagentId: string) => {
    if (!currentTaskId) return;
    await agentAbortSubagent(currentTaskId, subagentId);
    await refreshDetail(currentTaskId);
  }, [currentTaskId, refreshDetail]);

  if (!currentTaskId) {
    return (
      <div className="scene">
        <div className="empty room-empty">
          <p>没有打开的会话。</p>
          <button className="btn accent" onClick={goHome}><IconHome width={12} height={12} /> 回到新对话</button>
        </div>
      </div>
    );
  }

  // 先用列表快照立即画出任务壳，detail IPC 返回后再补齐运行信息，避免点击后整页空等。
  const task = taskSnapshot;
  if (!task) {
    return (
      <section className="scene scene-room workbench-hidden" data-testid="workbench-root" data-workbench-mode="hidden">
        <div className="convo room-loading" role="status">正在打开对话…</div>
      </section>
    );
  }
  const archived = task.state === "archived";
  const workspacePath = task?.workspace_path ?? null;
  const workspace = workspaces.find((item) => item.canonical_path === workspacePath);
  const workspaceAttached = Boolean(workspacePath);
  const workspaceAccessMode = workspace?.access_mode ?? "request_approval";
  const splitBounds = getRoomSplitBounds(roomWidth);
  const viewportWidth = typeof window === "undefined" ? 1600 : window.innerWidth;
  const workbenchLayout = workbenchMode === "focus"
    ? "focus"
    : viewportWidth <= 759
      ? "full"
      : viewportWidth <= 1359
        ? "overlay"
        : viewportWidth < 1600
          ? "compact"
          : "wide";
  const dismissWorkbench = () => {
    closeSubagentView();
    hideWorkbench(task?.state === "review_ready" && (canvasTab === "review" || canvasTab === "changes"));
  };

  const toggleWorkbenchFromRoom = () => {
    if (workbenchMode === "hidden") restoreWorkbench();
    else if (workbenchMode === "collapsed") expandReview();
    else dismissWorkbench();
  };

  const attachFolder = async () => {
    if (scopeBusy || running) return;
    setScopeBusy(true);
    setScopeError(null);
    try {
      const attached = await taskChooseWorkspace(currentTaskId);
      if (!attached?.workspace_path) return;
      await refreshWorkspaces();
      setCurrentProject(attached.workspace_path);
      await refreshDetail(currentTaskId);
    } catch (cause) {
      setScopeError(String(cause));
    } finally {
      setScopeBusy(false);
    }
  };

  const setWorkspaceAccessMode = async (accessMode: ProjectAccessMode) => {
    if (!workspacePath || scopeBusy) return;
    setScopeBusy(true);
    setScopeError(null);
    try {
      await workspaceSetAccessMode(workspacePath, accessMode);
      // 运行中仅保存项目级设置，当前 run 继续使用启动快照。
      // 空闲时用同路径幂等刷新丢弃缓存 session，下一轮重建权限作用域。
      if (!running) await taskSetWorkspace(currentTaskId, workspacePath);
      await refreshWorkspaces();
      await refreshDetail(currentTaskId);
    } catch (cause) {
      setScopeError(String(cause));
    } finally {
      setScopeBusy(false);
    }
  };

  return (
    <section
      ref={roomRef}
      className={`scene scene-room workbench-${workbenchMode}${isSplitDragging ? " is-split-dragging" : ""}`}
      style={{ "--room-convo-width": `${roomSplitPct}%` } as CSSProperties}
      data-testid="workbench-root"
      data-workbench-mode={workbenchMode}
      data-workbench-layout={workbenchLayout}
      data-owner-key={`task:${currentTaskId}`}
    >
      <div
        ref={convoRef}
        className="convo pane pane-lit"
        aria-hidden={workbenchMode === "focus" ? true : undefined}
      >
        <header className="room-conversation-head">
          <IconProjects width={16} height={16} />
          <div className="room-conversation-title">
            <strong>{task?.title || "任务会话"}</strong>
            <span>{workspace?.display_name ?? "未附加项目"} · {historyBranchId ? "历史分支，只读" : archived ? "已归档，只读" : running ? "正在运行" : "会话就绪"}</span>
          </div>
          {historicalBranches.length > 0 && (
            <label className="room-history-picker">
              <span>对话记录</span>
              <select
                aria-label="选择对话历史分支"
                value={historyBranchId ?? ""}
                onChange={(event) => setHistoryBranchId(event.target.value || null)}
              >
                <option value="">当前对话</option>
                {historicalBranches.map((branch, index) => (
                  <option key={branch.id} value={branch.id}>
                    {historicalBranchLabel(branch, index)}
                  </option>
                ))}
              </select>
            </label>
          )}
          {task && <TaskActionsMenu task={task} detail={detail} className="room-task-actions" />}
          <button
            type="button"
            className="room-workbench-toggle"
            onClick={toggleWorkbenchFromRoom}
            aria-label={workbenchMode === "hidden" || workbenchMode === "collapsed" ? "展开任务工作台" : "隐藏任务工作台"}
            aria-expanded={workbenchMode !== "hidden" && workbenchMode !== "collapsed"}
          >
            <IconSidebar width={17} height={17} />
          </button>
        </header>
        <div className={`room-scopebar ${workspacePath ? "has-workspace" : "needs-workspace"}`}>
          {workspacePath ? (
            <>
              <IconProjects width={13} height={13} />
              <span title={workspacePath}>{workspace?.display_name ?? "已附加文件夹"}</span>
              <span className="room-scope-state scoped">
                {projectAccessModeLabel(workspaceAccessMode)}
              </span>
              <span className="room-scope-state agent-engine-state">
                主 Agent · {task?.agent_engine === "codex" ? "Codex CLI" : "R-Code"}
              </span>
              {detail?.active_branch.id && detail.active_branch.id !== "main" && (
                <span className="room-scope-state" title={`从 ${detail?.active_branch.parent_branch_id ?? "主分支"} 分叉`}>
                  编辑分支
                </span>
              )}
            </>
          ) : (
            <>
              <span>此对话未附加文件夹</span>
              {detail?.active_branch.id && detail.active_branch.id !== "main" && (
                <span className="room-scope-state">编辑分支</span>
              )}
              {!archived && (
                <button
                  className="quiet-link"
                  disabled={scopeBusy || running}
                  title={running ? "当前运行结束后才能附加文件夹" : "为此对话一次性附加工作区"}
                  onClick={() => void attachFolder()}
                >
                  <IconAttach width={13} height={13} /> {scopeBusy ? "正在选择…" : running ? "运行结束后可附加" : "附加文件夹"}
                </button>
              )}
            </>
          )}
          <PlanShortcut
            taskMode={task.mode}
            controller={planController}
            onOpen={() => setCanvasTab("plan")}
          />
        </div>
        {scopeError && (
          <StatusBar kind="error" compact onDismiss={() => setScopeError(null)}>
            工作区操作失败：{scopeError}
          </StatusBar>
        )}
        {historyBranchId && (
          <div className="room-history-banner" role="status">
            <div>
              <strong>历史分支 · 只读</strong>
              <span>这里只浏览清空前的记录；当前对话、运行状态和排队消息均未切换。</span>
            </div>
            <button type="button" className="quiet-link" onClick={() => setHistoryBranchId(null)}>
              返回当前对话
            </button>
          </div>
        )}
        <Timeline
          ref={tlRef}
          taskId={currentTaskId}
          branchId={historyBranchId}
          workspacePath={workspacePath}
          cur={null}
          running={running}
          reviewing={running && activity.phase === "reviewing"}
          onAgentEvent={observeAgentEvent}
          selectedSubagentId={selectedSubagentId}
          onInspectSubagent={inspectSubagent}
        />
        {historyBranchId ? null : archived ? (
          <div className="room-archived-note">此对话已归档，只能查看历史。可在项目概览中还原，或通过右上角对话选项永久删除。</div>
        ) : (
          <>
            <PendingPermissions taskId={currentTaskId} />
            <Composer
              key={currentTaskId}
              taskId={currentTaskId}
              workspacePath={workspacePath}
              workspaceAttached={workspaceAttached}
              workspaceName={workspace?.display_name ?? null}
              workspaceAccessMode={workspaceAccessMode}
              onAccessModeChange={setWorkspaceAccessMode}
              scopeBusy={scopeBusy}
              providerName={task.provider_name ?? null}
              agentEngine={task.agent_engine}
              model={task.model ?? null}
              inference={task.inference ?? {}}
              providerChoices={providers.choices}
              providerFallback={providers.fallback}
              onProviderChanged={() => void refreshDetail(currentTaskId)}
              running={running}
              queuedMessages={queuedMessages}
              onAbort={abortRun}
              onSent={(text, mode, attachments) => tlRef.current?.onSent(text, mode, attachments)}
              onSendFailed={() => tlRef.current?.reload()}
              onActivitySent={observeSend}
              onShowSubagents={showSubagentList}
            />
          </>
        )}
      </div>
      {workbenchMode === "docked" && (
        <div
          className="room-splitter"
          role="separator"
          tabIndex={0}
          aria-label="调整对话与工作台宽度"
          aria-orientation="vertical"
          aria-valuemin={Math.round(splitBounds.min)}
          aria-valuemax={Math.round(splitBounds.max)}
          aria-valuenow={Math.round(roomSplitPct)}
          aria-valuetext={`对话 ${Math.round(roomSplitPct)}%，工作台 ${Math.round(100 - roomSplitPct)}%`}
          onDoubleClick={() => updateRoomSplit(DEFAULT_ROOM_SPLIT_PCT, true)}
          onKeyDown={onSplitKeyDown}
          onPointerDown={beginSplitDrag}
          onPointerMove={moveSplitDrag}
          onPointerUp={endSplitDrag}
          onPointerCancel={endSplitDrag}
        >
          <span aria-hidden="true" />
        </div>
      )}
      {workbenchMode === "docked" && (
        <button type="button" className="workbench-backdrop" onClick={dismissWorkbench} aria-label="关闭任务工作台" />
      )}
      <Canvas
        taskId={currentTaskId}
        task={task}
        planController={planController}
        running={running}
        activity={activity}
        workspacePath={workspacePath}
        workspaceAttached={workspaceAttached}
        subagentPanelOpen={subagentPanelOpen}
        selectedSubagentId={selectedSubagentId}
        openSubagentIds={subagentView.openIds}
        onInspectSubagent={inspectSubagent}
        onBackToSubagents={backToSubagentList}
        onCloseSubagentTab={closeSubagentTab}
        onCloseSubagents={closeSubagentView}
        onAbortSubagent={abortSubagent}
        onTaskChanged={() => refreshDetail(currentTaskId)}
      />
    </section>
  );
}
