/**
 * 把持久化时间线整理成产品级事件流。
 *
 * 底层仍保留完整 tool / run / system 记录供审计使用；这里负责普通对话里的渐进披露：
 * - 主运行变成轮次摘要；
 * - 编排协议折叠为一组子代理状态芯片；
 * - 相邻命令与文件操作归成可展开的活动；
 * - 原始协议名和 JSON 永不直接进入产品时间线。
 */
import type { TimelineItem } from "./model";
import { toolActivityKind, type ToolActivityKind } from "./tool-activity";

export type TimelineUserItem = Extract<TimelineItem, { kind: "you" }>;
export type TimelineRunItem = Extract<TimelineItem, { kind: "run" }>;
export type TimelineToolItem = Extract<TimelineItem, { kind: "tool" }>;
export type TimelineBaseDisplayItem = Exclude<TimelineItem, { kind: "you" | "run" | "tool" }>;

export type TimelineToolGroupKind = ToolActivityKind;

export interface TimelineToolGroupItem {
  kind: "tool_group";
  id: string;
  t: number;
  groupKind: TimelineToolGroupKind;
  tools: TimelineToolItem[];
}

export type TimelineSubagentStatus =
  | "queued"
  | "running"
  | "waiting_permission"
  | "completed"
  | "failed"
  | "cancelled";

export interface TimelineSubagentEntry {
  id: string;
  runId: string | null;
  label: string;
  summary: string | null;
  model: string | null;
  runtimeKind: TimelineRunItem["runtimeKind"];
  status: TimelineSubagentStatus;
}

export interface TimelineSubagentGroupItem {
  kind: "subagent_group";
  id: string;
  t: number;
  agents: TimelineSubagentEntry[];
}

export type TimelineDisplayItem =
  | TimelineBaseDisplayItem
  | TimelineToolGroupItem
  | TimelineSubagentGroupItem;

export interface TimelineTurn {
  id: string;
  user: TimelineUserItem | null;
  runs: TimelineRunItem[];
  items: TimelineDisplayItem[];
  hasActivity: boolean;
}

interface RawTurn {
  id: string;
  user: TimelineUserItem | null;
  body: TimelineItem[];
}

interface IndexedTurn {
  id: string;
  user: TimelineUserItem | null;
  start: number;
  end: number;
  visible: boolean;
  presented?: TimelineTurn | null;
}

export interface TimelineWindow {
  turns: TimelineTurn[];
  totalTurns: number;
}

export function buildTimelineTurns(items: readonly TimelineItem[]): TimelineTurn[] {
  const turns: RawTurn[] = [];
  let current: RawTurn = { id: "timeline-preamble", user: null, body: [] };

  for (const item of items) {
    if (item.kind === "you") {
      if (current.user || current.body.length > 0) turns.push(current);
      current = { id: `turn-${item.id}`, user: item, body: [] };
    } else {
      current.body.push(item);
    }
  }
  if (current.user || current.body.length > 0) turns.push(current);

  return turns
    .map(presentRawTurn)
    .filter((turn): turn is TimelineTurn => turn != null);
}

/**
 * 主对话的增量呈现索引。完整历史只在 reload 时做一次轻量边界扫描；昂贵的工具
 * 归组、子代理解析与展示对象分配仅发生在当前挂载窗口。流式 token 通常只让最后
 * 一轮的缓存失效，已完成的历史轮次保持引用稳定。
 */
export class TimelinePresentationCache {
  private items: readonly TimelineItem[] = [];
  private itemCount = 0;
  private turns: IndexedTurn[] = [];
  private visibleCount = 0;

  reset(items: readonly TimelineItem[]): void {
    this.items = items;
    this.itemCount = items.length;
    this.turns = indexTurns(items);
    this.visibleCount = this.turns.reduce((count, turn) => count + Number(turn.visible), 0);
  }

  /** 更新由流式 reducer 报告的最早变化点；不重新扫描无关历史。 */
  update(items: readonly TimelineItem[], startIndex: number): void {
    if (items !== this.items || items.length < this.itemCount || startIndex < 0) {
      if (items !== this.items || items.length < this.itemCount) this.reset(items);
      return;
    }

    const previousCount = this.itemCount;
    this.itemCount = items.length;
    if (this.turns.length === 0) {
      this.turns = indexTurns(items);
      this.visibleCount = this.turns.reduce((count, turn) => count + Number(turn.visible), 0);
      return;
    }

    if (items.length >= previousCount) this.rebuildSuffix(startIndex);
  }

