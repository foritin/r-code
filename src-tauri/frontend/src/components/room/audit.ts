/**
 * Summary 审计流构建器 —— 纯函数，不依赖 React。
 *
 * `task_events` 是无 payload 的轻量投影（只有 event_type + created_at），单独渲染
 * 只会得到「工具调用 / 工具结果」这种没有信息量的行。这里改为：把带内容的数据源
 * （会话 JSONL 的 tool_call/tool_result、file_changes、verifications、待批权限、
 * 子代理运行树）合成审计条目，再用 task_events 的时间戳做时间锚点与耗时计算，
 * 只保留那些「不带 payload 也能读懂」的事件类型。
 *
 * 一条审计 = 类型标签 + 主体（工具名·目标）+ 结果（✓ 耗时 / ✗ 原因）+ 时刻。
 */
import type {
  AgentRun,
  FileChange,
  PermissionRequest,
  SessionMessage,
  TaskEvent,
  TaskEventType,
  VerificationRecord,
} from "../../lib/types";
import { formatDurationMs, permissionRiskLabel, toolTarget, toolVerb } from "../../lib/format";
import { summarizeOutput } from "./model";

/** 工具调用构成：按动词聚合（读取/命令/检索/编辑/写入/其他）。 */
export interface ToolComposition {
  read: number;
  command: number;
  search: number;
  edit: number;
  write: number;
  other: number;
  total: number;
}

export type AuditKind = "tool" | "file" | "verify" | "permission" | "agent" | "session";
/** ok=成功 fail=失败 wait=进行中/待处理 info=中性记录 */
export type AuditState = "ok" | "fail" | "wait" | "info";

export interface AuditRow {
  id: string;
  /** 排序用毫秒时间戳；无法定位时间的条目沉到末尾。 */
  at: number | null;
  atIso: string | null;
  kind: AuditKind;
  /** 类型标签：工具 / 文件 / 验证 / 权限 / 子代理 / 会话 */
  tag: string;
  /** 主体，例如 `read_file · README.md` */
  text: string;
  /** 结果列，例如 `✓ 0.3s` / `✗ exit 101` / `等待批准` */
  result: string | null;
  state: AuditState;
  /** 悬停显示的完整信息（长路径、命令、输出摘要）。 */
  title?: string;
}

export interface AuditInput {
  messages: readonly SessionMessage[];
  events: readonly TaskEvent[];
  changes: readonly FileChange[];
  verifications: readonly VerificationRecord[];
  permissions: readonly PermissionRequest[];
  runs: readonly AgentRun[];
}

/** 不带 payload 也能读懂的事件类型；其余类型由上面更具体的数据源覆盖。 */
const SELF_EXPLANATORY_EVENTS: Partial<
  Record<TaskEventType, { kind: AuditKind; tag: string; text: string; state: AuditState }>
> = {
  task_created: { kind: "session", tag: "会话", text: "会话已创建", state: "info" },
  user_steered: { kind: "session", tag: "会话", text: "运行中插入了引导", state: "info" },
  user_message_queued: { kind: "session", tag: "会话", text: "消息已加入队列", state: "info" },
  queue_dispatched: { kind: "session", tag: "会话", text: "排队消息已发出", state: "info" },
  run_aborted: { kind: "session", tag: "会话", text: "运行被中止", state: "fail" },
  session_branched: { kind: "session", tag: "会话", text: "已创建编辑分支", state: "info" },
  session_cleared: { kind: "session", tag: "会话", text: "消息上下文已清空", state: "info" },
  permission_decided: { kind: "permission", tag: "权限", text: "权限已批复", state: "info" },
  change_requested: { kind: "session", tag: "审核", text: "已请求继续修改", state: "info" },
  system: { kind: "session", tag: "系统", text: "系统事件", state: "info" },
};

