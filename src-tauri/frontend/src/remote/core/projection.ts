/**
 * R07 remote core — 平台无关事件投影器（PWA 与未来 RN App 共享；R21 抽包前身）。
 *
 * 输入是 daemon 的 EventEnvelope（task.events / 长连接推送同形，payload.journalKind
 * 判别），输出是 UI 可渲染的会话行。本文件不 import DOM/Node/RN 专属 API（F13/F14）。
 */

export interface EventEnvelope {
  seq: number;
  task_id: string;
  run_id: string;
  kind: string;
  payload: Record<string, unknown> & { journalKind?: string };
}

/** 会话行（UI model）。 */
export type SessionRow =
  | { type: "user"; runId: string; text: string }
  | { type: "assistant"; runId: string; text: string }
  | { type: "tool"; runId: string; name: string; ok: boolean }
  | { type: "state"; runId: string; label: string; tone: "run" | "ok" | "fail" };

export interface SessionProjection {
  rows: SessionRow[];
  running: boolean;
  pendingApproval: { opId: string; summary: string } | null;
}

export function emptyProjection(): SessionProjection {
  return { rows: [], running: false, pendingApproval: null };
}

function kindOf(event: EventEnvelope): string {
  const fromPayload = event.payload?.journalKind;
  return typeof fromPayload === "string" ? fromPayload : "";
}

function textOf(value: unknown): string {
  return typeof value === "string" ? value : "";
}

/** 把一个事件并入会话投影（纯函数；实时流与 /resume 全量重建共用）。 */
export function applyEvent(state: SessionProjection, event: EventEnvelope): SessionProjection {
  const kind = kindOf(event);
  switch (kind) {
    case "input.queued": {
      const text = textOf(event.payload.text);
      if (!text) return state;
      return { ...state, rows: [...state.rows, { type: "user", runId: event.run_id, text }] };
    }
    case "assistant.message": {
      const text = textOf(event.payload.text);
      if (!text) return state;
      return {
        ...state,
        rows: [...state.rows, { type: "assistant", runId: event.run_id, text }],
      };
    }
    case "tool.call":
      return {
        ...state,
        rows: [
          ...state.rows,
          {
            type: "tool",
            runId: event.run_id,
            name: textOf(event.payload.name),
            ok: true,
          },
        ],
      };
    case "tool.result": {
      // Attach the outcome to the most recent matching tool row.
      const name = textOf(event.payload.name);
      const rows = [...state.rows];
      for (let index = rows.length - 1; index >= 0; index -= 1) {
        const row = rows[index];
        if (row.type === "tool" && row.name === name) {
          rows[index] = { ...row, ok: event.payload.ok === true };
          break;
        }
      }
      return { ...state, rows };
    }
    case "run.started":
      return { ...state, running: true, rows: [...state.rows, { type: "state", runId: event.run_id, label: "run started", tone: "run" }] };
    case "run.completed":
      return {
        ...state,
        running: false,
        rows: [...state.rows, { type: "state", runId: event.run_id, label: "completed", tone: "ok" }],
      };
    case "run.failed":
      return {
        ...state,
        running: false,
        rows: [...state.rows, { type: "state", runId: event.run_id, label: `failed: ${textOf(event.payload.error) || "unknown"}`, tone: "fail" }],
      };
    case "run.cancelled":
      return {
        ...state,
        running: false,
        rows: [...state.rows, { type: "state", runId: event.run_id, label: "cancelled", tone: "run" }],
      };
    case "approval.requested":
      return {
        ...state,
        pendingApproval: {
          opId: textOf(event.payload.opId),
          summary: textOf(event.payload.summary),
        },
      };
    case "approval.decided":
      return { ...state, pendingApproval: null };
    default:
      return state;
  }
}

/** 全量投影（游标重放 / 长连接增量共用同一入口）。 */
export function projectEvents(events: EventEnvelope[]): SessionProjection {
  return events.reduce(applyEvent, emptyProjection());
}