  window(limit: number): TimelineWindow {
    const take = Math.max(0, Math.floor(limit));
    const visible: TimelineTurn[] = [];
    for (let index = this.turns.length - 1; index >= 0 && visible.length < take; index -= 1) {
      const source = this.turns[index];
      if (!source.visible) continue;
      if (source.presented === undefined) source.presented = presentIndexedTurn(source, this.items);
      if (source.presented) visible.push(source.presented);
    }
    visible.reverse();
    return { turns: visible, totalTurns: this.visibleCount };
  }

  private turnIndexAt(itemIndex: number): number {
    let low = 0;
    let high = this.turns.length - 1;
    while (low <= high) {
      const middle = (low + high) >>> 1;
      const turn = this.turns[middle];
      if (itemIndex < turn.start) high = middle - 1;
      else if (itemIndex >= turn.end) low = middle + 1;
      else return middle;
    }
    return Math.max(0, Math.min(this.turns.length - 1, low));
  }

  private rebuildSuffix(startIndex: number): void {
    // startIndex 可能同时覆盖旧流式光标和新追加条目（例如 steer 后开始新答复）。
    // 保留它之前的完整轮次，只重建受影响的短后缀，既避免历史全扫也不会留下旧 caret。
    const firstTurn = this.turnIndexAt(startIndex);
    const suffixStart = this.turns[firstTurn]?.start ?? 0;
    const removed = this.turns.splice(firstTurn);
    for (const turn of removed) this.visibleCount -= Number(turn.visible);
    const appended = indexTurns(this.items, suffixStart);
    this.turns.push(...appended);
    for (const turn of appended) this.visibleCount += Number(turn.visible);
  }

}

function indexTurns(items: readonly TimelineItem[], start = 0): IndexedTurn[] {
  if (start >= items.length) return [];
  const turns: IndexedTurn[] = [];
  let turnStart = start;
  let user: TimelineUserItem | null = items[start]?.kind === "you" ? items[start] : null;
  let cursor = user ? start + 1 : start;

  for (; cursor < items.length; cursor += 1) {
    const item = items[cursor];
    if (item.kind !== "you") continue;
    const turn = indexedTurn(items, turnStart, cursor, user);
    if (turn) turns.push(turn);
    turnStart = cursor;
    user = item;
  }
  const tail = indexedTurn(items, turnStart, items.length, user);
  if (tail) turns.push(tail);
  return turns;
}

function indexedTurn(
  items: readonly TimelineItem[],
  start: number,
  end: number,
  user: TimelineUserItem | null,
): IndexedTurn | null {
  if (!user && end <= start) return null;
  const turn: IndexedTurn = {
    id: user ? `turn-${user.id}` : "timeline-preamble",
    user,
    start,
    end,
    visible: false,
  };
  turn.visible = rawTurnWillRender(items, turn);
  return turn;
}

function rawTurnWillRender(items: readonly TimelineItem[], turn: IndexedTurn): boolean {
  if (turn.user && !composerOwnsQueuedMessage(turn.user)) return true;
  const bodyStart = turn.user ? turn.start + 1 : turn.start;
  let hasChildRun = false;
  let hasDelegateTool = false;
  for (let index = bodyStart; index < turn.end; index += 1) {
    const item = items[index];
    if (item.kind === "run") {
      if (item.agentKind === "main") return true;
      hasChildRun = true;
      continue;
    }
    if (item.kind === "tool") {
      const protocol = protocolToolKind(item.name);
      if (!protocol) return true;
      if (protocol === "delegate") hasDelegateTool = true;
      continue;
    }
    if (item.kind === "ms" && isInternalProtocolLabel(item.label)) continue;
    return true;
  }
  return hasChildRun || hasDelegateTool;
}

function presentIndexedTurn(turn: IndexedTurn, items: readonly TimelineItem[]): TimelineTurn | null {
  const bodyStart = turn.user ? turn.start + 1 : turn.start;
  return presentRawTurn({
    id: turn.id,
    user: turn.user,
    body: items.slice(bodyStart, turn.end),
  });
}

