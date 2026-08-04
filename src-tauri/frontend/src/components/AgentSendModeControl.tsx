import { useCallback, useState } from "react";
import type { AgentSendMode } from "../lib/types";
import { IconChevronDown, IconRefresh } from "./icons";
import { Menu, MenuItem } from "./ui/Menu";

export type ExplicitAgentSendMode = Extract<AgentSendMode, "queue" | "steer" | "send_now">;

const SEND_MODE_STORAGE_KEY = "r-code:agent-send-mode";
const SEND_MODE_ORDER: readonly ExplicitAgentSendMode[] = ["queue", "steer", "send_now"];

function isExplicitAgentSendMode(value: string | null): value is ExplicitAgentSendMode {
  return value === "queue" || value === "steer" || value === "send_now";
}

function storedAgentSendMode(): ExplicitAgentSendMode {
  if (typeof window === "undefined") return "queue";
  try {
    const stored = window.localStorage.getItem(SEND_MODE_STORAGE_KEY);
    return isExplicitAgentSendMode(stored) ? stored : "queue";
  } catch {
    return "queue";
  }
}

export function useAgentSendModePreference(): [
  ExplicitAgentSendMode,
  (mode: ExplicitAgentSendMode) => void,
] {
  const [mode, setMode] = useState<ExplicitAgentSendMode>(storedAgentSendMode);
  const updateMode = useCallback((next: ExplicitAgentSendMode) => {
    setMode(next);
    try {
      window.localStorage.setItem(SEND_MODE_STORAGE_KEY, next);
    } catch {
      // A restricted WebView can reject localStorage; the in-memory preference still works.
    }
  }, []);
  return [mode, updateMode];
}

export function effectiveAgentSendMode(
  preferredMode: ExplicitAgentSendMode,
  running: boolean,
): AgentSendMode {
  return running ? preferredMode : "auto";
}

export function agentSendModeLabel(mode: ExplicitAgentSendMode): string {
  switch (mode) {
    case "steer":
      return "引导";
    case "send_now":
      return "立即发送";
    default:
      return "排队";
  }
}

export function agentSendModeTitle(mode: ExplicitAgentSendMode, running: boolean): string {
  if (!running) {
    switch (mode) {
      case "steer":
        return "当前空闲，将立即发送；运行中会在模型的下一个可介入点补充消息";
      case "send_now":
        return "当前空闲，将立即发送；运行中会停止当前生成并保留会话上下文";
      default:
        return "当前空闲，将立即发送；运行中会排到当前轮之后";
    }
  }
  switch (mode) {
    case "steer":
      return "在模型的下一个可介入点补充消息，不中断当前运行";
    case "send_now":
      return "停止当前生成并立即处理这条消息；保留会话上下文";
    default:
      return "当前运行结束后再发送这条消息";
  }
}

function nextAgentSendMode(mode: ExplicitAgentSendMode): ExplicitAgentSendMode {
  const index = SEND_MODE_ORDER.indexOf(mode);
  return SEND_MODE_ORDER[(index + 1) % SEND_MODE_ORDER.length];
}

interface Props {
  mode: ExplicitAgentSendMode;
  running: boolean;
  disabled?: boolean;
  onChange: (mode: ExplicitAgentSendMode) => void;
}

export function AgentSendModeControl({ mode, running, disabled = false, onChange }: Props) {
  const nextMode = nextAgentSendMode(mode);
  const idlePrefix = "当前空闲会直接发送；";
  return (
    <div className={`run-send-mode-control mode-${mode}`}>
      <button
        className="run-send-mode-label run-send-primary"
        type="button"
        disabled={disabled}
        onClick={() => onChange(nextMode)}
        aria-label={
          `当前发送方式：${agentSendModeLabel(mode)}。` +
          `点击切换为${agentSendModeLabel(nextMode)}`
        }
        title={`${agentSendModeTitle(mode, running)}；点击切换为${agentSendModeLabel(nextMode)}`}
      >
        <span className="run-send-mode-dot" aria-hidden="true" />
        <span>{agentSendModeLabel(mode)}</span>
        <IconRefresh className="run-send-mode-cycle" width={11} height={11} aria-hidden="true" />
        <kbd className="sr-only">Enter</kbd>
      </button>
      <Menu
        className="run-send-mode-menu-root"
        label="选择发送方式"
        placement="up"
        align="right"
        menuClassName="comp-more-menu"
        trigger={
          <button
            className="run-send-mode-trigger"
            type="button"
            disabled={disabled}
            aria-label={`选择发送方式，当前为${agentSendModeLabel(mode)}`}
            title="直接选择发送方式"
          >
            <IconChevronDown width={11} height={11} />
          </button>
        }
      >
        {({ close }) => (
          <>
            <MenuItem
              close={close}
              checked={mode === "queue"}
              hint={`${running ? "" : idlePrefix}运行中排到当前轮之后`}
              onSelect={() => onChange("queue")}
            >
              排队发送
            </MenuItem>
            <MenuItem
              close={close}
              checked={mode === "steer"}
              hint={`${running ? "" : idlePrefix}运行中等待模型的下一个可介入点`}
              onSelect={() => onChange("steer")}
            >
              引导当前运行
            </MenuItem>
            <MenuItem
              close={close}
              checked={mode === "send_now"}
              className="is-destructive"
              hint={`${running ? "停止当前生成；" : idlePrefix}保留会话上下文`}
              onSelect={() => onChange("send_now")}
            >
              立即发送
            </MenuItem>
          </>
        )}
      </Menu>
    </div>
  );
}
