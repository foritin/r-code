/**
 * Room 场景数据模型 —— 纯函数，不依赖 React。
 * - buildTimeline：sessionMessages（会话流）+ task events（带时间戳的工具锚点）+ AgentRun 合并为时间线条目。
 *   会话消息除 meta 外无时间戳：tool_call/tool_result 按顺序与 TaskEvent 对齐取 created_at，
 *   其余消息取「最近已知时间」（单调不减），供回放播放头调暗（data-t）用。
 */
import type {
  AgentEvent,
  AgentSendMode,
  AgentRun,
  QueuedMessage,
  QueuedMessageState,
  SessionMessage,
  TaskEvent,
} from "../../lib/types";
import { toolTarget } from "../../lib/format";

export interface PlanStep {
  description: string;
  completed: boolean;
}

export type ToolState = "active" | "ok" | "fail";

export type RunViewState = "active" | "finished" | "aborted" | "accepted" | "answered" | "failed";

export interface RunPresentation {
  state: RunViewState;
  label: string;
}

/** AgentRun 是运行呈现的唯一真源；任务事件继续保留给审计和时间锚点。 */
export function runPresentation(run: AgentRun): RunPresentation {
  if (run.ended_at == null) return { state: "active", label: "运行中" };
  switch (run.review_state) {
    case "rolled_back":
    case "aborted":
      return { state: "aborted", label: "已中止" };
    case "accepted":
    case "auto_accepted":
      return { state: "accepted", label: "已接受" };
    case "answered":
      return { state: "answered", label: "已答复" };
    case "failed":
      return { state: "failed", label: "执行失败" };
    case "pending":
      return { state: "finished", label: "已完成" };
    default:
      return { state: "finished", label: "已完成" };
  }
}

export type TimelineItem =
  | { kind: "ms"; id: string; t: number; ok: boolean | null; label: string }
  | {
      kind: "you";
      id: string;
      t: number;
      text: string;
      messageId?: string;
      sendMode: AgentSendMode;
      queuedState?: QueuedMessageState;
      queueId?: string;
    }
  | { kind: "agent"; id: string; t: number; text: string; streaming: boolean }
  | { kind: "plan"; id: string; t: number; steps: PlanStep[] }
  | {
      kind: "run";
      id: string;
      t: number;
      model: string;
      agentKind: AgentRun["agent_kind"];
      agentLabel: string | null;
      agentSummary: string | null;
      runtimeKind: AgentRun["runtime_kind"];
      startedAt: string;
      endedAt: string | null;
      usageJson: string | null;
      state: RunViewState;
      label: string;
    }
  | {
      kind: "tool";
      id: string;
      t: number;
      callId: string | null;
      name: string;
      target: string;
      state: ToolState;
      summary: string;
      /** 完整调用输入（原始 JSON 串）；展开卡片用，折叠态不读。 */
      inputJson: string | null;
      /** 完整调用输出（原始 JSON 串）；tool_result 到达前为 null。 */
      outputJson: string | null;
    };

/** RFC3339 → 相对会话起点的秒数（无法解析时回退 0，不为负）。 */
export function relSec(iso: string | null | undefined, startMs: number): number {
  if (!iso) return 0;
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return 0;
  return Math.max(0, (t - startMs) / 1000);
}

/** tool_result 输出 → 单行摘要。 */
export function summarizeOutput(outputJson: string | null | undefined, isError: boolean): string {
  const pick = (v: unknown): string => {
    if (typeof v === "string") return v;
    if (v && typeof v === "object") {
      const o = v as Record<string, unknown>;
      for (const k of ["content", "text", "stdout", "output", "message", "error"]) {
        const s = o[k];
        if (typeof s === "string" && s) return s;
      }
      try {
        return JSON.stringify(v);
      } catch {
        return String(v);
      }
    }
    return v == null ? "" : String(v);
  };
  let raw = "";
  if (outputJson) {
    try {
      raw = pick(JSON.parse(outputJson));
    } catch {
      raw = outputJson;
    }
  }
  const firstLine = raw.split("\n").find((l) => l.trim().length > 0) ?? "";
  const body = firstLine.trim();
  const cut = body.length > 72 ? body.slice(0, 71) + "…" : body;
  if (!cut) return isError ? "error" : "done";
  return cut;
}

