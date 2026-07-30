import type { CSSProperties } from "react";
import { IconSubagent } from "../icons";

export const SUBAGENT_COLORS = ["#58c7a4", "#d86e68", "#45a8b7", "#dca35f", "#9a82e8"] as const;

type SubagentAvatarSize = "xs" | "sm" | "md";

interface SubagentAvatarProps {
  index?: number;
  identity?: string;
  size?: SubagentAvatarSize;
  className?: string;
}

/**
 * 统一的子智能体身份标记。
 * 色彩只承担“区分不同实例”的辅助作用，真正的语义由协作节点图标提供。
 */
export function SubagentAvatar({ index = 0, identity, size = "md", className = "" }: SubagentAvatarProps) {
  const colorIndex = identity ? stableIdentityIndex(identity) : Math.abs(index);
  const color = SUBAGENT_COLORS[colorIndex % SUBAGENT_COLORS.length];
  return (
    <span
      className={`subagent-avatar size-${size}${className ? ` ${className}` : ""}`}
      style={{ "--subagent-color": color } as CSSProperties}
      aria-hidden="true"
    >
      <IconSubagent />
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
