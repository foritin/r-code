/**
 * 全局 toast 通知：右下角容器 + 需要用户注意的后台状态播报。
 *
 * 视觉与 StatusBar 同源（同一套 tint/edge 语义色、同一套图标），区别只在于
 * StatusBar 是内联的、贴在它所解释的那块 UI 旁边；toast 是全局的、用来把
 * 用户不在场时发生的事情捞回来。二者不互相替代。
 *
 * ---------------------------------------------------------------------------
 * 桌面系统通知（暂未接入）
 * src-tauri/Cargo.toml 目前没有 tauri-plugin-notification（依赖只有 updater 和
 * dialog），所以这里只做应用内 toast。后续要接系统通知时的接入点就是本文件的
 * notifyTask()：加上插件后在那里补一次
 *   import { isPermissionGranted, requestPermission, sendNotification }
 *     from "@tauri-apps/plugin-notification";
 *   import { getCurrentWindow } from "@tauri-apps/api/window";
 * 并且只在 `await getCurrentWindow().isFocused() === false` 时才发——窗口就在
 * 眼前还弹系统横幅是纯噪音。应用内 toast 无论如何都要留着（窗口聚焦时它才是
 * 唯一的提示）。
 * ---------------------------------------------------------------------------
 */
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useAppStore } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { useToastStore, pushToast } from "../../store/toast";
import type { Toast, ToastKind } from "../../store/toast";
import type { PermissionRequest, Task, TaskDetail, TaskState } from "../../lib/types";
import { isPartialSuccess } from "../../lib/presentation";
import { IconAlert, IconCheck, IconClose } from "../icons";

/** 退场动画时长，与 components.css 的 toastOut 保持一致。 */
const EXIT_MS = 150;
/** 可见倒计时刷新频率；用绝对时间计算，避免后台降频后越走越慢。 */
const COUNTDOWN_TICK_MS = 250;

