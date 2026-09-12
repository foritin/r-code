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
}

export const emptyAggregation: AggregationState = { pending: [], lastSeq: 0 };

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
  return sortState({ pending, lastSeq: event.seq });
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