function safeStringify(v: unknown): string {
  try {
    return JSON.stringify(v) ?? "";
  } catch {
    return String(v);
  }
}

/** 工具卡片展开态的载荷视图。 */
export interface ToolPayloadView {
  /** 展示用文本（已美化 / 已解包 / 已截断）。 */
  text: string;
  /** 语法高亮语言标识；null = 按纯文本渲染。 */
  lang: string | null;
  /** 行数，供折叠阈值判定。 */
  lines: number;
  /** 是否因超长被截断。 */
  truncated: boolean;
}

/** 单个载荷的展示上限：再大的输出在气泡里也没有阅读价值，且会拖垮 DOM。 */
const MAX_PAYLOAD_CHARS = 20_000;

/** 命令类工具的输入里，这些键之外的内容才值得再单独摊开成 JSON。 */
const COMMAND_KEYS = ["command", "cmd"];

/**
 * 工具调用的原始 JSON → 可读载荷。
 *
 * 三条取舍：
 * - shell 类输入直接还原成命令行并按 bash 高亮，比 `{"command":"ls -la"}` 有用得多；
 * - 只裹了一层文本字段的输出（content/stdout/...）解包成裸文本，不然满屏都是转义换行；
 * - 其余一律美化成 JSON，好过原始单行串。
 */
export function formatToolPayload(
  raw: string | null | undefined,
  role: "input" | "output"
): ToolPayloadView | null {
  if (raw == null) return null;

  // 快速通道：远超展示上限的载荷不值得先 parse 再 stringify 再截断
  // （14MB 原始串实测 158ms，全在主线程、就发生在用户点击那一刻）。
  // 判据用 raw.length 而非 trimmed —— trim() 本身在 14MB 上就要几十毫秒。
  if (raw.length > MAX_PAYLOAD_CHARS * 8) {
    const head = raw.slice(0, MAX_PAYLOAD_CHARS * 2).trimStart();
    const text = clampChars(head, MAX_PAYLOAD_CHARS);
    return { text, lang: null, lines: text.split("\n").length, truncated: true };
  }

  const trimmed = raw.trim();
  if (!trimmed) return null;

  let text = trimmed;
  let lang: string | null = null;

  let parsed: unknown;
  let parsedOk = true;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    parsedOk = false;
  }

  if (parsedOk) {
    if (typeof parsed === "string") {
      text = parsed;
    } else if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      const obj = parsed as Record<string, unknown>;
      const keys = Object.keys(obj);
      const cmdKey = role === "input" ? COMMAND_KEYS.find((k) => typeof obj[k] === "string") : undefined;

      if (cmdKey) {
        const rest = keys.filter((k) => k !== cmdKey);
        text = String(obj[cmdKey]);
        lang = "bash";
        if (rest.length > 0) {
          const extras: Record<string, unknown> = {};
          for (const k of rest) extras[k] = obj[k];
          // 命令之外还有参数（cwd、timeout…）：附在下面，别静默吞掉。
          text += `\n\n# 其余参数\n# ${safeStringify(extras)}`;
        }
      } else {
        // 只裹了一层文本字段 → 解包。**仅对输出生效**：输入里 {"path":"a.rs"} 解包成
        // "a.rs" 会把参数名丢掉，看的人分不清那是 path 还是 pattern。
        const textKey =
          role === "output"
            ? keys.length === 1 && typeof obj[keys[0]] === "string"
              ? keys[0]
              : ["content", "text", "stdout", "output", "message", "error"].find(
                  (k) => typeof obj[k] === "string" && keys.length <= 2
                )
            : undefined;
        if (textKey) {
          text = String(obj[textKey]);
          const rest = keys.filter((k) => k !== textKey);
          if (rest.length > 0) {
            const extras: Record<string, unknown> = {};
            for (const k of rest) extras[k] = obj[k];
            // 解包不等于丢弃：附带字段（exit_code、truncated…）照样要露出来。
            text += `\n\n---\n${safeStringify(extras)}`;
          }
        } else {
          text = prettyJson(parsed, trimmed);
          lang = "json";
        }
      }
    } else {
      text = prettyJson(parsed, trimmed);
      lang = "json";
    }
  }

  const truncated = text.length > MAX_PAYLOAD_CHARS;
  if (truncated) text = clampChars(text, MAX_PAYLOAD_CHARS);
  const lines = text.length === 0 ? 0 : text.split("\n").length;
  return { text, lang, lines, truncated };
}

