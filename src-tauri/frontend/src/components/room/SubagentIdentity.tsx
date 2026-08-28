import type { CSSProperties } from "react";
import type { AgentRun } from "../../lib/types";
import { IconCodexSubagent, IconSubagent } from "../icons";

export const RCODE_SUBAGENT_COLORS = ["#58c7a4", "#45a8b7", "#6d9ed4"] as const;
export const CODEX_SUBAGENT_COLORS = ["#df765d", "#d99a51", "#c8789f"] as const;
export const EXTERNAL_SUBAGENT_COLORS = ["#8a7bd1", "#6f8fc7", "#9b72b0"] as const;

type SubagentAvatarSize = "xs" | "sm" | "md";

/** 状态环：run=运行中（accent 脉冲）/ ok=完成 / warn=未完成或无产出。 */
export type SubagentAvatarStatus = "run" | "ok" | "warn";

interface SubagentAvatarProps {
  index?: number;
  identity?: string;
  runtimeKind?: AgentRun["runtime_kind"];
  size?: SubagentAvatarSize;
  status?: SubagentAvatarStatus;
  className?: string;
}

/**
 * 统一的子智能体身份标记。执行器决定图形与色系；轮廓统一为 signature 不对称
 * 圆角方块，稳定身份只在同一套执行器色板内区分不同实例。status 提供状态环。
 */
export function SubagentAvatar({
  index = 0,
  identity,
  runtimeKind = "native",
  size = "md",
  status,
  className = "",
}: SubagentAvatarProps) {
  const runtimeFamily = runtimeKind === "native"
    ? "rcode"
    : runtimeKind === "codex_exec" || runtimeKind === "codex_mcp"
      ? "codex"
      : "external";
  const palette = runtimeFamily === "rcode"
    ? RCODE_SUBAGENT_COLORS
    : runtimeFamily === "codex"
      ? CODEX_SUBAGENT_COLORS
      : EXTERNAL_SUBAGENT_COLORS;
  const colorIndex = identity ? stableIdentityIndex(identity) : Math.abs(index);
  const color = palette[colorIndex % palette.length];
  return (
    <span
      className={`subagent-avatar runtime-${runtimeFamily} size-${size}${className ? ` ${className}` : ""}`}
      data-runtime-family={runtimeFamily}
      data-status={status}
      style={{ "--subagent-color": color } as CSSProperties}
      aria-hidden="true"
    >
      {runtimeFamily === "codex" ? <IconCodexSubagent /> : <IconSubagent />}
    </span>
  );
}

function stableIdentityIndex(identity: string): number {
  let hash = 0;
  for (let index = 0; index < identity.length; index += 1) {
    hash = Math.imul(hash, 31) + identity.charCodeAt(index);
  }
  return Math.abs(hash);
}
