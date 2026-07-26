/**
 * Deck 场景聚合器 —— 由 tasks + details 派生态势带 gauges、三段门、动作行、settled 数据。
 * 全部为纯函数；时间敏感逻辑经 now 参数注入，渲染与测试都确定。
 * 注意：TaskEvent 是轻量投影（只有 type + timestamp），动作行的 target
 * 需从 changes / permissions / verifications 的具体负载里取。
 */
import type { Task, TaskDetail, VerificationRecord, Workspace } from "./types";
import { toolTarget, toolVerb } from "./format";

// ---------- 任务分桶 ----------

/** 未完结任务（需要 2s 轮询 detail 的口径，与 Rail 一致）。 */
export function isLiveTask(t: Task, detail?: TaskDetail): boolean {
  return (
    (t.state !== "idle" && t.state !== "archived") ||
    detail?.runs.some((run) => run.ended_at == null) === true
  );
}

/** 在飞任务（changes 悬而未决）：进行中 / 探索中 / 待审查。 */
export function isInFlightTask(t: Task): boolean {
  return t.state === "in_progress" || t.state === "exploring" || t.state === "review_ready";
}

/** 工作区路径 → 展示名（未附加工作区时明确显示为聊天）。 */
export function projectName(workspaces: Workspace[], workspacePath: string | null): string {
  if (!workspacePath) return "聊天";
  return (
    workspaces.find((w) => w.canonical_path === workspacePath)?.display_name ??
    workspacePath.split(/[\\/]/).pop() ??
    workspacePath
  );
}

/** 路径末段（worktree chip 用）。 */
export function baseName(p: string): string {
  return p.split(/[\\/]/).filter(Boolean).pop() ?? p;
}

// ---------- 态势带 gauges ----------

export interface DeckGauges {
  /** in_progress / exploring 任务数 */
  running: number;
  /** Needs you 待决项数（selectNeedsYou 长度，由调用方传入） */
  needsYou: number;
  /** 今日 status=passed 的 verification 数（本地日界） */
  verifiedToday: number;
  /** 在飞任务的 changes 总数 */
  filesInFlight: number;
  /** 近 7 天被接受（accepted / auto_accepted）的 run 数 */
  acceptedPerWeek: number;
}

const DAY_MS = 86_400_000;

function isToday(iso: string, now: number): boolean {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return false;
  const d = new Date(t);
  const n = new Date(now);
  return (
    d.getFullYear() === n.getFullYear() &&
    d.getMonth() === n.getMonth() &&
    d.getDate() === n.getDate()
  );
}

export function computeGauges(
  tasks: Task[],
  details: Record<string, TaskDetail>,
  needsYou: number,
  now = Date.now(),
): DeckGauges {
  let verifiedToday = 0;
  let filesInFlight = 0;
  let acceptedPerWeek = 0;
  const weekAgo = now - 7 * DAY_MS;

  for (const t of tasks) {
    const d = details[t.id];
    if (!d) continue;
    if (isInFlightTask(t)) filesInFlight += d.changes.length;
    for (const v of d.verifications) {
      if (v.status === "passed" && isToday(v.ended_at ?? v.started_at, now)) verifiedToday += 1;
    }
    for (const r of d.runs) {
      if (r.review_state !== "accepted" && r.review_state !== "auto_accepted") continue;
      const at = Date.parse(r.ended_at ?? r.started_at);
      if (!Number.isNaN(at) && at >= weekAgo) acceptedPerWeek += 1;
    }
  }

  return {
    running: tasks.filter(
      (t) =>
        t.state === "in_progress" ||
        t.state === "exploring" ||
        details[t.id]?.runs.some((run) => run.ended_at == null) === true
    ).length,
    needsYou,
    verifiedToday,
    filesInFlight,
    acceptedPerWeek,
  };
}

// ---------- 舰队示波器 ----------

/** 近 windowMs 内全部已加载任务的事件数（示波器能量种子）。 */
export function recentEventCount(
  details: Record<string, TaskDetail>,
  windowMs: number,
  now = Date.now(),
): number {
  let n = 0;
  const since = now - windowMs;
  for (const d of Object.values(details)) {
    for (const e of d.events) {
      const t = Date.parse(e.created_at);
      if (!Number.isNaN(t) && t >= since) n += 1;
    }
  }
  return n;
}

/** mulberry32 —— 确定性伪随机：同一种子同一波形，轮询间不抖动。 */
function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** 示波器柱高（%）：由最近事件数播种，闲时低平、忙时起伏。 */
export function waveHeights(seed: number, bars = 40): number[] {
  const rand = mulberry32(seed * 97 + 11);
  const energy = Math.min(1, seed / 24);
  const base = 10 + energy * 30; // 10–40
  const span = 14 + energy * 48; // 14–62
  const out: number[] = [];
  for (let i = 0; i < bars; i++) out.push(Math.round(base + rand() * span));
  return out;
}

// ---------- 三段门（Plan → Perms → Verify） ----------

export type GateState = "idle" | "wait" | "active" | "done" | "fail";

export interface TaskGates {
  plan: GateState;
  perms: GateState;
  verify: GateState;
  /** Perms 门计数：wait 时为待批数，done 时为已决数 */
  permCount: number;
}