function prefersReducedMotion(): boolean {
  return (
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

// ---------------------------------------------------------------- 容器

/**
 * 挂在 App 根部的 toast 容器。
 * 即使队列为空也保持挂载：aria-live 区域必须先于内容存在于 DOM 中，
 * 否则屏幕阅读器读不到后插入的那条。
 */
export function ToastHost() {
  const toasts = useToastStore((s) => s.toasts);
  const hostRef = useRef<HTMLDivElement>(null);
  const latestToastStamp = toasts[toasts.length - 1]?.createdAt ?? 0;

  useLayoutEffect(() => {
    const host = hostRef.current;
    if (!host || toasts.length === 0) return;
    // A bounded queue can scroll on short desktop windows. Keep the newest event visible;
    // keyboard users can focus the region and scroll back to persistent older failures.
    host.scrollTop = host.scrollHeight;
  }, [latestToastStamp, toasts.length]);

  return (
    <div
      ref={hostRef}
      className="toast-host"
      role="status"
      aria-label="通知"
      aria-live="polite"
      aria-atomic="false"
      aria-relevant="additions text"
      tabIndex={toasts.length > 0 ? 0 : -1}
    >
      {toasts.map((toast) => (
        <ToastCard key={toast.id} toast={toast} />
      ))}
    </div>
  );
}

function ToastCard({ toast }: { toast: Toast }) {
  const dismiss = useToastStore((s) => s.dismiss);
  const [paused, setPaused] = useState(false);
  const [leaving, setLeaving] = useState(false);
  const leaveTimer = useRef<number | null>(null);
  const timeout = toast.timeout ?? 0;
  /** 剩余可见时长；hover 暂停后从这里续，不是每次都从头开始 */
  const remaining = useRef(timeout);
  const [remainingMs, setRemainingMs] = useState(timeout);

  const beginDismiss = useCallback(() => {
    if (leaveTimer.current != null) return;
    setLeaving(true);
    leaveTimer.current = window.setTimeout(
      () => {
        leaveTimer.current = null;
        dismiss(toast.id);
      },
      prefersReducedMotion() ? 0 : EXIT_MS
    );
  }, [dismiss, toast.id]);

  useEffect(
    () => () => {
      if (leaveTimer.current != null) window.clearTimeout(leaveTimer.current);
    },
    []
  );

  // 去重刷新（createdAt 变化）：重置剩余时长，并撤销可能已经启动的退场。
  // 必须声明在计时 effect 之前——同一次提交里 effect 按声明序执行，
  // 计时 effect 的清理会先扣掉已用时间，这里再覆盖回满值。
  useEffect(() => {
    remaining.current = timeout;
    setRemainingMs(timeout);
    if (leaveTimer.current != null) {
      window.clearTimeout(leaveTimer.current);
      leaveTimer.current = null;
    }
    setLeaving(false);
  }, [timeout, toast.createdAt]);

  // 自动消失。timeout <= 0（error）永不自动关；hover / 键盘聚焦时暂停。
  useEffect(() => {
    if (timeout <= 0 || paused || leaving) return;
    const startedAt = Date.now();
    const startingRemaining = remaining.current;
    const updateCountdown = () => {
      setRemainingMs(Math.max(0, startingRemaining - (Date.now() - startedAt)));
    };
    const countdownTimer = window.setInterval(updateCountdown, COUNTDOWN_TICK_MS);
    const timer = window.setTimeout(beginDismiss, startingRemaining);
    return () => {
      window.clearTimeout(timer);
      window.clearInterval(countdownTimer);
      remaining.current = Math.max(0, startingRemaining - (Date.now() - startedAt));
    };
  }, [timeout, paused, leaving, toast.createdAt, beginDismiss]);

  const isError = toast.kind === "error";
  const action = toast.action;
  const remainingSeconds = timeout > 0 ? Math.max(0, Math.ceil(remainingMs / 1000)) : null;

  return (
    <div
      className={`toast toast--${toast.kind}` + (leaving ? " is-leaving" : "")}
      role={isError ? "alert" : undefined}
      aria-live={isError ? "assertive" : undefined}
      // Pause only after deliberate pointer movement. A toast can materialize underneath a
      // stationary cursor (especially when the compact companion raises the stack); treating
      // that synthetic hover as intent would make an auto-dismiss notification permanent.
      onPointerMove={() => setPaused(true)}
      onMouseLeave={() => setPaused(false)}
      onFocus={() => setPaused(true)}
      onBlur={() => setPaused(false)}
    >
      <span className="toast-icon">
        {toast.kind === "success" ? (
          <IconCheck width={14} height={14} />
        ) : (
          <IconAlert width={14} height={14} />
        )}
      </span>
      <div className="toast-copy">
        <p className="toast-title">{toast.title}</p>
        {toast.body && <p className="toast-text">{toast.body}</p>}
        {action && (
          <button
            type="button"
            className="btn sm toast-action"
            onClick={() => {
              action.run();
              beginDismiss();
            }}
          >
            {action.label}
          </button>
        )}
      </div>
      <div className="toast-controls">
        {remainingSeconds != null && (
          <span
            className="toast-countdown"
            aria-hidden="true"
            title={paused ? "自动关闭倒计时已暂停" : `${remainingSeconds} 秒后自动关闭`}
          >
            {remainingSeconds}s
          </span>
        )}
        <button type="button" className="iconbtn toast-close" onClick={beginDismiss} aria-label="关闭通知">
          <IconClose width={12} height={12} />
        </button>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------- 任务播报

/**
 * 活跃态：agent 还在跑，这一轮没结束。
 * （TaskState 只有 6 个取值，见 lib/types.ts —— 没有 done/failed/aborted 之类。）
 */
const ACTIVE_STATES: ReadonlySet<TaskState> = new Set<TaskState>(["exploring", "in_progress"]);

/**
 * 终结态：本轮已经停下来了。
 * idle 是普通问答结束，archived 是用户主动归档；两者都只用于状态流转判定，
 * 不产出 toast（见 completionToast），避免每轮对话结束都打断用户。
 */
const TERMINAL_STATES: ReadonlySet<TaskState> = new Set<TaskState>([
  "review_ready",
  "idle",
  "interrupted",
  "archived",
]);

/** 中止可从会话中回看，不应像不可恢复错误一样永久占住界面。 */
const INTERRUPTED_TOAST_TIMEOUT_MS = 5000;

/** 与 Rail / Canvas 一致的任务显示名。 */
function taskLabel(task: Task): string {
  return task.title.trim() || task.goal.trim() || "未命名会话";
}

/** 终结态 → 播报内容；返回 null 表示这个状态不值得打扰用户。 */
export function completionToast(task: Task, detail?: TaskDetail): { kind: ToastKind; title: string; body: string; timeout?: number } | null {
  const label = taskLabel(task);
  switch (task.state) {
    case "review_ready":
      if (isPartialSuccess(detail)) {
        return {
          kind: "warn",
          title: `修改待审阅：${label}`,
          body: "修改存在但总结失败。工作区改动已保留，请先审阅。",
        };
      }
      return { kind: "success", title: `待审阅：${label}`, body: "任务已跑完，改动等你确认。" };
    case "idle":
      return null;
    case "interrupted":
      return {
        kind: "error",
        title: `已中止：${label}`,
        body: "运行被打断，回到会话可以看最后一步。",
        timeout: INTERRUPTED_TOAST_TIMEOUT_MS,
      };
    default:
      return null;
  }
}

export function completionDetailReady(task: Task, detail?: TaskDetail): boolean {
  if (
    !detail
    || detail.task.state !== task.state
    || detail.task.updated_at !== task.updated_at
  ) {
    return false;
  }
  // TaskDetail runs are newest-first. Checking the first main run prevents an older completed
  // turn from making a still-open current run look ready.
  const latestMainRun = detail.runs.find((run) => run.agent_kind === "main");
  return latestMainRun?.ended_at != null;
}

/**
 * 监听任务状态流转，在"活跃 → 终结"时播报，并在出现待批权限时提醒。
 *
 * 用 store.subscribe 而不是 useTasksStore(selector)：Deck / Home / Rail 每 2s
 * 轮询一次，details 每次都是新对象，用选择器订阅会让 App 连带整棵场景树每 2s
 * 重渲染一遍。这里只需要副作用，不需要渲染。
 */
export function useTaskCompletionToasts(): void {
  useEffect(() => {
    const openRoom = (taskId: string, tab?: "review") => useAppStore.getState().openRoom(taskId, tab);

    // 初始快照：启动时已经是终结态的任务不该炸出一堆 toast。
    const seenState = new Map<string, TaskState>();
    const notifiedPermissions = new Set<string>();
    const pendingCompletionDetails = new Set<string>();
    const initial = useTasksStore.getState();
    // task 列表和 detail 是异步分两批到达的；第一批 detail 只是初始基线，
    // 不能被误判成“刚刚出现”的权限请求。
    let permissionBaselineEstablished = Object.keys(initial.details).length > 0;
    for (const task of initial.tasks) seenState.set(task.id, task.state);
    for (const detail of Object.values(initial.details)) {
      for (const p of detail.permissions) {
        if (p.decision === "pending") notifiedPermissions.add(p.id);
      }
    }

    const notifyCompletion = (task: Task, detail?: TaskDetail) => {
      const spec = completionToast(task, detail);
      if (!spec) return;
      const app = useAppStore.getState();
      // The destination already presents the result and next actions. Showing a second
      // card that only opens the same room adds an unnecessary click (notably after
      // startup recovery navigates directly to an interrupted conversation).
      if (app.scene === "room" && app.currentTaskId === task.id) return;
      const partial = isPartialSuccess(detail);
      pushToast({
        ...spec,
        action: partial
          ? { label: "审阅改动", run: () => openRoom(task.id, "review") }
          : { label: "打开会话", run: () => openRoom(task.id) },
      });
    };

    return useTasksStore.subscribe((state, prev) => {
      if (state.tasks !== prev.tasks) {
        for (const task of state.tasks) {
          const before = seenState.get(task.id);
          seenState.set(task.id, task.state);
          // before === undefined：首次见到这个任务，它的当前状态属于初始快照。
          if (before === undefined || before === task.state) continue;
          if (!ACTIVE_STATES.has(before) || !TERMINAL_STATES.has(task.state)) continue;
          if (task.state === "review_ready") {
            const detail = state.details[task.id];
            // task 与 detail 分批刷新。等 run 的 ended_at/summary 到位后再决定是普通
            // 成功还是“有修改但总结失败”，避免先弹一条误导性的成功通知。
            if (!completionDetailReady(task, detail)) {
              pendingCompletionDetails.add(task.id);
              continue;
            }
            notifyCompletion(task, detail);
          } else {
            notifyCompletion(task, state.details[task.id]);
          }
        }
        // 任务被归档/删除后会从列表消失，顺手清理记录，避免 Map 无限增长。
        if (seenState.size > state.tasks.length) {
          const live = new Set(state.tasks.map((t) => t.id));
          for (const id of seenState.keys()) {
            if (!live.has(id)) {
              seenState.delete(id);
              pendingCompletionDetails.delete(id);
            }
          }
        }
      }

      if (state.details !== prev.details) {
        const app = useAppStore.getState();
        const freshByTask = new Map<string, PermissionRequest[]>();
        const stillPending = new Set<string>();

        for (const taskId of [...pendingCompletionDetails]) {
          const task = state.tasks.find((candidate) => candidate.id === taskId);
          const detail = state.details[taskId];
          if (!task || task.state !== "review_ready") {
            pendingCompletionDetails.delete(taskId);
            continue;
          }
          if (!completionDetailReady(task, detail)) continue;
          pendingCompletionDetails.delete(taskId);
          notifyCompletion(task, detail);
        }

        for (const task of state.tasks) {
          const detail = state.details[task.id];
          if (!detail) continue;
          const fresh: PermissionRequest[] = [];
          for (const p of detail.permissions) {
            if (p.decision !== "pending") continue;
            stillPending.add(p.id);
            if (!notifiedPermissions.has(p.id)) fresh.push(p);
          }
          if (fresh.length > 0) freshByTask.set(task.id, fresh);
        }

        // 已决策的请求从记录里退出；仍待批的（含这一轮新出现的）都算已知，不重复播报。
        notifiedPermissions.clear();
        for (const id of stillPending) notifiedPermissions.add(id);

        if (!permissionBaselineEstablished) {
          permissionBaselineEstablished = true;
          return;
        }

        for (const [taskId, fresh] of freshByTask) {
          const task = state.tasks.find((t) => t.id === taskId);
          if (!task) continue;
          // 当前页面已经能直接处理该请求时，不再叠一层全局弹窗。
          // Dashboard 只展示当前项目的待处理项；Inbox 则展示全部待处理项。
          const alreadyVisible =
            app.scene === "inbox" ||
            (app.scene === "room" && app.currentTaskId === taskId) ||
            (app.scene === "dashboard" && task.workspace_path === state.currentProjectId);
          if (alreadyVisible) continue;
          pushToast({
            kind: "warn",
            title: `需要授权：${taskLabel(task)}`,
            body:
              fresh.length > 1
                ? `${fresh.length} 个操作在等你批准（${fresh[0].tool_name} 等）。`
                : `${fresh[0].tool_name} 需要你批准才能继续。`,
            action: { label: "去处理", run: () => openRoom(taskId) },
          });
        }
      }
    });
  }, []);
}
