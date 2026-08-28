// M4-01：Run Capsule 派生模型（R-TRACE-01/02、§5.4/§5.5）。
//
// 合同：
// - detail_state 四态：auto_compact | auto_expanded | user_compact | user_expanded；
//   user 态优先于自动规则，但新的失败/审批/提问仍强制相关条目可见（不销毁用户选择）。
// - fold 只影响呈现：事件永不删除；失败/审批/提问/final 永不可藏。
// - 父终态级联：计时器封口、迟到事件只计诊断（rejectedLate）不复活；
// - live 序列化 → 重放：结构/顺序/终态/fold 初始规则一致（同 reducer）。

export type CapsuleEventKind =
  | "commentary" | "tool" | "subagent" | "attention"
  | "approval" | "question" | "final" | "error" | "warning";

export interface CapsuleEvent {
  readonly seq: number;
  readonly kind: CapsuleEventKind;
  readonly status: "running" | "ok" | "error";
  readonly summary: string;
  /** raw reasoning / secret 永不进入 latest update：写入前由 sanitizeCapsuleText 清洗。 */
}

export type CapsuleFold = "auto_compact" | "auto_expanded" | "user_compact" | "user_expanded";

export interface RunCapsule {
  readonly runId: string;
  readonly fold: CapsuleFold;
  readonly events: readonly CapsuleEvent[];
  readonly terminal: null | { state: "completed" | "failed" | "cancelled" | "interrupted"; at: number };
  /** 迟到/重复事件诊断计数（不复活、不呈现为 running）。 */
  readonly rejectedLate: number;
  readonly timerRunning: boolean;
}

const ALWAYS_VISIBLE: readonly CapsuleEventKind[] = [
  "attention", "approval", "question", "final", "error", "warning",
];

/** 失败/审批/提问/final 不可藏。 */
export function isForcedVisible(kind: CapsuleEventKind): boolean {
  return ALWAYS_VISIBLE.includes(kind);
}

/** §5.4 自动折叠规则（用户态优先级在其上）。 */
export function autoFold(events: readonly CapsuleEvent[], terminal: RunCapsule["terminal"]): CapsuleFold {
  const hasForced = events.some((e) => isForcedVisible(e.kind) && e.status === "running");
  if (hasForced) return "auto_expanded";
  void terminal;
  return "auto_compact";
}

/** 用户操作优先：user_compact/user_expanded 在本 Run 生命周期内覆盖自动规则。 */
export function applyUserFold(prev: CapsuleFold, user: "compact" | "expand"): CapsuleFold {
  return user === "compact" ? "user_compact" : "user_expanded";
}

/** 迟到帧：终态后到达的事件只计诊断。 */
export function ingestEvent(
  capsule: RunCapsule,
  event: CapsuleEvent,
  now: number,
): RunCapsule {
  if (capsule.terminal) {
    return { ...capsule, rejectedLate: capsule.rejectedLate + 1 };
  }
  const events = [...capsule.events, event];
  const fold = capsule.fold.startsWith("user_")
    ? capsule.fold
    : autoFold(events, capsule.terminal);
  return { ...capsule, events, fold };
}

/** 父终态：计时封口 + 单调 terminal（重复设置同值幂等，改值拒绝）。 */
export function terminateCapsule(
  capsule: RunCapsule,
  state: NonNullable<RunCapsule["terminal"]>["state"],
  at: number,
): RunCapsule {
  if (capsule.terminal) return capsule; // 单调：不复活、不覆盖
  const events = capsule.events.map((e) =>
    e.status === "running" ? { ...e, status: "error" as const } : e,
  );
  return { ...capsule, terminal: { state, at }, events, timerRunning: false };
}

/** live 序列化 → 重放：结构/顺序/终态一致（同字段投影，JSON 安全）。 */
export function serializeCapsule(c: RunCapsule): string {
  return JSON.stringify({
    runId: c.runId,
    fold: c.fold,
    terminal: c.terminal,
    timerRunning: c.timerRunning,
    rejectedLate: c.rejectedLate,
    events: c.events.map((e) => ({ seq: e.seq, kind: e.kind, status: e.status, summary: e.summary })),
  });
}

export function deserializeCapsule(raw: string): RunCapsule {
  const o = JSON.parse(raw);
  return {
    runId: o.runId,
    fold: o.fold,
    terminal: o.terminal,
    timerRunning: o.timerRunning,
    rejectedLate: o.rejectedLate,
    events: o.events,
  };
}

/** A4：raw reasoning / secret 清洗（保守：命中即整行替换为占位符）。 */
export function sanitizeCapsuleText(text: string): string {
  return text
    .replace(/(raw[ _-]?reasoning|reasoning|thinking)\s*[:=][^\n]*/gi, "[REDACTED]")
    .replace(/(sk-[A-Za-z0-9_-]{12,}|gh[pousr]_[A-Za-z0-9]{20,})/g, "[REDACTED]");
}
