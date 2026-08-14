/** 展示格式化工具。 */
import type { AgentRun, PermissionRequest, RiskLevel } from "./types";

/** RFC3339 → 已流逝时长："12m 34s" / "2h 07m" / "3d" */
export function elapsedSince(iso: string, now = Date.now()): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "—";
  let s = Math.max(0, Math.floor((now - t) / 1000));
  if (s < 60) return `${s}s`;
  const d = Math.floor(s / 86400);
  s -= d * 86400;
  const h = Math.floor(s / 3600);
  s -= h * 3600;
  const m = Math.floor(s / 60);
  s -= m * 60;
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m ${String(s).padStart(2, "0")}s`;
}

/** RFC3339 → 分钟级相对时长，适用于空间受限的会话列表。 */
export function elapsedMinutes(iso: string, now = Date.now()): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "—";
  let minutes = Math.max(0, Math.floor((now - t) / 60_000));
  if (minutes === 0) return "刚刚";
  const days = Math.floor(minutes / 1_440);
  minutes -= days * 1_440;
  const hours = Math.floor(minutes / 60);
  minutes -= hours * 60;
  if (days > 0) return hours > 0 ? `${days}d ${hours}h` : `${days}d`;
  if (hours > 0) return minutes > 0 ? `${hours}h ${minutes}m` : `${hours}h`;
  return `${minutes}m`;
}

/** 仅用于展示：移除 Windows canonicalize 产生的 verbatim 路径前缀。 */
export function displayPath(path: string): string {
  const uncPrefix = "\\\\?\\UNC\\";
  const verbatimPrefix = "\\\\?\\";
  if (path.startsWith(uncPrefix)) return `\\\\${path.slice(uncPrefix.length)}`;
  const drivePath = path.startsWith(verbatimPrefix) ? path.slice(verbatimPrefix.length) : "";
  return /^[A-Za-z]:[\\/]/.test(drivePath) ? drivePath : path;
}

function stripVerbatimDrivePrefixes(text: string, marker: string, escapedSeparator: boolean): string {
  let rendered = text;
  let cursor = 0;
  while (cursor < rendered.length) {
    const markerIndex = rendered.indexOf(marker, cursor);
    if (markerIndex < 0) break;
    const driveIndex = markerIndex + marker.length;
    const drive = rendered[driveIndex];
    const separator = rendered[driveIndex + 2];
    const validSeparator = escapedSeparator
      ? separator === "/" || (separator === "\\" && rendered[driveIndex + 3] === "\\")
      : separator === "/" || separator === "\\";
    if (drive && /[A-Za-z]/.test(drive) && rendered[driveIndex + 1] === ":" && validSeparator) {
      rendered = rendered.slice(0, markerIndex) + rendered.slice(driveIndex);
      cursor = markerIndex + 3;
    } else {
      cursor = driveIndex;
    }
  }
  return rendered;
}

/** 仅用于可见文本：兼容旧会话中的原始及 JSON 转义 Windows verbatim 路径。 */
export function displayPathsInText(text: string): string {
  const escapedUnc = text.replaceAll("\\\\\\\\?\\\\UNC\\\\", "\\\\\\\\");
  const escapedDrive = stripVerbatimDrivePrefixes(escapedUnc, "\\\\\\\\?\\\\", true);
  const rawUnc = escapedDrive.replaceAll("\\\\?\\UNC\\", "\\\\");
  return stripVerbatimDrivePrefixes(rawUnc, "\\\\?\\", false);
}

/** RFC3339 → "HH:MM"（本地时间） */
export function clockTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "—";
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

/**
 * RFC3339 → "HH:MM:SS"（本地时间）。
 * 审计流里同一分钟常有多条记录，分钟级时间无法排序取证，必须到秒。
 */
export function clockSeconds(iso: string | null | undefined): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "—";
  return [d.getHours(), d.getMinutes(), d.getSeconds()]
    .map((part) => String(part).padStart(2, "0"))
    .join(":");
}

/** 从 task id 派生确定性的会话光谱色（rail 色条 / 会话标识）。 */
const HUES = ["#6ee7f2", "#eebf6d", "#8b7cf6", "#5fe3a1", "#f2a3d8", "#8fb8e8", "#f0a05a"];
export function hueFor(id: string): string {
  let h = 0;
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) >>> 0;
  return HUES[h % HUES.length];
}

/** 工具名 → 动作行 verb（fusion：edit/write/run/touched）。 */
export function toolVerb(toolName: string): string {
  const n = toolName.toLowerCase();
  if (n.includes("write") || n.includes("create") || n.includes("写入") || n.includes("创建")) return "write";
  if (
    n.includes("edit")
    || n.includes("patch")
    || n.includes("replace")
    || n.includes("文件修改")
    || n.includes("编辑")
  ) return "edit";
  if (
    n.includes("bash")
    || n.includes("shell")
    || n.includes("run")
    || n.includes("exec")
    || n.includes("command")
    || n.includes("terminal")
    || n.includes("命令")
  ) return "run";
  if (
    n.includes("read")
    || n.includes("view")
    || n.includes("list_file")
    || n.includes("list_dir")
    || n.includes("读取")
    || n.includes("查看")
  ) return "read";
  if (
    n.includes("search")
    || n.includes("grep")
    || n.includes("glob")
    || n.includes("find")
    || n.includes("搜索")
    || n.includes("查找")
  ) return "search";
  return "tool";
}

/** 从工具输入 JSON 提取展示目标（路径或命令）。 */
export function toolTarget(inputJson: string | null | undefined): string {
  if (!inputJson) return "";
  try {
    const v = JSON.parse(inputJson) as Record<string, unknown>;
    for (const k of ["path", "file_path", "filePath", "filename"]) {
      const val = v[k];
      if (typeof val === "string" && val) return displayPath(val);
    }
    for (const k of ["command", "cmd", "query", "pattern"]) {
      const val = v[k];
      if (typeof val === "string" && val) return val;
    }
    return "";
  } catch {
    return "";
  }
}

/** IPC 错误 → 可读文案（Tauri invoke 可能抛 string 或 Error）。 */
export function errText(e: unknown): string {
  if (e instanceof Error) return e.message;
  return String(e);
}

/** 模式 → 人性化文案（humane language 红线）。 */
export function modeLabel(mode: "ask" | "edit" | "auto" | "plan"): string {
  return mode === "plan"
    ? "Plan — 先规划，确认后实施"
    : "Agent — 直接处理；可用能力由项目权限决定";
}

/** 模式 → 芯片用短标签；解释性文案放 title，避免与项目权限芯片撞车。 */
export function modeShortLabel(mode: "ask" | "edit" | "auto" | "plan"): string {
  return mode === "plan" ? "Plan" : "Agent";
}

export interface PermissionAttribution {
  kind: "main" | "subagent" | "terminal" | "unknown";
  label: string;
}

/** 权限归属优先依据运行树，旧记录再回退到 caller 约定。 */
export function permissionAttribution(
  permission: PermissionRequest,
  runs: readonly AgentRun[]
): PermissionAttribution {
  const run = permission.run_id
    ? runs.find((candidate) => candidate.id === permission.run_id)
    : undefined;
  if (run?.agent_kind === "subagent") {
    return { kind: "subagent", label: run.agent_label?.trim() || "子代理" };
  }
  if (run?.agent_kind === "main") return { kind: "main", label: "主代理" };

  const caller = permission.caller?.trim() ?? "";
  if (caller === "agent") return { kind: "main", label: "主代理" };
  if (caller.startsWith("subagent:")) return { kind: "subagent", label: "子代理" };
  if (caller.startsWith("terminal:")) return { kind: "terminal", label: "终端" };
  return { kind: "unknown", label: "未知来源" };
}

/** 风险等级的面向用户说明，保留等级编号以便审计定位。 */
export function permissionRiskLabel(risk: RiskLevel): string {
  switch (risk) {
    case "R0":
      return "无风险";
    case "R1":
      return "低风险";
    case "R2":
      return "需要确认";
    case "R3":
      return "高风险";
    case "R4":
      return "已阻止";
  }
}

/** 任务状态 → 灯变体。 */
export function lampFor(state: string, needsYou: boolean): "run" | "attn" | "done" | "fail" | "" {
  if (needsYou) return "attn";
  switch (state) {
    case "in_progress":
    case "exploring": return "run";
    case "review_ready": return "attn";
    case "idle": return "done";
    case "interrupted": return "fail";
    case "archived": return "";
    default: return "";
  }
}