/** 合成审计流，按时间倒序返回（最新在前）。 */
export function buildAuditFeed(input: AuditInput, limit = 12): AuditRow[] {
  const rows: AuditRow[] = [
    ...toolRows(input.messages, input.events),
    ...fileRows(input.changes),
    ...verificationRows(input.verifications),
    ...permissionRows(input.permissions),
    ...subagentRows(input.runs),
    ...eventRows(input.events),
  ];

  rows.sort((a, b) => {
    if (a.at == null && b.at == null) return 0;
    if (a.at == null) return 1;
    if (b.at == null) return -1;
    return b.at - a.at;
  });
  return rows.slice(0, limit);
}

// ---------- 工具调用 ----------

/**
 * 会话消息本身没有可靠时间戳，沿用时间线的做法：按出现顺序与 task_events 里的
 * tool_call / tool_result 时间戳对齐，从而拿到发起时刻与真实耗时。
 */
function toolRows(messages: readonly SessionMessage[], events: readonly TaskEvent[]): AuditRow[] {
  const ordered = [...events].sort((a, b) => a.id - b.id);
  const callTs = ordered.filter((e) => e.event_type === "tool_call").map((e) => e.created_at);
  const resultTs = ordered.filter((e) => e.event_type === "tool_result").map((e) => e.created_at);

  const rows: AuditRow[] = [];
  const indexByCallId = new Map<string, number>();
  const startedByCallId = new Map<string, number | null>();
  const hiddenCallIds = new Set<string>();
  let callIdx = 0;
  let resultIdx = 0;
  let seq = 0;

  for (const message of messages) {
    if (message.kind === "tool_call") {
      const iso = callTs[callIdx++] ?? message.timestamp ?? null;
      const at = parseMs(iso);
      const name = (message.tool_name ?? "").trim() || "工具";
      if (isCoordinationTool(name)) {
        if (message.call_id) hiddenCallIds.add(message.call_id);
        continue;
      }
      const target = toolTarget(message.input_json);
      const presentation = auditToolPresentation(name);
      const row: AuditRow = {
        id: `tool-${seq++}`,
        at,
        atIso: iso,
        kind: "tool",
        tag: presentation.tag,
        text: target ? shortTarget(target) : presentation.label,
        result: "进行中",
        state: "wait",
        title: target ? `${name}\n${target}` : name,
      };
      rows.push(row);
      if (message.call_id) {
        indexByCallId.set(message.call_id, rows.length - 1);
        startedByCallId.set(message.call_id, at);
      }
      continue;
    }

    if (message.kind !== "tool_result") continue;

    const iso = resultTs[resultIdx++] ?? message.timestamp ?? null;
    const at = parseMs(iso);
    if (message.call_id && hiddenCallIds.has(message.call_id)) continue;
    const failed = message.is_error === true;
    const summary = summarizeOutput(message.output_json, failed);
    const index = message.call_id ? indexByCallId.get(message.call_id) : undefined;

    if (index == null) {
      rows.push({
        id: `tool-${seq++}`,
        at,
        atIso: iso,
        kind: "tool",
        tag: "工具",
        text: "工具结果",
        result: failed ? `✗ ${cut(summary, 32)}` : `✓ ${cut(summary, 32)}`,
        state: failed ? "fail" : "ok",
        title: summary,
      });
      continue;
    }

    const row = rows[index];
    const started = message.call_id ? startedByCallId.get(message.call_id) ?? null : null;
    const spent = started != null && at != null ? formatDuration(at - started) : null;
    row.state = failed ? "fail" : "ok";
    row.result = failed ? `✗ ${cut(summary, 32)}` : spent ? `✓ ${spent}` : "✓ 完成";
    row.title = `${row.title ?? ""}\n${failed ? "失败" : "完成"}：${summary}`.trim();
  }

  return rows;
}

function isCoordinationTool(name: string): boolean {
  const normalized = name.trim().toLowerCase().replace(/[.\-\s]+/g, "_");
  return normalized.endsWith("delegate_task")
    || normalized.endsWith("collect_subagents")
    || normalized.endsWith("spawn_agent")
    || normalized.endsWith("wait_agent")
    || normalized.endsWith("wait_agents");
}