export function gatesFor(task: Task, detail: TaskDetail | undefined): TaskGates {
  // Plan：探索中 = 计划进行；进入执行/审查/归档即过；idle 看是否跑过
  let plan: GateState = "idle";
  if (task.state === "exploring") plan = "active";
  else if (
    task.state === "in_progress" ||
    task.state === "review_ready" ||
    task.state === "archived"
  )
    plan = "done";
  else if ((detail?.runs.length ?? 0) > 0) plan = "done";

  // Perms：待批 → wait（计数待批）；有已决 → done（计数已决）；无请求但在跑 → done（无需批准）
  let perms: GateState = "idle";
  let permCount = 0;
  if (detail) {
    const pending = detail.permissions.filter((p) => p.decision === "pending");
    const decided = detail.permissions.length - pending.length;
    if (pending.length > 0) {
      perms = "wait";
      permCount = pending.length;
    } else if (decided > 0) {
      perms = "done";
      permCount = decided;
    } else if (task.state === "in_progress" || task.state === "review_ready") {
      perms = "done";
    }
  }

  return { plan, perms, verify: verifyState(detail), permCount };
}

/** 最新一条 verification（按 started_at）。 */
export function latestVerification(
  detail: TaskDetail | undefined,
): VerificationRecord | undefined {
  if (!detail || detail.verifications.length === 0) return undefined;
  const sorted = [...detail.verifications].sort((a, b) =>
    a.started_at.localeCompare(b.started_at),
  );
  return sorted[sorted.length - 1];
}

function verifyState(detail: TaskDetail | undefined): GateState {
  if (!detail || detail.verifications.length === 0) return "idle";
  if (detail.verifications.some((v) => v.status === "running")) return "active";
  const last = latestVerification(detail);
  if (!last) return "idle";
  switch (last.status) {
    case "passed":
      return "done";
    case "failed":
    case "timeout":
      return "fail";
    default:
      return "wait"; // stale / superseded
  }
}

// ---------- 动作行（fleet card / rows 的 act 列） ----------

export interface ActionLine {
  verb: string;
  target: string;
}

/**
 * 最近动作：running verification > file_changed > 待批权限 > verification_run > tool 事件。
 * verb/target 复用 format 的 toolVerb/toolTarget；events 只带类型与时间，
 * target 从 changes / permissions / verifications 的负载取。
 */
export function actionLineFor(task: Task, detail: TaskDetail | undefined): ActionLine | null {
  if (!detail) return null;

  const runningVerify = detail.verifications.find((v) => v.status === "running");
  if (runningVerify) {
    return { verb: toolVerb(runningVerify.command), target: runningVerify.command };
  }

  const lastEvent = [...detail.events].sort((a, b) => b.id - a.id)[0];
  if (!lastEvent) {
    return task.state === "exploring" ? { verb: "read", target: "exploring repository" } : null;
  }

  const latestChange = [...detail.changes].sort((a, b) =>
    b.created_at.localeCompare(a.created_at),
  )[0];

  switch (lastEvent.event_type) {
    case "file_changed":
      if (latestChange) {
        return {
          verb: toolVerb(latestChange.change_type === "create" ? "create_file" : "edit_file"),
          target: latestChange.path,
        };
      }
      break;
    case "permission_requested": {
      const p = detail.permissions.find((x) => x.decision === "pending");
      if (p) {
        return {
          verb: toolVerb(p.tool_name),
          target: toolTarget(p.input_summary) || p.input_summary,
        };
      }
      break;
    }
    case "verification_run": {
      const v = latestVerification(detail);
      if (v) return { verb: toolVerb(v.command), target: v.command };
      break;
    }
    case "tool_call":
    case "tool_result":
      if (latestChange) return { verb: "edit", target: latestChange.path };
      return { verb: "tool", target: "working…" };
    case "subagent_started": {
      const active = detail.runs.filter(
        (run) => run.agent_kind === "subagent" && run.ended_at == null
      ).length;
      return { verb: "investigate", target: `${active || 1} 个子代理正在工作` };
    }
    case "subagent_finished":
      return { verb: "subagent", target: "调查结果已返回主代理" };
    default:
      break;
  }

  if (task.state === "exploring") return { verb: "read", target: "exploring repository" };
  return null;
}

// ---------- settled（已 accept / answered / rolled back） ----------

export type SettledOutcome = "accepted" | "answered" | "rolled_back" | "aborted";

export interface SettledItem {
  task: Task;
  outcome: SettledOutcome;
  /** 落定时间（run.ended_at，退化为 task.updated_at） */
  when: string;
  /** 该任务的变更（diffstat 展示用） */
  changes: TaskDetail["changes"];
}

/** 最近 limit 条 settled 任务，按落定时间倒序。 */
export function settledItems(
  tasks: Task[],
  details: Record<string, TaskDetail>,
  limit = 10,
): SettledItem[] {
  const out: SettledItem[] = [];
  for (const t of tasks) {
    const d = details[t.id];
    if (!d) continue;
    const ended = d.runs
      .filter((r) => r.ended_at !== null)
      .sort((a, b) => (b.ended_at ?? "").localeCompare(a.ended_at ?? ""))[0];
    if (!ended) continue;
    let outcome: SettledOutcome | null = null;
    if (ended.review_state === "accepted" || ended.review_state === "auto_accepted")
      outcome = "accepted";
    else if (ended.review_state === "answered") outcome = "answered";
    else if (ended.review_state === "rolled_back") outcome = "rolled_back";
    else if (ended.review_state === "aborted") outcome = "aborted";
    if (!outcome) continue;
    out.push({ task: t, outcome, when: ended.ended_at ?? t.updated_at, changes: d.changes });
  }
  out.sort((a, b) => b.when.localeCompare(a.when));
  return out.slice(0, limit);
}
