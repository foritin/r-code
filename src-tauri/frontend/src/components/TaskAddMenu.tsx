import { useEffect, useRef, useState } from "react";
import {
  planCreate,
  planGet,
  taskSetMode,
} from "../lib/ipc";
import type { Task, TaskAgentEngine, TaskMode } from "../lib/types";
import {
  ATTACHMENT_PICKER_ACCEPT,
} from "./Attachments";
import {
  IconClose,
  IconEdit,
  IconFile,
  IconGoal,
  IconHelp,
  IconPause,
  IconPlay,
  IconPlus,
  IconProjects,
  IconTrash,
} from "./icons";
import { Menu } from "./ui/Menu";

interface Props {
  onFiles: (files: readonly File[]) => Promise<void> | void;
  disabled?: boolean;
  /** Mode transitions are forbidden while the current main run is active. */
  running?: boolean;
  task?: Task | null;
  agentEngine: TaskAgentEngine;
  draftGoal?: string;
  draftMode?: TaskMode;
  goalMode: boolean;
  onGoalModeChange: (active: boolean) => void;
  onDraftModeChange?: (mode: TaskMode) => void;
  onTaskChanged?: () => Promise<void> | void;
  onError?: (message: string) => void;
}

function readableError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

interface GoalModeChipProps {
  disabled?: boolean;
  onExit: () => void;
}

/** Active composer mode. Hover/focus swaps the target for the removable × affordance. */
export function GoalModeChip({ disabled = false, onExit }: GoalModeChipProps) {
  return (
    <span className="goal-mode-control">
      <i className="goal-mode-divider" aria-hidden="true" />
      <button
        className="goal-mode-chip"
        type="button"
        disabled={disabled}
        aria-label="退出目标模式"
        title="退出目标模式"
        onClick={onExit}
      >
        <span className="goal-mode-target" aria-hidden="true"><IconGoal width={17} height={17} /></span>
        <span className="goal-mode-close" aria-hidden="true"><IconClose width={11} height={11} /></span>
        <span>目标</span>
      </button>
    </span>
  );
}

interface ActiveGoalBarProps {
  goal: string;
  running: boolean;
  stopped?: boolean;
  busy?: boolean;
  onEdit: () => void;
  onStop: () => void;
  onResume: () => void;
  onDelete: () => void;
}

/** Durable Goal status and lifecycle controls. The composer remains the only input surface. */
export function ActiveGoalBar({
  goal,
  running,
  stopped = false,
  busy = false,
  onEdit,
  onStop,
  onResume,
  onDelete,
}: ActiveGoalBarProps) {
  return (
    <section className={`active-goal-bar${running ? " is-running" : stopped ? " is-stopped" : " is-idle"}`} aria-label="当前目标">
      <IconGoal className="active-goal-icon" width={17} height={17} />
      <div className="active-goal-copy">
        <strong>{running ? "进行中的目标" : stopped ? "已停止的目标" : "当前目标"}</strong>
        <span title={goal}>{goal}</span>
      </div>
      <div className="active-goal-actions">
        <button type="button" disabled={busy} onClick={onEdit} aria-label="编辑目标" title="编辑目标">
          <IconEdit width={16} height={16} />
        </button>
        {running ? (
          <button type="button" disabled={busy} onClick={onStop} aria-label="停止目标" title="停止目标">
            <IconPause width={16} height={16} />
          </button>
        ) : (
          <button type="button" disabled={busy} onClick={onResume} aria-label="继续目标" title="继续执行目标">
            <IconPlay width={16} height={16} />
          </button>
        )}
        <button
          className="is-destructive"
          type="button"
          disabled={busy}
          onClick={onDelete}
          aria-label="删除目标"
          title="删除目标"
        >
          <IconTrash width={16} height={16} />
        </button>
      </div>
    </section>
  );
}

/**
 * Composer-local Add menu. Goal is a mode of the main composer instead of a
 * second text field, while file/folder pickers remain anchored to this menu.
 */