/**
 * JSON.parse 在 V8 是迭代的（能吃下任意深度），JSON.stringify 是递归的。
 * 深嵌套载荷会在这里抛 RangeError —— 而这发生在 useMemo 里（渲染期），
 * 应用又没有 ErrorBoundary，后果是整树卸载白屏。必须兜住。
 */
function prettyJson(value: unknown, fallback: string): string {
  try {
    return JSON.stringify(value, null, 2) ?? fallback;
  } catch {
    return fallback;
  }
}

/** 按字符数截断，但不切断代理对（否则末尾会渲染成 �）。 */
function clampChars(s: string, max: number): string {
  const cut = s.slice(0, max);
  const last = cut.charCodeAt(cut.length - 1);
  // 高位代理落在边界上 → 后半截已被切走，整个丢掉这一个码元。
  return last >= 0xd800 && last <= 0xdbff ? cut.slice(0, -1) : cut;
}

/** 合并会话消息、任务事件的工具时间锚点与每次 AgentRun 为时间线条目。 */
export function buildTimeline(
  messages: SessionMessage[],
  events: TaskEvent[],
  runs: AgentRun[],
  startIso: string,
  queuedMessages: QueuedMessage[] = []
): TimelineItem[] {
  const startMs = Date.parse(startIso);
  const anchor = Number.isNaN(startMs) ? Date.now() : startMs;
  const sorted = [...events].sort((a, b) => a.id - b.id);
  const callTs = sorted.filter((e) => e.event_type === "tool_call").map((e) => e.created_at);
  const resultTs = sorted.filter((e) => e.event_type === "tool_result").map((e) => e.created_at);
  let callIdx = 0;
  let resultIdx = 0;

  const items: TimelineItem[] = [];
  let lastT = 0;
  let seq = 0;
  let nextUserSendMode: AgentSendMode | null = null;
  const nid = (p: string) => `${p}-${seq++}`;

  for (const m of messages) {
    switch (m.kind) {
      case "meta":
        break; // 会话元信息不进时间线（胶片 meta 行展示）
      case "message": {
        const text = (m.text ?? "").trim();
        if (!text) break;
        if (m.role === "user") {
          items.push({
            kind: "you",
            id: m.id ? `msg-${m.id}` : nid("you"),
            t: lastT,
            text,
            messageId: m.id,
            sendMode: nextUserSendMode ?? "auto",
          });
          nextUserSendMode = null;
        } else {
          items.push({
            kind: "agent",
            id: m.id ? `msg-${m.id}` : nid("ag"),
            t: lastT,
            text,
            streaming: false,
          });
        }
        break;
      }
      case "system": {
        if (m.text === "r_code_user_message_mode") {
          nextUserSendMode = parseUserSendMode(m.output_json) ?? "auto";
        } else if (m.text === "plan" && m.output_json) {
          try {
            const data = JSON.parse(m.output_json) as { steps?: PlanStep[] };
            if (Array.isArray(data.steps)) {
              items.push({ kind: "plan", id: nid("pl"), t: lastT, steps: data.steps });
            }
          } catch {
            /* 无法解析的 plan 负载直接跳过 */
          }
        } else if (m.text) {
          items.push({ kind: "ms", id: nid("ms"), t: lastT, ok: null, label: m.text });
        }
        break;
      }
      case "tool_call": {
        const ts = callTs[callIdx++];
        if (ts) lastT = Math.max(lastT, relSec(ts, anchor));
        items.push({
          kind: "tool",
          // key 必须跨「流式 → 历史重建」保持稳定：卡片现在持有展开态，
          // 位置序号做 key 会让每次 reload 都 remount，用户看到一半的输出自己收起来。
          id: m.call_id ? `tool-${m.call_id}` : nid("tc"),
          t: lastT,
          callId: m.call_id ?? null,
          name: m.tool_name ?? "tool",
          target: toolTarget(m.input_json),
          state: "active",
          summary: "",
          inputJson: m.input_json ?? null,
          outputJson: null,
        });
        break;
      }
      case "tool_result": {
        const ts = resultTs[resultIdx++];
        if (ts) lastT = Math.max(lastT, relSec(ts, anchor));
        const summary = summarizeOutput(m.output_json, m.is_error === true);
        const state: ToolState = m.is_error === true ? "fail" : "ok";
        const row = [...items].reverse().find(
          (it): it is Extract<TimelineItem, { kind: "tool" }> =>
            it.kind === "tool" && it.callId != null && it.callId === m.call_id && it.state === "active"
        );
        if (row) {
          row.state = state;
          row.summary = summary;
          row.outputJson = m.output_json ?? null;
        } else {
          items.push({
            kind: "tool",
            id: m.call_id ? `toolr-${m.call_id}` : nid("tr"),
            t: lastT,
            callId: m.call_id ?? null,
            name: "result",
            target: "",
            state,
            summary,
            inputJson: null,
            outputJson: m.output_json ?? null,
          });
        }
        break;
      }
    }
  }

  for (const queued of queuedMessages) {
    const sendMode = queueSendMode(queued);
    if (
      items.some(
        (item) =>
          item.kind === "you" && item.text === queued.message && item.sendMode === sendMode
      )
    ) {
      continue;
    }
    const t = Math.max(lastT, relSec(queued.created_at, anchor));
    lastT = t;
    items.push({
      kind: "you",
      id: `queued-${queued.id}`,
      t,
      text: queued.message,
      sendMode,
      queuedState: queued.state,
      queueId: queued.id,
    });
  }

  return mergeRunItems(items, runs, anchor);
}