function presentRawTurn(turn: RawTurn): TimelineTurn | null {
  const presented = presentTurn(turn);
  const normalized = composerOwnsQueuedMessage(presented.user)
    ? { ...presented, user: null }
    : presented;
  return normalized.user || normalized.runs.length || normalized.items.length ? normalized : null;
}

/**
 * Queued messages belong to the reorderable composer queue until dispatch succeeds.
 * Rendering them in the conversation at the same time duplicates the same content and
 * makes queue reordering look like it rewrites chat history.
 */
function composerOwnsQueuedMessage(user: TimelineUserItem | null): boolean {
  return user?.queuedState === "queued"
    || user?.queuedState === "dispatching"
    || user?.queuedState === "failed";
}

function presentTurn(turn: RawTurn): TimelineTurn {
  const runs = turn.body.filter(
    (item): item is TimelineRunItem => item.kind === "run" && item.agentKind === "main"
  );
  const childRuns = turn.body.filter(
    (item): item is TimelineRunItem => item.kind === "run" && item.agentKind === "subagent"
  );
  const body = turn.body.filter((item) => item.kind !== "run");
  const delegateTools = body.filter(
    (item): item is TimelineToolItem => item.kind === "tool" && protocolToolKind(item.name) === "delegate"
  );
  const agents = subagentEntries(childRuns, delegateTools);
  const output: TimelineDisplayItem[] = [];
  let emittedAgents = false;

  for (let index = 0; index < body.length;) {
    const item = body[index];

    if (item.kind === "ms" && isInternalProtocolLabel(item.label)) {
      index += 1;
      continue;
    }

    if (item.kind === "tool") {
      const protocol = protocolToolKind(item.name);
      if (protocol) {
        if (!emittedAgents && agents.length > 0) {
          output.push({
            kind: "subagent_group",
            id: `subagents-${turn.id}`,
            t: item.t,
            agents,
          });
          emittedAgents = true;
        }
        index += 1;
        continue;
      }

      const groupKind = toolGroupKind(item);
      const tools = [item];
      let cursor = index + 1;
      while (cursor < body.length) {
        const candidate = body[cursor];
        if (
          candidate.kind !== "tool"
          || protocolToolKind(candidate.name)
          || toolGroupKind(candidate) !== groupKind
        ) {
          break;
        }
        tools.push(candidate);
        cursor += 1;
      }
      output.push({
        kind: "tool_group",
        id: `tool-group-${tools.map((tool) => tool.id).join("-")}`,
        t: tools[0].t,
        groupKind,
        tools,
      });
      index = cursor;
      continue;
    }

    output.push(item as TimelineBaseDisplayItem);
    index += 1;
  }

  if (!emittedAgents && agents.length > 0) {
    const group: TimelineSubagentGroupItem = {
      kind: "subagent_group",
      id: `subagents-${turn.id}`,
      t: childRuns[0]?.t ?? 0,
      agents,
    };
    const finalResponse = findLastIndex(output, (item) => item.kind === "agent");
    if (finalResponse >= 0) output.splice(finalResponse, 0, group);
    else output.push(group);
  }

  return {
    id: turn.id,
    user: turn.user,
    runs,
    items: output,
    hasActivity: output.some((item) =>
      item.kind === "tool_group" || item.kind === "subagent_group" || item.kind === "context"
    ),
  };
}

function toolGroupKind(tool: TimelineToolItem): TimelineToolGroupKind {
  return toolActivityKind(tool.name);
}

function protocolToolKind(name: string): "delegate" | "collect" | null {
  const normalized = name.trim().toLowerCase().replace(/[.\-\s]+/g, "_");
  if (normalized.endsWith("delegate_task") || normalized.endsWith("spawn_agent")) return "delegate";
  if (
    normalized.endsWith("collect_subagents")
    || normalized.endsWith("wait_agent")
    || normalized.endsWith("wait_agents")
  ) {
    return "collect";
  }
  return null;
}

function isInternalProtocolLabel(label: string): boolean {
  return label === "subagent_lifecycle"
    || label === "subagent_activity"
    || label === "subagent_tool_audit";
}

