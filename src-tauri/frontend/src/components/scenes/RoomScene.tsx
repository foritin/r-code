/**
 * 单任务对话：聊天不依赖工作区；工作区范围可以在对话过程中按需附加。
 */
import { useCallback, useEffect, useReducer, useRef, useState, type CSSProperties } from "react";
import { useAppStore } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { usePoll } from "../../lib/poll";
import {
  agentAbort,
  agentAbortSubagent,
  taskSetWorkspace,
  workspaceChoose,
  workspaceSetAccessMode,
} from "../../lib/ipc";
import { useProviders } from "../../lib/provider";
import { StatusBar } from "../ui/StatusBar";
import type { AgentEvent, AgentSendMode, ProjectAccessMode } from "../../lib/types";
import { Timeline, type TimelineHandle } from "../room/Timeline";
import { Composer } from "../room/Composer";
import { PendingPermissions } from "../room/Permissions";
import { Canvas } from "../room/Canvas";
import { ActivityStrip } from "../room/ActivityStrip";
import { SubagentPanel } from "../room/SubagentPanel";
import { activityTraceReducer, createActivityTraceState } from "../room/activity";
import { IconAttach, IconHome, IconProjects } from "../icons";
import { projectAccessModeLabel } from "../ProjectAccessSelector";