function parseUserSendMode(value: string | null | undefined): AgentSendMode | null {
  if (!value) return null;
  try {
    const payload = JSON.parse(value) as { mode?: unknown };
    switch (payload.mode) {
      case "auto":
      case "steer":
      case "queue":
      case "send_now":
        return payload.mode;
      default:
        return null;
    }
  } catch {
    return null;
  }
}

/** 当前持久化队列用优先级区分“普通排队”和“立即发送”的中止后优先分发。 */
function queueSendMode(message: QueuedMessage): AgentSendMode {
  return message.priority >= 1_000_000 ? "send_now" : "queue";
}

/**
 * 把 AgentRun 合并进时间线。
 *
 * 这里不能按时间戳排序。会话消息**没有**时间戳 —— `session_messages`
 * （commands.rs）把 `SessionEvent::Message` 转成 `SessionMessage` 时固定写
 * `timestamp: None`，只有 Meta 有值。所以 buildTimeline 里条目的 `t` 只是
 * 一个"被工具调用推进的序号"，而 `run.started_at` 是真实墙钟时间。
 *
 * 旧实现按 `t` 大小插入，两个不同量纲混在一起：纯问答（无工具调用）的会话里
 * 所有消息 `t === 0`、所有 run `t > 0`，于是每一条 run 行都被推到列表末尾，
 * N 条"运行 X 已完成"连成一片堆在底部，看起来像重复记录。
 *
 * 正确的锚点是**轮次边界**：一轮 = 一条用户消息 + 其后的 agent 输出，
 * 一次运行收尾于该轮末尾。轮次边界可以直接从条目序列里读出来，不需要时间戳。
 */
