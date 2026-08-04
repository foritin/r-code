import { useEffect, useRef, useState } from "react";
import {
  planCreate,
  planGet,
  taskSetMode,
  taskUpdateGoal,
} from "../lib/ipc";
import type { Task, TaskAgentEngine, TaskMode } from "../lib/types";
import {
  ATTACHMENT_PICKER_ACCEPT,
} from "./Attachments";
import {
  IconArrowRight,
  IconFile,
  IconHelp,
  IconPlus,
  IconProjects,
  IconText,
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
  onDraftGoalChange?: (goal: string) => void;
  onDraftModeChange?: (mode: TaskMode) => void;
  onTaskChanged?: () => Promise<void> | void;
  onError?: (message: string) => void;
}

type MenuView = "add" | "goal";

function readableError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * Composer-local Add menu. It is a portal-backed dialog, so the Goal editor and
 * folder picker cannot be clipped by the composer or the narrow conversation pane.
 */
export function TaskAddMenu({
  onFiles,
  disabled = false,
  running = false,
  task,
  agentEngine,
  draftGoal = "",
  draftMode = "ask",
  onDraftGoalChange,
  onDraftModeChange,
  onTaskChanged,
  onError,
}: Props) {
  const [view, setView] = useState<MenuView>("add");
  const [goalDraft, setGoalDraft] = useState(task?.goal ?? draftGoal);
  const [busy, setBusy] = useState<"goal" | "plan" | null>(null);
  const [saved, setSaved] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const folderInputRef = useRef<HTMLInputElement>(null);
  const goalInputRef = useRef<HTMLTextAreaElement>(null);
  const currentGoal = task?.goal ?? draftGoal;
  const currentMode = task?.mode ?? draftMode;
  const planAvailable = agentEngine === "r_code";

  useEffect(() => {
    if (view !== "goal") setGoalDraft(currentGoal);
  }, [currentGoal, view]);

  useEffect(() => {
    if (view !== "goal") return;
    const id = window.requestAnimationFrame(() => {
      goalInputRef.current?.focus();
      goalInputRef.current?.setSelectionRange(goalDraft.length, goalDraft.length);
    });
    return () => window.cancelAnimationFrame(id);
  }, [goalDraft.length, view]);

  useEffect(() => {
    folderInputRef.current?.setAttribute("webkitdirectory", "");
  }, []);

  const saveGoal = async (value: string) => {
    if (busy) return;
    setBusy("goal");
    setSaved(false);
    try {
      const normalized = value.trim();
      if (task) {
        await taskUpdateGoal(task.id, normalized);
        await onTaskChanged?.();
      } else {
        onDraftGoalChange?.(normalized);
      }
      setGoalDraft(normalized);
      setSaved(true);
    } catch (error) {
      onError?.(`保存目标失败：${readableError(error)}`);
    } finally {
      setBusy(null);
    }
  };

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
      onOpenChange={(open) => {
        if (!open) {
          setView("add");
          setSaved(false);
        }
      }}
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
      {({ close }) => view === "goal" ? (
        <div className="task-goal-editor">
          <button className="task-add-back" type="button" onClick={() => setView("add")}>
            <IconArrowRight width={14} height={14} aria-hidden="true" />
            返回添加
          </button>
          <label htmlFor="task-goal-draft">目标</label>
          <p>用于约束后续规划与执行；它不会替代本轮消息。</p>
          <textarea
            id="task-goal-draft"
            ref={goalInputRef}
            rows={3}
            value={goalDraft}
            placeholder="例如：在不削减现有功能的前提下完成可验证的交付"
            onChange={(event) => {
              setGoalDraft(event.target.value);
              setSaved(false);
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
                event.preventDefault();
                void saveGoal(goalDraft);
              }
            }}
          />
          <div className="task-goal-actions">
            <button
              className="quiet-link"
              type="button"
              disabled={busy === "goal" || (!currentGoal && !goalDraft)}
              onClick={() => void saveGoal("")}
            >
              清空目标
            </button>
            {saved && <span role="status">已保存</span>}
            <button
              className="btn accent"
              type="button"
              disabled={busy === "goal" || goalDraft.trim() === currentGoal.trim()}
              onClick={() => void saveGoal(goalDraft)}
            >
              {busy === "goal" ? "保存中…" : "保存目标"}
            </button>
          </div>
        </div>
      ) : (
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
          <button className="task-add-item" type="button" onClick={() => setView("goal")}>
            <IconText width={17} height={17} />
            <span>
              <strong>目标 {currentGoal && <em>已设置</em>}</strong>
              <small>{currentGoal || "设置要持续追求的结果"}</small>
            </span>
            <IconArrowRight width={14} height={14} />
          </button>
          <button
            className={`task-add-item${currentMode === "plan" ? " is-active" : ""}`}
            type="button"
            disabled={!planAvailable || running || busy === "plan"}
            title={!planAvailable
              ? "计划模式仅支持 R-Code 主 Agent"
              : running
                ? "请等待当前运行结束或先中断，再切换计划模式"
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
