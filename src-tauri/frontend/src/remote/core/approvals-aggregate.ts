/**
 * R07c — 审批聚合（纯 TS）：跨任务 pending op 排序（createdSeq 升序）、
 * 决策后移除（事件驱动 + 乐观回滚共用）、事件游标续传（after_seq 重放
 * 不重不丢）。无 DOM 依赖（PWA/RN 共享）。
 */

import type { EventEnvelope } from "./projection.ts";

export interface AggregatedApproval {
  opId: string;
  summary: string;
  taskId: string;
  runId: string;
  createdSeq: number;
}

export interface AggregationState {
  pending: AggregatedApproval[];
  /** 已消费的事件游标（重连后从 lastSeq+1 续传）。 */
  lastSeq: number;
  /** P19B-C：本任务已落库的 effect 授权（active/superseded 都保留作审计）。 */
  effectApprovals: EffectApprovalView[];
  /** P19B-C：本任务仍可决策的 effect 待决请求。 */
  effectPending: EffectApprovalPending[];
}

export const emptyAggregation: AggregationState = {
  pending: [],
  lastSeq: 0,
  effectApprovals: [],
  effectPending: [],
};

// ---------------------------------------------------------------------------
// P19B-C 效果授权投影（与桌面 Permissions/Canvas 渲染同一份权威）
// ---------------------------------------------------------------------------

/** 权威材料的六个 camelCase 键，顺序即 daemon 冻结的键序（三端一致）。 */
export const EFFECT_MATERIAL_FIELDS = [
  "taskId",
  "planRevision",
  "workUnitId",
  "effectClass",
  "network",
  "payloadHash",
] as const;

export type EffectMaterialKey = (typeof EFFECT_MATERIAL_FIELDS)[number];

export type EffectMaterial = Record<EffectMaterialKey, string>;

/** 落库审批视图（逐字段等于 store 记录）。 */
export type EffectApprovalView = EffectMaterial & {
  approvalId: string;
  actorId: string;
  sessionId: string;
  scope: string;
  state: "active" | "superseded";
  createdAtMs: number;
  supersededAtMs?: number;
};

export type EffectApprovalPending = EffectMaterial & { operationId: string };

/**
 * `approvals.effect.list` 响应 → 投影。坏行一律丢弃（缺键即不是权威），
 * 绝不补默认值伪造出看似有效的授权——失败关闭。
 */
export function projectEffectApprovals(
  state: AggregationState,
  answer: Record<string, unknown> | null | undefined,
): AggregationState {
  const taskId = typeof answer?.taskId === "string" ? answer.taskId : "";
  if (!taskId) return state;
  const approvals = Array.isArray(answer?.approvals) ? answer.approvals : [];
  const pending = Array.isArray(answer?.pending) ? answer.pending : [];
  const nextApprovals = approvals
    .map(projectApproval)
    .filter((row): row is EffectApprovalView => row !== null);
  const nextPending = pending
    .map(projectPending)
    .filter((row): row is EffectApprovalPending => row !== null);
  return { ...state, effectApprovals: nextApprovals, effectPending: nextPending };
}

/** 一条授权的展示行 = 六列材料 + 审批身份/归属/作用域（与桌面同序）。 */
export function effectAuthorityRows(
  approval: EffectApprovalView,
): Array<{ key: string; value: string }> {
  return [
    ...EFFECT_MATERIAL_FIELDS.map((key) => ({ key, value: approval[key] })),
    { key: "approvalId", value: approval.approvalId },
    { key: "actorId", value: approval.actorId },
    { key: "sessionId", value: approval.sessionId },
    { key: "scope", value: approval.scope },
  ];
}

/** 一条待决请求的展示行 = 六列材料 + 待决操作号（与桌面同序）。 */
export function effectPendingRows(
  pending: EffectApprovalPending,
): Array<{ key: string; value: string }> {
  return [
    ...EFFECT_MATERIAL_FIELDS.map((key) => ({ key, value: pending[key] })),
    { key: "operationId", value: pending.operationId },
  ];
}

