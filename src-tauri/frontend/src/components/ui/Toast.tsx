/**
 * 全局 toast 通知：右下角容器 + 需要用户注意的后台状态播报。
 *
 * 视觉与 StatusBar 同源（同一套 tint/edge 语义色、同一套图标），区别只在于
 * StatusBar 是内联的、贴在它所解释的那块 UI 旁边；toast 是全局的、用来把
 * 用户不在场时发生的事情捞回来。二者不互相替代。
 *
 * 后端 native_notification 模块负责前台/后台分流：前台事件在这里落成 toast，
 * 后台优先交给系统通知；权限拒绝、不可用或发送失败时仍会回落到这里。
 */
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import i18n, { getAppLocale, t as translate } from "../../i18n";
import {
  nativeNotificationSetLocale,
  notificationMarkRead,
  onNativeNotification,
  onNativeNotificationOpen,
} from "../../lib/ipc";
import { routeNativeNotificationOpen } from "../../lib/native-notification-routing";
import { useAppStore } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { useToastStore, pushToast } from "../../store/toast";
import type { Toast, ToastKind } from "../../store/toast";
import type {
  NativeNotificationEvent,
  NativeNotificationOpenPayload,
  PermissionRequest,
  Task,
  TaskDetail,
  TaskState,
} from "../../lib/types";
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

function appWindowIsForeground(): boolean {
  if (typeof document === "undefined") return true;
  return document.visibilityState === "visible" && document.hasFocus();
}

// ---------------------------------------------------------------- 容器

/**
 * 挂在 App 根部的 toast 容器。
 * 即使队列为空也保持挂载：aria-live 区域必须先于内容存在于 DOM 中，
 * 否则屏幕阅读器读不到后插入的那条。
 */
export function ToastHost() {
  const { t } = useTranslation();
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
      aria-label={t("notifications.ariaLabel")}
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
  const { t } = useTranslation();
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
            title={paused
              ? t("notifications.countdownPaused")
              : t("notifications.countdown", { count: remainingSeconds })}
          >
            {remainingSeconds}s
          </span>
        )}
        <button
          type="button"
          className="iconbtn toast-close"
          onClick={beginDismiss}
          aria-label={t("notifications.closeAria")}
        >
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
 * idle 通常是普通问答结束，archived 是用户主动归档；普通 idle 不产出 toast，
 * 但 latest main run 明确失败时是例外，避免错误静默消失。
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
  return task.title.trim() || task.goal.trim() || translate("notifications.unnamedTask");
}

