import { isPlanToolName, toolDisplayName, toolVerb, type ToolDisplayLanguage } from "../../lib/format";

export type ToolActivityKind = "command" | "file" | "lookup" | "plan" | "tool";
export type ToolActivityState = "active" | "ok" | "fail";

export function toolActivityKind(toolName: string): ToolActivityKind {
  if (isPlanToolName(toolName)) return "plan";
  const verb = toolVerb(toolName);
  if (verb === "run") return "command";
  if (verb === "edit" || verb === "write") return "file";
  // 读取/查看具体文件与编辑同流呈现：彩色类型图标 + 动词的文件行（原型 C）。
  // 目录/清单类（list_*）仍归「探索」，避免目录行淹没时间线。
  if (verb === "read" && !isListingTool(toolName)) return "file";
  if (verb === "read" || verb === "search") return "lookup";
  return "tool";
}

function isListingTool(toolName: string): boolean {
  return toolName.toLowerCase().includes("list_");
}

export function toolActivityTitle(
  kind: ToolActivityKind,
  count: number,
  state: ToolActivityState,
  target = "",
  language: ToolDisplayLanguage = "zh",
): string {
  const compactTarget = compactActivityTarget(target);
  const multiple = count > 1;
  if (kind === "plan") {
    const label = toolDisplayName(target, language);
    if (state === "active") return multiple ? `正在推进 ${count} 项计划` : `正在${label}`;
    if (state === "fail") return multiple ? `${count} 项计划操作中有失败` : `${label}失败`;
    return multiple ? `已完成 ${count} 项计划操作` : `已${label}`;
  }
  if (kind === "command") {
    if (state === "active") return multiple ? `正在执行 ${count} 个命令` : compactTarget ? `正在执行 ${compactTarget}` : "正在执行命令";
    if (state === "fail") return multiple ? `${count} 个命令中有执行失败` : compactTarget ? `命令执行失败：${compactTarget}` : "命令执行失败";
    return multiple ? `已执行 ${count} 个命令` : compactTarget ? `已执行 ${compactTarget}` : "已执行命令";
  }
  if (kind === "file") {
    if (state === "active") return multiple ? `正在处理 ${count} 个文件` : "正在处理文件";
    if (state === "fail") return multiple ? `${count} 个文件未全部处理完成` : "文件处理未完成";
    return multiple ? `已处理 ${count} 个文件` : "已处理文件";
  }
  if (kind === "lookup") {
    if (state === "active") return multiple ? `正在探索 ${count} 项` : compactTarget ? `正在探索 ${compactTarget}` : "正在探索";
    if (state === "fail") return multiple ? `${count} 项探索中有失败` : compactTarget ? `探索失败：${compactTarget}` : "探索失败";
    return multiple ? `已探索 ${count} 项` : compactTarget ? `已探索 ${compactTarget}` : "已探索";
  }
  if (state === "active") return multiple ? `正在使用 ${count} 个工具` : `正在使用 ${compactTarget || "工具"}`;
  if (state === "fail") return multiple ? `${count} 个工具中有执行失败` : `${compactTarget || "工具"}执行失败`;
  return multiple ? `已使用 ${count} 个工具` : `已使用 ${compactTarget || "工具"}`;
}

export function toolActivityProgress(states: readonly ToolActivityState[]): string {
  const completed = states.filter((state) => state === "ok").length;
  const active = states.filter((state) => state === "active").length;
  const failed = states.filter((state) => state === "fail").length;
  if (active > 0) {
    return `完成 ${completed}/${states.length} · ${active} 运行中${failed > 0 ? ` · ${failed} 失败` : ""}`;
  }
  if (failed > 0) return `完成 ${completed}/${states.length} · ${failed} 失败`;
  return states.length > 1 ? `${states.length} 项完成` : "完成";
}

export interface ToolDiffStat {
  plus: number;
  minus: number;
}

/**
 * 从编辑 / 写入工具的输入载荷估算行级差异，用于文件行的行内 +N −N。
 * edit 载荷是 old_string / new_string 成对替换；write 载荷只有整份新内容，
 * 删除行数未知，按 0 展示。解析失败或字段缺失返回 null，调用方回退到摘要。
 */
export function toolDiffStat(inputJson: string | null | undefined, verb: string): ToolDiffStat | null {
  if (!inputJson || (verb !== "edit" && verb !== "write")) return null;
  try {
    const value = JSON.parse(inputJson) as Record<string, unknown>;
    const lineCount = (candidate: unknown): number | null => {
      if (typeof candidate !== "string") return null;
      return candidate === "" ? 0 : candidate.split("\n").length;
    };
    if (verb === "edit") {
      const plus = lineCount(value.new_string ?? value.newText);
      const minus = lineCount(value.old_string ?? value.oldText);
      if (plus === null && minus === null) return null;
      return { plus: plus ?? 0, minus: minus ?? 0 };
    }
    const plus = lineCount(value.content ?? value.contents ?? value.text);
    return plus === null ? null : { plus, minus: 0 };
  } catch {
    return null;
  }
}

function compactActivityTarget(value: string): string {
  const normalized = value.trim().replace(/\s+/g, " ");
  return normalized.length > 72 ? `${normalized.slice(0, 71)}…` : normalized;
}