function auditToolPresentation(name: string): { tag: string; label: string } {
  switch (toolVerb(name)) {
    case "run": return { tag: "命令", label: "命令行" };
    case "read": return { tag: "读取", label: "读取内容" };
    case "search": return { tag: "检索", label: "搜索内容" };
    case "edit": return { tag: "编辑", label: "编辑文件" };
    case "write": return { tag: "写入", label: "写入文件" };
    default: return { tag: "工具", label: "工具调用" };
  }
}

// ---------- 文件变更 ----------

const CHANGE_LABEL: Record<FileChange["change_type"], string> = {
  create: "新建",
  modify: "修改",
  delete: "删除",
  rename: "重命名",
};

function fileRows(changes: readonly FileChange[]): AuditRow[] {
  return changes.map((change) => ({
    id: `file-${change.id}`,
    at: parseMs(change.created_at),
    atIso: change.created_at,
    kind: "file" as const,
    tag: "文件",
    text: `${CHANGE_LABEL[change.change_type]} · ${shortTarget(change.path)}`,
    result: null,
    state: "info" as const,
    title: change.old_path ? `${change.old_path}\n→ ${change.path}` : change.path,
  }));
}

// ---------- 验证 ----------

function verificationRows(verifications: readonly VerificationRecord[]): AuditRow[] {
  return verifications.map((record) => {
    const spent =
      record.ended_at != null
        ? formatDuration((parseMs(record.ended_at) ?? 0) - (parseMs(record.started_at) ?? 0))
        : null;
    let state: AuditState = "info";
    let result: string;
    switch (record.status) {
      case "passed":
        state = "ok";
        result = spent ? `✓ 通过 · ${spent}` : "✓ 通过";
        break;
      case "failed":
        state = "fail";
        result = `✗ exit ${record.exit_code ?? "?"}`;
        break;
      case "timeout":
        state = "fail";
        result = "✗ 超时";
        break;
      case "running":
        state = "wait";
        result = "运行中";
        break;
      case "stale":
        result = "已过期";
        break;
      default:
        result = "已被替代";
        break;
    }
    return {
      id: `verify-${record.id}`,
      at: parseMs(record.started_at),
      atIso: record.started_at,
      kind: "verify" as const,
      tag: "验证",
      text: cut(record.command, 48),
      result,
      state,
      title: record.command,
    };
  });
}

// ---------- 权限 ----------

function permissionRows(permissions: readonly PermissionRequest[]): AuditRow[] {
  return permissions
    .filter((permission) => permission.decision === "pending")
    .map((permission) => ({
      id: `perm-${permission.id}`,
      at: parseMs(permission.created_at),
      atIso: permission.created_at,
      kind: "permission" as const,
      tag: "权限",
      text: `${permission.tool_name} · ${permissionRiskLabel(permission.risk_level)}`,
      result: "等待批准",
      state: "wait" as const,
      title: permission.input_summary,
    }));
}

// ---------- 子代理 ----------

function subagentRows(runs: readonly AgentRun[]): AuditRow[] {
  return runs
    .filter((run) => run.agent_kind === "subagent")
    .map((run) => {
      const ended = run.ended_at != null;
      let state: AuditState = "wait";
      let result = "运行中";
      if (ended) {
        if (run.review_state === "failed") {
          state = "fail";
          result = "✗ 执行失败";
        } else if (run.review_state === "aborted" || run.review_state === "rolled_back") {
          state = "fail";
          result = "✗ 已中止";
        } else {
          state = "ok";
          const spent = formatDuration(
            (parseMs(run.ended_at) ?? 0) - (parseMs(run.started_at) ?? 0)
          );
          result = spent ? `✓ 完成 · ${spent}` : "✓ 完成";
        }
      }
      const iso = run.ended_at ?? run.started_at;
      return {
        id: `run-${run.id}`,
        at: parseMs(iso),
        atIso: iso,
        kind: "agent" as const,
        tag: "子代理",
        text: run.agent_label?.trim() || "只读调查",
        result,
        state,
        title: run.summary ?? run.model,
      };
    });
}

// ---------- 自解释事件 ----------