export function TaskAddMenu({
  onFiles,
  disabled = false,
  running = false,
  task,
  agentEngine,
  draftGoal = "",
  draftMode = "ask",
  goalMode,
  onGoalModeChange,
  onDraftModeChange,
  onTaskChanged,
  onError,
}: Props) {
  const [busy, setBusy] = useState<"plan" | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const folderInputRef = useRef<HTMLInputElement>(null);
  const currentGoal = task?.goal_active ? task.goal : draftGoal;
  const currentMode = task?.mode ?? draftMode;
  const planAvailable = agentEngine === "r_code";

  useEffect(() => {
    folderInputRef.current?.setAttribute("webkitdirectory", "");
  }, []);

  const enablePlan = async (close: () => void) => {
    if (busy) return;
    if (!planAvailable) {
      onError?.("计划模式当前仅支持 R-Code 主 Agent；请先切换主 Agent。");
      return;
    }
    setBusy("plan");
    try {
      if (task) {
        await taskSetMode(task.id, "plan");
        const existing = await planGet(task.id);
        if (!existing || ["completed", "cancelled"].includes(existing.plan.state)) {
          try {
            await planCreate(task.id);
          } catch (error) {
            // A concurrent panel refresh may have initialized the same Plan. Only
            // surface the original error if no new active aggregate appeared.
            const current = await planGet(task.id);
            if (!current || ["completed", "cancelled"].includes(current.plan.state)) throw error;
          }
        }
        await onTaskChanged?.();
      } else {
        onDraftModeChange?.("plan");
      }
      close();
    } catch (error) {
      onError?.(`开启计划模式失败：${readableError(error)}`);
    } finally {
      setBusy(null);
    }
  };

  const pickFiles = (input: HTMLInputElement, close: () => void) => {
    const files = Array.from(input.files ?? []);
    input.value = "";
    if (files.length > 0) void onFiles(files);
    close();
  };

  return (
    <Menu
      role="dialog"
      label="添加到任务"
      placement="up"
      align="left"
      menuClassName="task-add-popover"
      disabled={disabled}
      trigger={
        <button
          className="composer-add-trigger"
          type="button"
          aria-label="添加到任务"
          title="添加文件、目标或计划模式"
        >
          <IconPlus width={16} height={16} />
        </button>
      }
    >
      {({ close }) => (
        <div className="task-add-list">
          <div className="task-add-title">添加</div>
          <button className="task-add-item" type="button" onClick={() => fileInputRef.current?.click()}>
            <IconFile width={17} height={17} />
            <span><strong>文件</strong><small>图片、文本、代码或 PDF</small></span>
          </button>
          <button className="task-add-item" type="button" onClick={() => folderInputRef.current?.click()}>
            <IconProjects width={17} height={17} />
            <span><strong>文件夹</strong><small>附加其中可读取的文件，最多 8 个</small></span>
          </button>
          <button
            className={`task-add-item${goalMode ? " is-active" : ""}`}
            type="button"
            aria-pressed={goalMode}
            onClick={() => {
              onGoalModeChange(true);
              close();
            }}
          >
            <IconGoal width={17} height={17} />
            <span>
              <strong>目标 {goalMode ? <em>编辑中</em> : currentGoal && <em>{running ? "进行中" : "已设置"}</em>}</strong>
              <small>{goalMode ? "在主输入框中编写，发送后立即执行" : currentGoal || "在主输入框中编写并立即执行"}</small>
            </span>
          </button>
          <button
            className={`task-add-item${currentMode === "plan" ? " is-active" : ""}`}
            type="button"
            disabled={!planAvailable || running || busy === "plan"}
            title={!planAvailable
              ? "计划模式仅支持 R-Code 主 Agent"
              : running
                ? "请等待当前运行结束或先停止，再切换计划模式"
                : "先通过交互明确边界，再产出可确认的功能计划"}
            onClick={() => void enablePlan(close)}
          >
            <IconHelp width={17} height={17} />
            <span>
              <strong>计划模式 {currentMode === "plan" && <em>已开启</em>}</strong>
              <small>{!planAvailable
                ? "请先切换到 R-Code 主 Agent"
                : running
                  ? "当前运行结束后可切换"
                  : "先规划，再按功能事项实施"}</small>
            </span>
          </button>
          <input
            ref={fileInputRef}
            className="sr-only"
            type="file"
            accept={ATTACHMENT_PICKER_ACCEPT}
            multiple
            tabIndex={-1}
            aria-hidden="true"
            onChange={(event) => pickFiles(event.currentTarget, close)}
          />
          <input
            ref={folderInputRef}
            className="sr-only"
            type="file"
            accept={ATTACHMENT_PICKER_ACCEPT}
            multiple
            tabIndex={-1}
            aria-hidden="true"
            onChange={(event) => pickFiles(event.currentTarget, close)}
          />
        </div>
      )}
    </Menu>
  );
}
