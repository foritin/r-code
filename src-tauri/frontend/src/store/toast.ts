import { create } from "zustand";

/**
 * 全局 toast 通知队列。
 *
 * 存在的理由：后台任务跑完、权限卡在待批，用户如果不在对应 Room 里就完全无感——
 * 既没有系统通知也没有应用内提示，只能靠自己回去翻 Deck。
 *
 * 设计约束：
 * - 队列上限 4 条，超出挤掉最旧的；toast 不是日志，堆满屏幕等于没有提示。
 * - 同 kind + title 在 3s 内不叠加，只刷新已有那条的时间戳（轮询会把同一事件
 *   报好几遍，叠加就会变成刷屏）。
 * - error 不自动消失：失败必须由用户手动确认，不能一眨眼就没了。
 * - 额外导出命令式入口 pushToast / dismissToast，让非 React 代码（store 订阅、
 *   轮询回调、事件监听）也能发通知。
 */

export type ToastKind = "info" | "success" | "warn" | "error";

/** 行动按钮：点了之后 toast 自行退场。 */
export interface ToastAction {
  label: string;
  run: () => void;
}

export interface Toast {
  id: string;
  kind: ToastKind;
  title: string;
  body?: string;
  /** 可选的行动按钮 */
  action?: ToastAction;
  /** 毫秒；0 或 undefined 表示不自动消失 */
  timeout?: number;
  createdAt: number;
}

/**
 * push 的入参。id / createdAt 由 store 补齐；
 * timeout 省略 = 用该 kind 的默认档（入队后一定是明确数值，0 即常驻）。
 */
export type ToastInput = Omit<Toast, "id" | "createdAt"> & { id?: string };

/** 队列上限：再多就该去 Inbox 看了。 */
const MAX_TOASTS = 4;

/** 去重窗口：这段时间内同 kind + title 视为同一件事。 */
const DEDUPE_WINDOW_MS = 3000;

const DEFAULT_TIMEOUT: Record<ToastKind, number> = {
  info: 4000,
  success: 4000,
  warn: 6000,
  /** 失败必须手动关闭 */
  error: 0,
};

interface ToastState {
  toasts: Toast[];
  /** 入队（或命中去重时刷新已有那条），返回最终生效的 toast id。 */
  push: (input: ToastInput) => string;
  dismiss: (id: string) => void;
}

let seq = 0;

function nextToastId(): string {
  seq += 1;
  return `toast-${seq.toString(36)}-${Date.now().toString(36)}`;
}

/**
 * 队列溢出裁剪。
 *
 * 不能简单地按时间砍最旧的：常驻条目（timeout === 0，即 error）的契约就是
 * 「必须由用户手动确认」。后台几个任务同时刷 toast 就把唯一需要看的那条错误
 * 顶掉，等于契约作废。所以先淘汰会自动消失的，实在不够再动常驻的。
 */
function evict(list: Toast[]): Toast[] {
  const excess = list.length - MAX_TOASTS;
  if (excess <= 0) return list;

  const doomed = new Set<string>();
  // 第一轮：从最旧开始淘汰「会自己消失」的。
  for (const t of list) {
    if (doomed.size >= excess) break;
    if ((t.timeout ?? 0) > 0) doomed.add(t.id);
  }
  // 第二轮：全是常驻条目，只能按最旧淘汰。
  for (const t of list) {
    if (doomed.size >= excess) break;
    doomed.add(t.id);
  }
  return list.filter((t) => !doomed.has(t.id));
}

export const useToastStore = create<ToastState>((set) => ({
  toasts: [],

  push: (input) => {
    const now = Date.now();
    const timeout = input.timeout ?? DEFAULT_TIMEOUT[input.kind];
    let id = input.id ?? nextToastId();

    // set 是同步的，下面的 id 回写在 return 之前一定已经完成。
    set((s) => {
      // 显式 id 优先匹配：否则同 id 不同 title 会并存两条，React key 撞车。
      const dupIndex = input.id
        ? s.toasts.findIndex((t) => t.id === input.id)
        : s.toasts.findIndex(
            (t) =>
              t.kind === input.kind &&
              t.title === input.title &&
              now - t.createdAt < DEDUPE_WINDOW_MS
          );

      if (dupIndex >= 0) {
        const existing = s.toasts[dupIndex];
        id = existing.id;
        const toasts = s.toasts.slice();
        // createdAt 变化会让渲染层重置自动消失计时，等于"这条又新鲜了一次"。
        toasts[dupIndex] = {
          ...existing,
          body: input.body,
          action: input.action,
          timeout,
          createdAt: now,
        };
        return { toasts };
      }

      const next = [...s.toasts, { ...input, id, timeout, createdAt: now }];
      return { toasts: next.length > MAX_TOASTS ? evict(next) : next };
    });

    return id;
  },

  dismiss: (id) => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
}));

// ---------- 命令式入口（供非 React 代码调用） ----------

/** 发一条 toast，返回其 id（命中去重时是被刷新那条的 id）。 */
export function pushToast(input: ToastInput): string {
  return useToastStore.getState().push(input);
}

/** 立即移除一条 toast（渲染层的退场动画走组件内部的 beginDismiss）。 */
export function dismissToast(id: string): void {
  useToastStore.getState().dismiss(id);
}