export function mergeRunItems(
  items: TimelineItem[],
  runs: AgentRun[],
  startMs: number
): TimelineItem[] {
  const withoutRuns = items.filter((item) => item.kind !== "run");
  const ordered = orderedUniqueRuns(runs);
  if (ordered.length === 0) return withoutRuns;

  // 轮次边界：一条用户消息，且它前面已经出现过非用户条目 → 新一轮开始。
  // 末尾再补一个边界，承载最后一轮（以及所有溢出的 run）。
  const boundaries: number[] = [];
  let sawResponse = false;
  withoutRuns.forEach((item, index) => {
    if (item.kind === "you") {
      if (sawResponse) {
        boundaries.push(index);
        sawResponse = false;
      }
    } else {
      sawResponse = true;
    }
  });
  boundaries.push(withoutRuns.length);
  const lastSlot = boundaries.length - 1;

  // 主 run 按顺序落到各轮边界；运行中的那条永远在末尾（spinner 必须跟着最新内容）。
  // run 数多于轮数时（例如 send_now 打断产生的"已中止 + 已完成"）多出来的堆到末尾，
  // 这与旧行为一致，不会更糟。
  const slotOf = new Map<string, number>();
  let mainIndex = 0;
  for (const run of ordered) {
    if (run.agent_kind === "subagent") continue;
    const slot = run.ended_at == null ? lastSlot : Math.min(mainIndex, lastSlot);
    slotOf.set(run.id, slot);
    mainIndex++;
  }
  // 子代理跟随父 run 所在的轮次；找不到父就归到末尾。
  for (const run of ordered) {
    if (run.agent_kind !== "subagent") continue;
    const parentSlot = run.parent_run_id ? slotOf.get(run.parent_run_id) : undefined;
    slotOf.set(run.id, parentSlot ?? lastSlot);
  }

  const buckets = new Map<number, AgentRun[]>();
  for (const run of ordered) {
    const slot = slotOf.get(run.id) ?? lastSlot;
    const list = buckets.get(slot);
    if (list) list.push(run);
    else buckets.set(slot, [run]);
  }
  // 桶内顺序：子代理在前（它们先于父 run 结束），主 run 收尾。
  for (const list of buckets.values()) {
    list.sort((a, b) => {
      const ak = a.agent_kind === "subagent" ? 0 : 1;
      const bk = b.agent_kind === "subagent" ? 0 : 1;
      return ak - bk || a.started_at.localeCompare(b.started_at);
    });
  }

  const merged: TimelineItem[] = [...withoutRuns];
  // 从后往前插入，前面的下标才不会被位移。
  for (const slot of [...buckets.keys()].sort((a, b) => b - a)) {
    const at = boundaries[slot];
    const list = buckets.get(slot) ?? [];
    // t 沿用前一条的值：它只服务于回放游标的 dim() 比较，
    // 用 run 的墙钟秒数会让同一轮里的条目 t 跳变。
    const anchorT = at > 0 ? merged[at - 1]?.t ?? 0 : relSec(list[0].started_at, startMs);
    merged.splice(
      at,
      0,
      ...list.map((run) => ({
        kind: "run" as const,
        id: `run-${run.id}`,
        t: anchorT,
        model: run.model,
        agentKind: run.agent_kind,
        agentLabel: run.agent_label,
        agentSummary: run.summary,
        runtimeKind: run.runtime_kind,
        startedAt: run.started_at,
        endedAt: run.ended_at,
        usageJson: run.usage_json,
        ...runPresentation(run),
      }))
    );
  }
  return merged;
}

