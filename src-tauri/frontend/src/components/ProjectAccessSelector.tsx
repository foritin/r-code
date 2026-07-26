import type { ProjectAccessMode } from "../lib/types";
import { useAsyncAction } from "../lib/hooks";
import { Menu, MenuItem } from "./ui/Menu";
import { StatusBar } from "./ui/StatusBar";
import { IconChevronDown } from "./icons";

const OPTIONS: ReadonlyArray<{
  value: ProjectAccessMode;
  label: string;
  description: string;
}> = [
  {
    value: "request_approval",
    label: "请求批准",
    description: "访问外部服务、执行命令或修改文件时总是询问。",
  },
  {
    value: "risk_based",
    label: "替我审批",
    description: "仅在检测到中高风险操作时请求批准。",
  },
  {
    value: "full_access",
    label: "完全访问权限",
    description: "自动批准工作区内的低、中、高风险操作。",
  },
];

export function projectAccessModeLabel(mode: ProjectAccessMode): string {
  return OPTIONS.find((option) => option.value === mode)?.label ?? "请求批准";
}

export function projectAccessModeShortLabel(mode: ProjectAccessMode): string {
  return (
    {
      request_approval: "批准",
      risk_based: "风险",
      full_access: "完全",
    } as Record<ProjectAccessMode, string>
  )[mode];
}

interface Props {
  value: ProjectAccessMode;
  workspaceName: string;
  disabled?: boolean;
  /** up：输入区（菜单向上展开）；down：会话顶栏 */
  placement?: "up" | "down";
  onChange: (next: ProjectAccessMode) => Promise<void> | void;
}

/**
 * 项目级权限入口。模式只影响 Agent 的自动工具调用；本地路径始终受当前工作区边界限制。
 *
 * 开合、Escape、方向键导航、焦点归还都交给 Menu —— 原先这里自己挂了一个**常驻**
 * 的 document mousedown 监听器（useEffect 依赖 []，没有 open 守卫），Rail 里每渲染
 * 一个工作区行就多挂一个。
 */
export function ProjectAccessSelector({
  value,
  workspaceName,
  disabled = false,
  placement = "up",
  onChange,
}: Props) {
  const save = useAsyncAction(async (next: ProjectAccessMode) => {
    if (next === value) return;
    await onChange(next);
  }, { label: "更新权限" });

  return (
    <div className="project-access-control">
      <Menu
        trigger={
          <button
            type="button"
            className="project-access-trigger"
            title={`${workspaceName}：${projectAccessModeLabel(value)}（仅限此工作区）`}
          >
            <span>权限：{save.busy ? "保存中…" : projectAccessModeLabel(value)}</span>
            <IconChevronDown width={12} height={12} />
          </button>
        }
        label="项目 Agent 权限"
        placement={placement}
        align="right"
        disabled={disabled || save.busy}
        menuClassName="project-access-menu"
      >
        {({ close }) => (
          <>
            <div className="popover-head">
              <strong>应如何批准操作？</strong>
              <span>仅限「{workspaceName}」工作区</span>
            </div>
            {OPTIONS.map((option) => (
              <MenuItem
                key={option.value}
                close={close}
                checked={option.value === value}
                hint={option.description}
                disabled={save.busy}
                onSelect={() => void save.run(option.value)}
              >
                {option.label}
              </MenuItem>
            ))}
          </>
        )}
      </Menu>
      {save.error && (
        <StatusBar kind="error" compact onDismiss={save.clearError}>
          {save.error}
        </StatusBar>
      )}
    </div>
  );
}
