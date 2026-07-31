import { useEffect, useState } from "react";
import { codexIntegrationStatus, taskSetAgentEngine } from "../../lib/ipc";
import type { TaskAgentEngine } from "../../lib/types";
import { useAsyncAction } from "../../lib/hooks";
import { Menu, MenuItem, MenuSeparator } from "../ui/Menu";
import { StatusBar } from "../ui/StatusBar";
import { IconChevronDown, IconSubagent } from "../icons";
import { useAppStore } from "../../store/app";

interface Props {
  taskId: string;
  value: TaskAgentEngine;
  workspaceAttached: boolean;
  running: boolean;
  onChanged: () => void;
}

/** 会话级主 Agent 选择。后端禁止运行中切换，UI 同步禁用以避免假成功。 */
export function AgentEngineSwitcher({
  taskId,
  value,
  workspaceAttached,
  running,
  onChanged,
}: Props) {
  const setSettingsPane = useAppStore((state) => state.setSettingsPane);
  const [codexReady, setCodexReady] = useState(false);
  const apply = useAsyncAction(async (next: TaskAgentEngine) => {
    await taskSetAgentEngine(taskId, next);
    onChanged();
  }, { label: "切换主 Agent" });

  useEffect(() => {
    let alive = true;
    void codexIntegrationStatus().then((status) => {
      if (alive) setCodexReady(Boolean(status.integration_ready));
    }).catch(() => {
      if (alive) setCodexReady(false);
    });
    return () => { alive = false; };
  }, [taskId]);

  const codexBlocked = !workspaceAttached || !codexReady;
  const title = running
    ? "当前运行结束后可切换主 Agent"
    : `本会话主 Agent：${value === "codex" ? "Codex CLI" : "R-Code"}`;

  return (
    <div className="agent-engine-switcher">
      <Menu
        label="选择主 Agent"
        placement="up"
        align="left"
        disabled={running || apply.busy}
        trigger={
          <button
            type="button"
            className={`provider-pill ready agent-engine-pill engine-${value}`}
            title={title}
            disabled={running || apply.busy}
          >
            <IconSubagent width={14} height={14} />
            <span>{value === "codex" ? "Codex CLI" : "R-Code"}</span>
            <IconChevronDown width={12} height={12} />
          </button>
        }
      >
        {({ close }) => (
          <>
            <MenuItem
              close={close}
              checked={value === "r_code"}
              onSelect={() => void apply.run("r_code")}
              hint="自定义 Provider · 宿主编排"
            >
              R-Code
            </MenuItem>
            <MenuItem
              close={close}
              checked={value === "codex"}
              disabled={codexBlocked}
              onSelect={() => void apply.run("codex")}
              hint={!workspaceAttached ? "先附加工作区" : !codexReady ? "先连接 Codex CLI" : "本机 Codex CLI"}
            >
              Codex CLI
            </MenuItem>
            <MenuSeparator />
            <MenuItem close={close} onSelect={() => setSettingsPane("agents")}>查看编排策略</MenuItem>
            {!codexReady && <MenuItem close={close} onSelect={() => setSettingsPane("codex")}>连接 Codex CLI</MenuItem>}
          </>
        )}
      </Menu>
      {apply.error && <StatusBar kind="error" compact onDismiss={apply.clearError}>{apply.error}</StatusBar>}
    </div>
  );
}
