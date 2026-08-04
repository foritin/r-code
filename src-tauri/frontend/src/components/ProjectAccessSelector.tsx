import type { ProjectAccessMode } from "../lib/types";
import { useAsyncAction } from "../lib/hooks";
import { Menu } from "./ui/Menu";
import { StatusBar } from "./ui/StatusBar";
import {
  IconCheck,
  IconChevronDown,
  IconShield,
  IconTerminal,
  IconUser,
} from "./icons";

const OPTIONS: ReadonlyArray<{
  value: ProjectAccessMode;
  label: string;
  description: string;
  icon: typeof IconShield;
}> = [
  {
    value: "request_approval",
    label: "请求审批",
    description: "编辑文件、使用互联网或执行命令前始终询问。",
    icon: IconUser,
  },
  {
    value: "risk_based",
    label: "替我审批",
    description: "自动批准低风险操作，仅在检测到中高风险时询问。",
    icon: IconTerminal,
  },
  {
    value: "full_access",
    label: "完全访问权限",
    description: "自动批准工作区内的低、中、高风险操作。",
    icon: IconShield,
  },
];

export function projectAccessModeLabel(mode: ProjectAccessMode): string {
  return OPTIONS.find((option) => option.value === mode)?.label ?? "请求审批";
}

export function projectAccessModeShortLabel(mode: ProjectAccessMode): string {
  return (
    {
      request_approval: "审批",
      risk_based: "风险",
      full_access: "完全",
    } as Record<ProjectAccessMode, string>
  )[mode];
}

interface Props {
  value: ProjectAccessMode;
  workspaceName: string;
  disabled?: boolean;
  /** 没有可授权的工作区时仍保留入口，打开后解释边界而不是直接消失。 */
  unavailableReason?: string;
  /** 运行中修改只影响下一轮；由调用方把这个事实显示在菜单里。 */
  changeNotice?: string;
  /** up：输入区（菜单向上展开）；down：会话顶栏 */
  placement?: "up" | "down";
  openRequest?: number;
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
  unavailableReason,
  changeNotice,
  placement = "up",
  openRequest,
  onChange,
}: Props) {
  const save = useAsyncAction(async (next: ProjectAccessMode) => {
    if (next === value) return;
    await onChange(next);
  }, { label: "更新权限" });

  return (
    <div className={`project-access-control mode-${value}${unavailableReason ? " unavailable" : ""}`}>
      <Menu
        trigger={
          <button
            type="button"
            className={`project-access-trigger mode-${value}${unavailableReason ? " unavailable" : ""}`}
            title={unavailableReason ?? `${workspaceName}：${projectAccessModeLabel(value)}（仅限此工作区）`}
          >
            <IconShield width={15} height={15} aria-hidden="true" />
            <span>
              权限：{save.busy
                ? "保存中…"
                : unavailableReason
                  ? "需附加文件夹"
                  : projectAccessModeLabel(value)}
            </span>
            <IconChevronDown width={12} height={12} />
          </button>
        }
        label="项目 Agent 权限"
        placement={placement}
        align="right"
        disabled={disabled || save.busy}
        menuClassName="project-access-menu"
        openRequest={openRequest}
      >
        {({ close }) => (
          <>
            <div className="project-access-head">
              <strong>应如何批准 Agent 操作？</strong>
              <span className="project-access-scope">
                {unavailableReason ?? `仅作用于「${workspaceName}」工作区`}
              </span>
              {changeNotice && !unavailableReason && (
                <span className="project-access-notice">{changeNotice}</span>
              )}
            </div>
            <div className="project-access-options">
              {OPTIONS.map((option) => {
                const OptionIcon = option.icon;
                const selected = option.value === value && !unavailableReason;
                return (
                  <button
                    type="button"
                    role="menuitemradio"
                    aria-checked={selected}
                    className={`project-access-option mode-${option.value}${selected ? " selected" : ""}`}
                    key={option.value}
                    disabled={save.busy || Boolean(unavailableReason)}
                    onClick={() => {
                      close();
                      void save.run(option.value);
                    }}
                  >
                    <span className="project-access-option-icon" aria-hidden="true">
                      <OptionIcon width={18} height={18} />
                    </span>
                    <span className="project-access-option-copy">
                      <strong>{option.label}</strong>
                      <small>{option.description}</small>
                    </span>
                    {selected && <IconCheck className="project-access-check" width={16} height={16} aria-hidden="true" />}
                  </button>
                );
              })}
            </div>
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
