// M1-03 共享状态投影（PRD §4.4 / R-STATUS-01）。
//
// 这是 Rail / Room / Workbench / 通知 / Automation 唯一允许的状态语义来源：
// - DISPLAY_PRIORITY 实现 §4.4 优先级表，全组合输出唯一；
// - attention 只由派生事实（待审/question/binding/run_failed/review）构成，
//   unread 是通知计数，永远不反向改变 Attention；
// - 终态单调：completed/failed/cancelled/interrupted 一旦成立，迟到的
//   running/queued/attention 帧（重复投递、乱序回放）不能复活或翻转它；
// - 父终态级联：同一快照内的 child/tool/timer 在一次 reduce 中原子封口，
//   不依赖 UI timeout，也不隐藏节点伪装终结。

import type { TaskAttention, TaskDisplayState } from "./types";

/** PRD §4.4 数字越大优先级越高。 */
const DISPLAY_PRIORITY: Readonly<Record<TaskDisplayState, number>> = {
  archived: 100,
  waiting_for_approval: 90,
  waiting_for_question: 90,
  interrupted: 80,
  failed: 80,
  workspace_binding_invalid: 80,
  review_ready: 70,
  verification_required: 70,
  verifying: 60,
  running: 50,
  queued: 40,
  idle: 10,
};

export const TERMINAL_DISPLAY_STATES: readonly TaskDisplayState[] = [
  "archived",
  "failed",
  "interrupted",
];

function isTerminal(state: TaskDisplayState): boolean {
  return TERMINAL_DISPLAY_STATES.includes(state);
}

/** PRD §4.4 数字越大优先级越高。声明序即同级并列时的稳定次级序。 */
const DISPLAY_STATE_ORDER = Object.keys(DISPLAY_PRIORITY) as TaskDisplayState[];

function higher(left: TaskDisplayState, right: TaskDisplayState): TaskDisplayState {
  const delta = DISPLAY_PRIORITY[left] - DISPLAY_PRIORITY[right];
  if (delta !== 0) return delta > 0 ? left : right;
  // 同级（如 approval|question）必须与输入顺序无关：取声明序更靠前者。
  return DISPLAY_STATE_ORDER.indexOf(left) <= DISPLAY_STATE_ORDER.indexOf(right) ? left : right;
}

/** 派生 Attention 的事实键；unread_count 任何取值都不得进入该集合。 */
export interface ProjectionInput {
  display_states: readonly TaskDisplayState[];
  pending_permissions: number;
  pending_questions: number;
  binding_invalid: boolean;
  latest_run_failed: boolean;
  review_pending: boolean;
  /** 仅用于通知徽标；不得影响 display_state 或 attention。 */
  unread_count?: number;
}

export interface StatusProjection {
  display_state: TaskDisplayState;
  attention: TaskAttention[];
}

/** §4.4 优先级归并：多来源帧合成唯一显示态 + Attention 集。 */
export function projectStatus(input: ProjectionInput): StatusProjection {
  let state: TaskDisplayState =
    input.display_states.length > 0
      ? input.display_states.reduce(higher)
      : "idle";
  const attention: TaskAttention[] = [];
  if (input.pending_permissions > 0) attention.push("approval_required");
  if (input.pending_questions > 0) attention.push("user_question");
  if (input.binding_invalid) attention.push("workspace_binding_invalid");
  if (input.latest_run_failed) {
    attention.push("run_failed");
    state = higher(state, "failed");
  }
  if (input.review_pending) {
    attention.push("review_required");
    state = higher(state, "review_ready");
  }
  return { display_state: state, attention };
}

/**
 * 终态单调合并：已到终态的任务收到迟到/重复帧时保持原终态。
 * previous 为 undefined 时接受当前帧；这是唯一能改变终态的路径
 * （显式 restore/unarchive 动作由调用方先清除投影再重放）。
 */
export function mergeMonotonic(
  previous: TaskDisplayState | undefined,
  incoming: TaskDisplayState,
): TaskDisplayState {
  if (previous === undefined) return incoming;
  if (isTerminal(previous)) return previous;
  return higher(previous, incoming);
}

export interface CascadeSnapshot {
  parent_display_state: TaskDisplayState;
  children: { id: string; display_state: TaskDisplayState }[];
  tools: { id: string; status: "running" | "ok" | "error" }[];
  timers: { id: string; running: boolean }[];
}

export interface CascadeOutcome {
  parent: TaskDisplayState;
  children: { id: string; display_state: TaskDisplayState }[];
  tools: { id: string; status: "running" | "ok" | "error" }[];
  timers: { id: string; running: boolean }[];
}

/** 父终态 → 同步封口全部未终结子节点；已终结者保持原样（审计友好）。 */
export function cascadeParentTerminal(snapshot: CascadeSnapshot): CascadeOutcome {
  const parentTerminated = isTerminal(snapshot.parent_display_state);
  return {
    parent: snapshot.parent_display_state,
    children: snapshot.children.map((c) => ({
      id: c.id,
      display_state: parentTerminated && !isTerminal(c.display_state)
        ? "interrupted"
        : c.display_state,
    })),
    tools: snapshot.tools.map((t) => ({
      id: t.id,
      status: parentTerminated && t.status === "running" ? "error" : t.status,
    })),
    timers: snapshot.timers.map((t) => ({
      id: t.id,
      running: parentTerminated ? false : t.running,
    })),
  };
}

// ---- M1-03.A5：状态 glyph 合同（身份色/身份图形与状态分离）----
// spinning 仅允许 running 与 checking(verifying)；其余状态一律静态图形+文案，
// queued/waiting/approval/terminal 不使用任何旋转指示。

export interface StatusGlyph {
  readonly glyph: string;
  readonly spinning: boolean;
}

export const STATUS_GLYPHS: Readonly<Record<TaskDisplayState, StatusGlyph>> = {
  archived: { glyph: "■", spinning: false },
  waiting_for_approval: { glyph: "◆", spinning: false },
  waiting_for_question: { glyph: "◇", spinning: false },
  failed: { glyph: "✕", spinning: false },
  interrupted: { glyph: "‖", spinning: false },
  workspace_binding_invalid: { glyph: "⚠", spinning: false },
  review_ready: { glyph: "☰", spinning: false },
  verification_required: { glyph: "⊘", spinning: false },
  verifying: { glyph: "◐", spinning: true },
  running: { glyph: "●", spinning: true },
  queued: { glyph: "▪", spinning: false },
  idle: { glyph: "✓", spinning: false },
};

export function statusGlyph(state: TaskDisplayState): StatusGlyph {
  return STATUS_GLYPHS[state];
}