function eventRows(events: readonly TaskEvent[]): AuditRow[] {
  const rows: AuditRow[] = [];
  for (const event of events) {
    const shape = SELF_EXPLANATORY_EVENTS[event.event_type];
    if (!shape) continue;
    rows.push({
      id: `event-${event.id}`,
      at: parseMs(event.created_at),
      atIso: event.created_at,
      kind: shape.kind,
      tag: shape.tag,
      text: shape.text,
      result: null,
      state: shape.state,
    });
  }
  return rows;
}

// ---------- 运行简报聚合 ----------

/**
 * 工具调用构成 —— 从会话 JSONL 的 tool_call 按动词聚合，排除协调类工具
 * （delegate/wait 等不产生面板上可读的"操作"）。用于把平铺的读取/检索噪声
 * 压缩成一条构成条，总数直接来自消息流，不受审计条数上限影响。
 */
export function summarizeToolComposition(messages: readonly SessionMessage[]): ToolComposition {
  const counts: ToolComposition = { read: 0, command: 0, search: 0, edit: 0, write: 0, other: 0, total: 0 };
  for (const message of messages) {
    if (message.kind !== "tool_call") continue;
    const name = (message.tool_name ?? "").trim();
    if (!name || isCoordinationTool(name)) continue;
    switch (toolVerb(name)) {
      case "run": counts.command += 1; break;
      case "read": counts.read += 1; break;
      case "search": counts.search += 1; break;
      case "edit": counts.edit += 1; break;
      case "write": counts.write += 1; break;
      default: counts.other += 1;
    }
    counts.total += 1;
  }
  return counts;
}

/** 常规后台动作（成功/进行中的读取与检索）：折叠进构成条，不占关键事件时间线。 */
export function isRoutineAuditRow(row: AuditRow): boolean {
  return row.kind === "tool" && row.state !== "fail" && (row.tag === "读取" || row.tag === "检索");
}

/**
 * 关键事件 = 审计流里值得逐条看的拐点：命令、编辑、写入、文件变更、验证、
 * 权限、子代理起落、会话标记，以及任何失败行。常规读取/检索由调用方折叠计数。
 */
export function buildKeyEvents(rows: readonly AuditRow[], limit = 7): AuditRow[] {
  return rows.filter((row) => !isRoutineAuditRow(row)).slice(0, limit);
}

/**
 * 活动火花线 —— 把 task_events 的时间戳按会话时间跨度分桶，返回 0..1 归一化
 * 强度序列（长度 = buckets）。无事件或跨度不足一分钟时返回空数组。
 */
export function activityBuckets(events: readonly TaskEvent[], buckets = 12): number[] {
  const stamps = events
    .map((event) => Date.parse(event.created_at))
    .filter((at) => !Number.isNaN(at))
    .sort((a, b) => a - b);
  if (stamps.length === 0) return [];
  const start = stamps[0];
  const end = stamps[stamps.length - 1];
  if (end - start < 60_000) return [];
  const counts = new Array<number>(buckets).fill(0);
  for (const at of stamps) {
    const index = Math.min(buckets - 1, Math.floor(((at - start) / (end - start)) * buckets));
    counts[index] += 1;
  }
  const peak = Math.max(...counts);
  return peak > 0 ? counts.map((count) => count / peak) : [];
}

// ---------- 工具函数 ----------

function parseMs(iso: string | null | undefined): number | null {
  if (!iso) return null;
  const parsed = Date.parse(iso);
  return Number.isNaN(parsed) ? null : parsed;
}

function cut(value: string, max: number): string {
  const text = value.trim().replace(/\s+/g, " ");
  return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}

/** 路径只保留「父目录/文件名」，完整路径进 title。 */
function shortTarget(value: string): string {
  const normalized = value.trim();
  if (!normalized) return "";
  if (!/[\\/]/.test(normalized)) return cut(normalized, 44);
  const parts = normalized.split(/[\\/]/).filter(Boolean);
  const tail = parts.slice(-2).join("/");
  return cut(parts.length > 2 ? `…/${tail}` : tail, 44);
}

// 时长格式化唯一实现在 lib/format.ts（F-maint-04 收敛；语义不变）。
function formatDuration(ms: number): string {
  return formatDurationMs(ms);
}
