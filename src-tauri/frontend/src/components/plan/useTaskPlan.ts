import { useCallback, useEffect, useRef, useState, type Dispatch, type SetStateAction } from "react";
import { planGet } from "../../lib/ipc";
import { usePoll } from "../../lib/poll";
import type { PlanView, TaskMode } from "../../lib/types";

export interface TaskPlanController {
  view: PlanView | null;
  loaded: boolean;
  loadError: string | null;
  setView: Dispatch<SetStateAction<PlanView | null>>;
  refresh: () => Promise<PlanView | null>;
  clearLoadError: () => void;
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * 单一任务的 Plan 状态源。
 *
 * Room 始终持有这个控制器，因此工作台隐藏时仍能发现 Plan/HITL 状态；所有异步
 * 返回都再次核对 task_id，切换项目或会话后不会把上一任务的 Plan 写进当前 UI。
 */
export function useTaskPlan(taskId: string | null, taskMode: TaskMode | null): TaskPlanController {
  const [view, setView] = useState<PlanView | null>(null);
  const [loadedForTaskId, setLoadedForTaskId] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const activeTaskId = useRef(taskId);
  activeTaskId.current = taskId;
  // useEffect 会在提交后才清理旧状态；渲染时先按 owner 过滤，项目切换的首帧也绝不泄露旧 Plan。
  const scopedView = view?.plan.task_id === taskId ? view : null;

  const refresh = useCallback(async () => {
    const requestedTaskId = taskId;
    if (!requestedTaskId) {
      setView(null);
      setLoadedForTaskId(null);
      setLoadError(null);
      return null;
    }

    try {
      const next = await planGet(requestedTaskId);
      if (activeTaskId.current !== requestedTaskId) return next;
      if (next && next.plan.task_id !== requestedTaskId) {
        throw new Error("计划返回了不属于当前会话的数据");
      }
      setView(next);
      setLoadedForTaskId(requestedTaskId);
      setLoadError(null);
      return next;
    } catch (cause) {
      if (activeTaskId.current === requestedTaskId) {
        setLoadedForTaskId(requestedTaskId);
        setLoadError(`读取计划失败：${errorText(cause)}`);
      }
      throw cause;
    }
  }, [taskId]);

  usePoll(
    async () => { await refresh(); },
    1800,
    taskId != null && (taskMode === "plan" || scopedView != null),
    "计划状态",
  );

  useEffect(() => {
    setView(null);
    setLoadedForTaskId(null);
    setLoadError(null);
    // 已批准的任务会回到 auto 模式，因此进入 Room 时仍需探测一次持久化 Plan。
    void refresh().catch(() => undefined);
  }, [taskId, refresh]);

  return {
    view: scopedView,
    loaded: taskId == null || loadedForTaskId === taskId,
    loadError,
    setView,
    refresh,
    clearLoadError: () => setLoadError(null),
  };
}