function subagentEntries(
  runs: readonly TimelineRunItem[],
  delegateTools: readonly TimelineToolItem[]
): TimelineSubagentEntry[] {
  if (runs.length > 0) {
    return [...runs]
      .sort((a, b) => a.startedAt.localeCompare(b.startedAt) || a.runId.localeCompare(b.runId))
      .map((run) => ({
        id: `subagent-entry-${run.runId}`,
        runId: run.runId,
        label: run.agentLabel?.trim() || runtimeFallbackLabel(run.runtimeKind),
        summary: run.agentSummary,
        model: run.model || null,
        runtimeKind: run.runtimeKind,
        status: runStatus(run),
      }));
  }

  return delegateTools.map((tool, index) => placeholderSubagent(tool, index));
}

function placeholderSubagent(tool: TimelineToolItem, index: number): TimelineSubagentEntry {
  const input = parseRecord(tool.inputJson);
  const output = parseRecord(tool.outputJson);
  const agent = firstString(input, ["agent"]) ?? firstString(output, ["agent"]);
  const runtimeKind = placeholderRuntimeKind(agent);
  const goal = firstString(input, ["goal", "task", "prompt"]);
  const label = firstString(input, ["label", "name"])
    ?? firstString(output, ["label", "name"])
    ?? (goal ? compactLabel(goal) : runtimeFallbackLabel(runtimeKind));
  const status = protocolStatus(firstString(output, ["status", "state"]), tool.state);
  return {
    id: `subagent-placeholder-${tool.callId ?? tool.id}-${index}`,
    runId: null,
    label,
    summary: goal,
    model: firstString(output, ["model"]) ?? null,
    runtimeKind,
    status,
  };
}

function placeholderRuntimeKind(agent: string | null | undefined): TimelineRunItem["runtimeKind"] {
  switch (agent?.trim().toLowerCase()) {
    case "codex": return "codex_exec";
    default: return "native";
  }
}

function runStatus(run: TimelineRunItem): TimelineSubagentStatus {
  if (run.state === "active") return "running";
  if (run.state === "failed") return "failed";
  if (run.state === "aborted") return "cancelled";
  return "completed";
}

function protocolStatus(value: string | null, toolState: TimelineToolItem["state"]): TimelineSubagentStatus {
  switch (value?.toLowerCase()) {
    case "queued": return "queued";
    case "running": return "running";
    case "waiting_permission": return "waiting_permission";
    case "completed": return "completed";
    case "failed": return "failed";
    case "cancelled": return "cancelled";
    default: return toolState === "fail" ? "failed" : "running";
  }
}

function runtimeFallbackLabel(kind: TimelineRunItem["runtimeKind"]): string {
  switch (kind) {
    case "native": return "R-Code 子代理";
    case "codex_exec": return "Codex CLI 子代理";
    case "codex_mcp": return "Codex MCP 子代理";
  }
}

function compactLabel(value: string): string {
  const normalized = value.trim().replace(/\s+/g, " ");
  return normalized.length > 52 ? `${normalized.slice(0, 51)}…` : normalized;
}

function parseRecord(raw: string | null): Record<string, unknown> | null {
  if (!raw) return null;
  let value: unknown = raw;
  for (let depth = 0; depth < 3; depth += 1) {
    if (typeof value === "string") {
      try {
        value = JSON.parse(value);
      } catch {
        return null;
      }
      continue;
    }
    if (value && typeof value === "object" && !Array.isArray(value)) {
      const record = value as Record<string, unknown>;
      if (typeof record.content === "string") {
        try {
          const nested = JSON.parse(record.content);
          if (nested && typeof nested === "object" && !Array.isArray(nested)) {
            return { ...record, ...(nested as Record<string, unknown>) };
          }
        } catch {
          // content 是普通文本时仍返回外层可读字段。
        }
      }
      return record;
    }
    return null;
  }
  return null;
}

function firstString(record: Record<string, unknown> | null, keys: readonly string[]): string | null {
  if (!record) return null;
  for (const key of keys) {
    const value = record[key];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return null;
}

function findLastIndex<T>(items: readonly T[], predicate: (item: T) => boolean): number {
  for (let index = items.length - 1; index >= 0; index -= 1) {
    if (predicate(items[index])) return index;
  }
  return -1;
}