/** 终结态 → 播报内容；返回 null 表示这个状态不值得打扰用户。 */
export function completionToast(task: Task, detail?: TaskDetail): { kind: ToastKind; title: string; body: string; timeout?: number } | null {
  const label = taskLabel(task);
  switch (task.state) {
    case "review_ready":
      if (isPartialSuccess(detail)) {
        return {
          kind: "warn",
          title: translate("notifications.reviewPartialTitle", { task: label }),
          body: translate("notifications.reviewPartialBody"),
        };
      }
      return {
        kind: "success",
        title: translate("notifications.reviewReadyTitle", { task: label }),
        body: translate("notifications.reviewReadyBody"),
      };
    case "idle": {
      const latestMainRun = detail?.runs.find((run) => run.agent_kind === "main");
      if (latestMainRun?.review_state !== "failed") return null;
      return {
        kind: "error",
        title: translate("notifications.runFailedTitle", { task: label }),
        body: translate("notifications.runFailedBody"),
      };
    }
    case "interrupted":
      return {
        kind: "error",
        title: translate("notifications.interruptedTitle", { task: label }),
        body: translate("notifications.interruptedBody"),
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
    const seenNativeSources = new Set<string>();
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

    const rememberNativeSource = (sourceKey: string) => {
      // Set 保留插入顺序；刷新已存在项到末尾，形成轻量 LRU，避免长时间运行后无界增长。
      seenNativeSources.delete(sourceKey);
      seenNativeSources.add(sourceKey);
      if (seenNativeSources.size > 512) {
        const oldest = seenNativeSources.values().next().value;
        if (oldest) seenNativeSources.delete(oldest);
      }
    };

    const completionSourceKey = (task: Task, detail?: TaskDetail): string | null => {
      const run = detail?.runs.find((candidate) => candidate.agent_kind === "main");
      if (!run?.ended_at) return null;
      if (task.state === "review_ready") return `review:${task.id}:${run.id}`;
      if (task.state === "idle" && run.review_state === "failed") {
        return `run_failed:${task.id}:${run.id}`;
      }
      return null;
    };

    const notifyCompletion = (task: Task, detail?: TaskDetail) => {
      const sourceKey = completionSourceKey(task, detail);
      if (sourceKey && seenNativeSources.has(sourceKey)) return;
      // A background result belongs to native_notification. Do not pre-claim its source here:
      // the bridge still needs to distinguish a delivered system banner from an in-app fallback.
      if (sourceKey && !appWindowIsForeground()) return;
      if (sourceKey) rememberNativeSource(sourceKey);
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
        action: task.state === "review_ready" || partial
          ? {
              label: translate("notifications.actions.reviewChanges"),
              run: () => openRoom(task.id, "review"),
            }
          : {
              label: translate(task.state === "idle"
                ? "notifications.actions.openTask"
                : "notifications.actions.openConversation"),
              run: () => openRoom(task.id),
            },
      });
    };

    const nativeEventAlreadyVisible = (event: NativeNotificationEvent): boolean => {
      if (event.target.type !== "task") return false;
      const taskId = event.target.task_id;
      const app = useAppStore.getState();
      if (app.scene === "room" && app.currentTaskId === taskId) return true;
      if (event.kind !== "permission_required") return false;
      if (app.scene === "inbox") return true;
      if (app.scene !== "dashboard") return false;
      const tasks = useTasksStore.getState();
      const task = tasks.tasks.find((candidate) => candidate.id === taskId);
      return task?.workspace_path === tasks.currentProjectId;
    };

    const handleNativeNotification = (event: NativeNotificationEvent) => {
      if (seenNativeSources.has(event.source_key)) return;
      rememberNativeSource(event.source_key);
      // system 表示横幅已经交给操作系统；这里只登记 source，阻止轮询层再次弹 toast。
      if (event.delivery !== "in_app" || nativeEventAlreadyVisible(event)) return;

      const targetTaskId = event.target.type === "task" ? event.target.task_id : null;
      const action = targetTaskId
        ? {
            label: translate(event.kind === "permission_required"
              ? "notifications.actions.handleTask"
              : event.kind === "review_ready"
                ? "notifications.actions.reviewChanges"
                : "notifications.actions.openTask"),
            run: () => openRoom(
              targetTaskId,
              event.kind === "review_ready" ? "review" : undefined,
            ),
          }
        : undefined;
      const kind: ToastKind = event.kind === "permission_required"
        ? "warn"
        : event.kind === "run_failed"
          ? "error"
          : "success";
      pushToast({
        id: `native:${event.source_key}`,
        kind,
        title: event.title,
        body: event.body,
        action,
      });
    };

    const handleNativeOpen = (payload: NativeNotificationOpenPayload) => {
      routeNativeNotificationOpen(payload, (taskId) => openRoom(taskId));
      void notificationMarkRead(payload.notification_id).catch(() => {});
    };

    let disposed = false;
    const unlisteners: Array<() => void> = [];
    const registerUnlistener = (promise: Promise<() => void>) => {
      void promise.then((unlisten) => {
        if (disposed) unlisten();
        else unlisteners.push(unlisten);
      }).catch(() => {});
    };
    registerUnlistener(onNativeNotification(handleNativeNotification));
    registerUnlistener(onNativeNotificationOpen(handleNativeOpen));

    const syncNativeLocale = () => {
      void nativeNotificationSetLocale(getAppLocale()).catch(() => {});
    };
    syncNativeLocale();
    i18n.on("languageChanged", syncNativeLocale);

    const unsubscribeTasks = useTasksStore.subscribe((state, prev) => {
      if (state.tasks !== prev.tasks) {
        for (const task of state.tasks) {
          const before = seenState.get(task.id);
          seenState.set(task.id, task.state);
          // before === undefined：首次见到这个任务，它的当前状态属于初始快照。
          if (before === undefined || before === task.state) continue;
          if (!ACTIVE_STATES.has(before) || !TERMINAL_STATES.has(task.state)) continue;
          if (task.state === "review_ready" || task.state === "idle") {
            const detail = state.details[task.id];
            // task 与 detail 分批刷新。等 run 的 ended_at/summary 到位后再决定是普通
            // 成功、部分成功还是失败，避免先弹一条误导性的结果通知。
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
          if (!task || (task.state !== "review_ready" && task.state !== "idle")) {
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
          const unseen = fresh.filter(
            (permission) => !seenNativeSources.has(`permission:${permission.id}`),
          );
          if (unseen.length === 0) continue;
          if (!appWindowIsForeground()) continue;
          for (const permission of unseen) {
            rememberNativeSource(`permission:${permission.id}`);
          }
          // 当前页面已经能直接处理该请求时，不再叠一层全局弹窗。
          // Dashboard 只展示当前项目的待处理项；Inbox 则展示全部待处理项。
          const alreadyVisible =
            app.scene === "inbox" ||
            (app.scene === "room" && app.currentTaskId === taskId) ||
            (app.scene === "dashboard" && task.workspace_path === state.currentProjectId);
          if (alreadyVisible) continue;
          pushToast({
            kind: "warn",
            title: translate("notifications.permissionTitle", { task: taskLabel(task) }),
            body:
              unseen.length > 1
                ? translate("notifications.permissionManyBody", {
                    count: unseen.length,
                    tool: unseen[0].tool_name,
                  })
                : translate("notifications.permissionSingleBody", { tool: unseen[0].tool_name }),
            action: {
              label: translate("notifications.actions.handleTask"),
              run: () => openRoom(taskId),
            },
          });
        }
      }
    });

    return () => {
      disposed = true;
      unsubscribeTasks();
      i18n.off("languageChanged", syncNativeLocale);
      for (const unlisten of unlisteners) unlisten();
    };
  }, []);
}
