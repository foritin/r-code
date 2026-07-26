/**
 * 全局 toast 通知：右下角容器 + 后台任务完成的自动播报。
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
import { useCallback, useEffect, useRef, useState } from "react";
import { useAppStore } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { useToastStore, pushToast } from "../../store/toast";
import type { Toast, ToastKind } from "../../store/toast";
import type { PermissionRequest, Task, TaskState } from "../../lib/types";
import { IconAlert, IconCheck, IconClose } from "../icons";

/** 退场动画时长，与 components.css 的 toastOut 保持一致。 */
const EXIT_MS = 150;

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

  return (
    <div
      className="toast-host"
      role="status"
      aria-live="polite"
      aria-atomic="false"
      aria-relevant="additions text"
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
    const timer = window.setTimeout(beginDismiss, remaining.current);
    return () => {
      window.clearTimeout(timer);
      remaining.current = Math.max(0, remaining.current - (Date.now() - startedAt));
    };
  }, [timeout, paused, leaving, toast.createdAt, beginDismiss]);

  const isError = toast.kind === "error";
  const action = toast.action;

  return (
    <div
      className={`toast toast--${toast.kind}` + (leaving ? " is-leaving" : "")}
      role={isError ? "alert" : undefined}
      aria-live={isError ? "assertive" : undefined}
      onMouseEnter={() => setPaused(true)}
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
      <button type="button" className="iconbtn toast-close" onClick={beginDismiss} aria-label="关闭通知">
        <IconClose width={12} height={12} />
      </button>
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
 * archived 也算终结，但它是用户自己点归档造成的，不需要再回声一次，
 * 所以只进这个集合用于"已结束"的判定，不产出 toast（见 completionToast）。
 */
const TERMINAL_STATES: ReadonlySet<TaskState> = new Set<TaskState>([
  "review_ready",
  "idle",
  "interrupted",
  "archived",
]);

/** 与 Rail / Canvas 一致的任务显示名。 */
function taskLabel(task: Task): string {
  return task.title.trim() || task.goal.trim() || "未命名会话";
}

/** 终结态 → 播报内容；返回 null 表示这个状态不值得打扰用户。 */
function completionToast(task: Task): { kind: ToastKind; title: string; body: string } | null {
  const label = taskLabel(task);
  switch (task.state) {
    case "review_ready":
      return { kind: "success", title: `待审阅：${label}`, body: "任务已跑完，改动等你确认。" };
    case "idle":
      return { kind: "info", title: `已结束：${label}`, body: "本轮结束，没有留下待审阅的改动。" };
    case "interrupted":
      return { kind: "error", title: `已中止：${label}`, body: "运行被打断，回到会话可以看最后一步。" };
    default:
      return null;
  }
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
    const openRoom = (taskId: string) => useAppStore.getState().openRoom(taskId);

    // 初始快照：启动时已经是终结态的任务不该炸出一堆 toast。
    const seenState = new Map<string, TaskState>();
    const notifiedPermissions = new Set<string>();
    const initial = useTasksStore.getState();
    for (const task of initial.tasks) seenState.set(task.id, task.state);
    for (const detail of Object.values(initial.details)) {
      for (const p of detail.permissions) {
        if (p.decision === "pending") notifiedPermissions.add(p.id);
      }
    }

    return useTasksStore.subscribe((state, prev) => {
      if (state.tasks !== prev.tasks) {
        for (const task of state.tasks) {
          const before = seenState.get(task.id);
          seenState.set(task.id, task.state);
          // before === undefined：首次见到这个任务，它的当前状态属于初始快照。
          if (before === undefined || before === task.state) continue;
          if (!ACTIVE_STATES.has(before) || !TERMINAL_STATES.has(task.state)) continue;
          const spec = completionToast(task);
          if (!spec) continue;
          pushToast({
            ...spec,
            action: { label: "打开会话", run: () => openRoom(task.id) },
          });
        }
        // 任务被归档/删除后会从列表消失，顺手清理记录，避免 Map 无限增长。
        if (seenState.size > state.tasks.length) {
          const live = new Set(state.tasks.map((t) => t.id));
          for (const id of seenState.keys()) {
            if (!live.has(id)) seenState.delete(id);
          }
        }
      }

      if (state.details !== prev.details) {
        const app = useAppStore.getState();
        const freshByTask = new Map<string, PermissionRequest[]>();
        const stillPending = new Set<string>();

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

        for (const [taskId, fresh] of freshByTask) {
          // 用户就在这个任务的 Room 里，权限卡片已经摆在眼前，再弹一条就是噪音。
          if (app.scene === "room" && app.currentTaskId === taskId) continue;
          const task = state.tasks.find((t) => t.id === taskId);
          if (!task) continue;
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