function projectMaterial(row: Record<string, unknown>): EffectMaterial | null {
  const material = {} as EffectMaterial;
  for (const key of EFFECT_MATERIAL_FIELDS) {
    const value = row[key];
    if (typeof value !== "string") return null;
    material[key] = value;
  }
  return material;
}

function projectApproval(value: unknown): EffectApprovalView | null {
  if (typeof value !== "object" || value === null) return null;
  const row = value as Record<string, unknown>;
  const material = projectMaterial(row);
  const state = row.state;
  if (!material) return null;
  if (typeof row.approvalId !== "string" || typeof row.actorId !== "string") return null;
  if (typeof row.sessionId !== "string" || typeof row.scope !== "string") return null;
  if (state !== "active" && state !== "superseded") return null;
  if (typeof row.createdAtMs !== "number") return null;
  return {
    ...material,
    approvalId: row.approvalId,
    actorId: row.actorId,
    sessionId: row.sessionId,
    scope: row.scope,
    state,
    createdAtMs: row.createdAtMs,
    ...(typeof row.supersededAtMs === "number" ? { supersededAtMs: row.supersededAtMs } : {}),
  };
}

function projectPending(value: unknown): EffectApprovalPending | null {
  if (typeof value !== "object" || value === null) return null;
  const row = value as Record<string, unknown>;
  const material = projectMaterial(row);
  if (!material || typeof row.operationId !== "string") return null;
  return { ...material, operationId: row.operationId };
}


/** approvals.list 行 → 聚合行（按 createdSeq 升序；空 opId 的坏行丢弃）。 */
export function mergePendingList(
  state: AggregationState,
  rows: Array<Record<string, unknown>>,
): AggregationState {
  const incoming = rows
    .map((row) => ({
      opId: typeof row.opId === "string" ? row.opId : "",
      summary: typeof row.summary === "string" ? row.summary : "",
      taskId: typeof row.taskId === "string" ? row.taskId : "",
      runId: typeof row.runId === "string" ? row.runId : "",
      createdSeq: typeof row.createdSeq === "number" ? row.createdSeq : 0,
    }))
    .filter((row) => row.opId !== "");
  const byId = new Map(state.pending.map((row) => [row.opId, row]));
  for (const row of incoming) {
    byId.set(row.opId, row);
  }
  return sortState({ ...state, pending: [...byId.values()] });
}

/** 事件流合并：requested 添加、decided 移除；seq 单调去重（R07c.A1 续传）。 */
export function applyApprovalEvent(
  state: AggregationState,
  event: EventEnvelope,
): AggregationState {
  if (event.seq <= state.lastSeq) {
    return state; // 重连重放：已消费（不重）
  }
  const kind = event.payload?.journalKind;
  let pending = state.pending;
  if (kind === "approval.requested") {
    const opId = typeof event.payload.opId === "string" ? event.payload.opId : "";
    if (opId && !pending.some((row) => row.opId === opId)) {
      pending = [
        ...pending,
        {
          opId,
          summary: typeof event.payload.summary === "string" ? event.payload.summary : "",
          taskId: event.task_id,
          runId: event.run_id,
          createdSeq: event.seq,
        },
      ];
    }
  } else if (kind === "approval.decided") {
    const opId = typeof event.payload.opId === "string" ? event.payload.opId : "";
    pending = pending.filter((row) => row.opId !== opId);
  }
  return sortState({
    ...state,
    pending,
    lastSeq: event.seq,
    // P19B-C：事件流不带 effect 授权行（那是 approvals.effect.list 快照面），
    // 原样带过，避免事件合并把已投影的授权清空。
  });
}

/** 乐观移除（决策请求已发出；失败时调用方 mergePendingList 回滚）。 */
export function optimisticRemove(
  state: AggregationState,
  opId: string,
): AggregationState {
  return { ...state, pending: state.pending.filter((row) => row.opId !== opId) };
}

function sortState(state: AggregationState): AggregationState {
  return {
    ...state,
    pending: [...state.pending].sort((a, b) => a.createdSeq - b.createdSeq),
  };
}