const ROOM_SPLIT_STORAGE_KEY = "r-code.room.split-pct";
const DEFAULT_ROOM_SPLIT_PCT = 55;
const ROOM_SPLITTER_WIDTH = 11;
const MIN_CONVERSATION_WIDTH = 360;
const MIN_CANVAS_WIDTH = 300;

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
  const detail = useTasksStore((s) => (currentTaskId ? s.details[currentTaskId] : undefined));
  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const refreshWorkspaces = useTasksStore((s) => s.refreshWorkspaces);
  const workspaces = useTasksStore((s) => s.workspaces);

  const [scopeBusy, setScopeBusy] = useState(false);
  const [scopeError, setScopeError] = useState<string | null>(null);
  const boundProvider = useTasksStore((s) =>
    currentTaskId ? s.details[currentTaskId]?.task.provider_name ?? null : null
  );
  const providers = useProviders([currentTaskId, boundProvider]);
  const [activity, dispatchActivity] = useReducer(activityTraceReducer, createActivityTraceState());
  const [selectedSubagentId, setSelectedSubagentId] = useState<string | null>(null);
  const [subagentPanelRequest, setSubagentPanelRequest] = useState(0);
  const tlRef = useRef<TimelineHandle>(null);
  const roomRef = useRef<HTMLElement>(null);
  const splitDraggingRef = useRef(false);
  const roomSplitRef = useRef(initialRoomSplit());
  const [roomWidth, setRoomWidth] = useState(0);
  const [roomSplitPct, setRoomSplitPct] = useState(() => roomSplitRef.current);
  const [isSplitDragging, setIsSplitDragging] = useState(false);
  const running = detail?.runs.some((run) => run.ended_at === null) ?? false;
  const queuedMessages = detail?.queued_messages ?? [];
  const pendingPermissions = detail?.permissions.filter((permission) => permission.decision === "pending") ?? [];
  const queueStamp = queuedMessages.map((message) => `${message.id}:${message.state}`).join("|");
  const permissionStamp = pendingPermissions.map((permission) => permission.id).join("|");
  const runsStamp = (detail?.runs ?? [])
    .map(
      (run) =>
        `${run.id}:${run.agent_kind}:${run.agent_label ?? ""}:${run.review_state}:${run.ended_at ?? ""}:${run.summary ?? ""}`
    )
    .join("|");

  useEffect(() => {
    dispatchActivity({ type: "reset" });
    setSelectedSubagentId(null);
  }, [currentTaskId]);

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

  const task = detail?.task;
  const workspacePath = task?.workspace_path ?? null;
  const workspace = workspaces.find((item) => item.canonical_path === workspacePath);
  const workspaceAttached = Boolean(workspacePath);
  const workspaceAccessMode = workspace?.access_mode ?? "request_approval";
  const splitBounds = getRoomSplitBounds(roomWidth);

  const attachFolder = async () => {
    if (scopeBusy) return;
    setScopeBusy(true);
    setScopeError(null);
    try {
      const selected = await workspaceChoose();
      if (!selected) return;
      await taskSetWorkspace(currentTaskId, selected.canonical_path);
      await refreshWorkspaces();
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
      // 空闲会话会更新 runtime 作用域；运行中禁用此控件，避免当前 run 的策略突变。
      await taskSetWorkspace(currentTaskId, workspacePath);
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
      className={"scene scene-room" + (isSplitDragging ? " is-split-dragging" : "")}
      style={{ "--room-convo-width": `${roomSplitPct}%` } as CSSProperties}
    >
      <div className="convo pane pane-lit">
        <div className="room-scopebar">
          {workspacePath ? (
            <>
              <IconProjects width={13} height={13} />
              <span title={workspacePath}>{workspace?.display_name ?? "已附加文件夹"}</span>
              <span className="room-scope-state scoped">
                {projectAccessModeLabel(workspaceAccessMode)}
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
              <button className="quiet-link" disabled={scopeBusy} onClick={() => void attachFolder()}>
                <IconAttach width={13} height={13} /> {scopeBusy ? "正在选择…" : "附加文件夹"}
              </button>
            </>
          )}
        </div>
        {scopeError && (
          <StatusBar kind="error" compact onDismiss={() => setScopeError(null)}>
            工作区操作失败：{scopeError}
          </StatusBar>
        )}
        <Timeline
          ref={tlRef}
          taskId={currentTaskId}
          cur={null}
          running={running}
          onAgentEvent={observeAgentEvent}
        />
        <ActivityStrip
          state={activity}
          running={running}
        />
        <SubagentPanel
          key={currentTaskId}
          state={activity}
          selectedSubagentId={selectedSubagentId}
          onInspectSubagent={setSelectedSubagentId}
          onAbortSubagent={abortSubagent}
          openRequest={subagentPanelRequest}
        />
        <PendingPermissions taskId={currentTaskId} />
        <Composer
          taskId={currentTaskId}
          workspacePath={workspacePath}
          workspaceAttached={workspaceAttached}
          workspaceName={workspace?.display_name ?? null}
          workspaceAccessMode={workspaceAccessMode}
          onAccessModeChange={setWorkspaceAccessMode}
          scopeBusy={scopeBusy}
          providerName={task?.provider_name ?? null}
          model={task?.model ?? null}
          providerChoices={providers.choices}
          providerFallback={providers.fallback}
          onProviderChanged={() => void refreshDetail(currentTaskId)}
          running={running}
          queuedMessages={queuedMessages}
          onAbort={abortRun}
          onSent={(text, mode) => tlRef.current?.onSent(text, mode)}
          onSendFailed={() => tlRef.current?.reload()}
          onActivitySent={observeSend}
          onShowSubagents={() => setSubagentPanelRequest((value) => value + 1)}
        />
      </div>
      <div
        className="room-splitter"
        role="separator"
        tabIndex={0}
        aria-label="调整对话与画布宽度"
        aria-orientation="vertical"
        aria-valuemin={Math.round(splitBounds.min)}
        aria-valuemax={Math.round(splitBounds.max)}
        aria-valuenow={Math.round(roomSplitPct)}
        aria-valuetext={`对话 ${Math.round(roomSplitPct)}%，画布 ${Math.round(100 - roomSplitPct)}%`}
        onDoubleClick={() => updateRoomSplit(DEFAULT_ROOM_SPLIT_PCT, true)}
        onKeyDown={onSplitKeyDown}
        onPointerDown={beginSplitDrag}
        onPointerMove={moveSplitDrag}
        onPointerUp={endSplitDrag}
        onPointerCancel={endSplitDrag}
      >
        <span aria-hidden="true" />
      </div>
      <Canvas
        taskId={currentTaskId}
        running={running}
        activity={activity}
        workspacePath={workspacePath}
        workspaceAttached={workspaceAttached}
        selectedSubagentId={selectedSubagentId}
        onCloseSubagent={() => setSelectedSubagentId(null)}
        onAbortSubagent={abortSubagent}
      />
    </section>
  );
}