function orderedUniqueRuns(runs: AgentRun[]): AgentRun[] {
  const seen = new Set<string>();
  return [...runs]
    .sort((a, b) => a.started_at.localeCompare(b.started_at) || a.id.localeCompare(b.id))
    .filter((run) => {
      if (seen.has(run.id)) return false;
      seen.add(run.id);
      return true;
    });
}

/** 将一条流式 AgentEvent 应用到时间线（返回新数组）。nowSec 由调用方提供。 */
export function applyAgentEvent(
  prev: TimelineItem[],
  ev: AgentEvent,
  nowSec: number,
  nid: () => string
): TimelineItem[] {
  const items = [...prev];
  const last = items[items.length - 1];
  switch (ev.type) {
    case "message": {
      if (last?.kind === "agent" && last.streaming) {
        items[items.length - 1] = { ...last, text: last.text + ev.text, streaming: ev.delta };
      } else {
        items.push({ kind: "agent", id: nid(), t: nowSec, text: ev.text, streaming: ev.delta });
      }
      return items;
    }
    case "tool_call": {
      let inputJson: string | undefined;
      try {
        inputJson = JSON.stringify(ev.input);
      } catch {
        inputJson = undefined;
      }
      items.push({
        kind: "tool",
        // 与 buildTimeline 同一套 key，重建后卡片不会 remount（展开态得以保留）。
        id: ev.call_id ? `tool-${ev.call_id}` : nid(),
        t: nowSec,
        callId: ev.call_id,
        name: ev.name,
        target: toolTarget(inputJson),
        state: "active",
        summary: "",
        inputJson: inputJson ?? null,
        outputJson: null,
      });
      return items;
    }
    case "tool_result": {
      // 后端存的是 `Value::to_string()`（commands.rs），即**永远是 JSON 编码文本**。
      // 这里若对字符串走裸串分支，流式与历史重建两条路径的编码就不一致：
      // 输出本身是合法 JSON 文本时（cat package.json、MCP 返回 JSON blob），
      // 流式会当 JSON 高亮、重建后退化成单行原始串，run 结束瞬间肉眼可见跳变。
      const outputJson = safeStringify(ev.output);
      const summary = summarizeOutput(outputJson, ev.is_error);
      for (let i = items.length - 1; i >= 0; i--) {
        const it = items[i];
        if (it.kind === "tool" && it.callId === ev.call_id && it.state === "active") {
          items[i] = { ...it, state: ev.is_error ? "fail" : "ok", summary, outputJson };
          return items;
        }
      }
      items.push({
        kind: "tool",
        id: ev.call_id ? `toolr-${ev.call_id}` : nid(),
        t: nowSec,
        callId: ev.call_id,
        name: "result",
        target: "",
        state: ev.is_error ? "fail" : "ok",
        summary,
        inputJson: null,
        outputJson,
      });
      return items;
    }
    case "plan": {
      for (let i = items.length - 1; i >= 0; i--) {
        const it = items[i];
        if (it.kind === "plan") {
          items[i] = { ...it, steps: ev.steps };
          return items;
        }
      }
      items.push({ kind: "plan", id: nid(), t: nowSec, steps: ev.steps });
      return items;
    }
    case "activity":
      // 活动条消费该事件；时间线不展示内部活动或推理文本。
      return items;
    case "scoped":
    case "subagent_lifecycle":
      // 子代理详情由 Working 列表和持久化运行树呈现；主时间线默认保持折叠。
      return items;
    case "state":
      return items; // state 事件由订阅方触发 refresh + 历史重建
  }
}

// ---------- 终端输出 ----------

/* eslint-disable no-control-regex */
const ANSI_RE =
  /\x1B(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1B\\)|\([0-9A-B]|[@-_])/g;

/** 剥离 ANSI 转义序列（CSI/OSC/单字符），CR 归一化 —— 终端输出以纯文本 <pre> 展示。 */
export function stripAnsi(s: string): string {
  return s
    .replace(ANSI_RE, "")
    .replace(/\x07/g, "")
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "");
}
