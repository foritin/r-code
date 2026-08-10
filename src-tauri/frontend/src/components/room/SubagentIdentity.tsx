import type { CSSProperties } from "react";
import type { AgentRun } from "../../lib/types";
import { IconCodexSubagent, IconSubagent } from "../icons";

export const RCODE_SUBAGENT_COLORS = ["#58c7a4", "#45a8b7", "#6d9ed4"] as const;
export const CODEX_SUBAGENT_COLORS = ["#df765d", "#d99a51", "#c8789f"] as const;

type SubagentAvatarSize = "xs" | "sm" | "md";

interface SubagentAvatarProps {
  index?: number;
  identity?: string;
  runtimeKind?: AgentRun["runtime_kind"];
  size?: SubagentAvatarSize;
  className?: string;
}

/**
 * 统一的子智能体身份标记。执行器决定图形、色系和轮廓；稳定身份只在同一套
 * 执行器色板内区分不同实例。
 */
export function SubagentAvatar({
  index = 0,
  identity,
  runtimeKind = "native",
  size = "md",
  className = "",
}: SubagentAvatarProps) {
  const runtimeFamily = runtimeKind === "native" ? "rcode" : "codex";
  const palette = runtimeFamily === "rcode" ? RCODE_SUBAGENT_COLORS : CODEX_SUBAGENT_COLORS;
  const colorIndex = identity ? stableIdentityIndex(identity) : Math.abs(index);
  const color = palette[colorIndex % palette.length];
  return (
    <span
      className={`subagent-avatar runtime-${runtimeFamily} size-${size}${className ? ` ${className}` : ""}`}
      data-runtime-family={runtimeFamily}
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
