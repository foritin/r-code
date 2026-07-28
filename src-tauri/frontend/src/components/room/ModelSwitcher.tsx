/**
 * 会话的「模型服务 → 具体模型」二级选择。
 *
 * 原先只能切 provider，模型固定取自设置里的 `providers[name].model`，想换
 * deepseek-v4-flash / -pro 必须去设置页改全局值（会影响所有绑定该服务的会话）。
 *
 * 后端本来就支持会话级模型覆盖（SessionState.model → CompletionRequest.model），
 * 只是 commands.rs 恒传 model: None 且 tasks 表没有列。补上之后这里就能直接选。
 *
 * 交互取舍：不用 hover 展开子菜单（键盘与触控都难用），而是把模型平铺在各自
 * 服务的分组下。同服务内换模型即时生效（下次运行），跨服务切换保留二次确认，
 * 与原有的 ProviderSwitcher 行为一致。
 */
import { useState } from "react";
import { taskSetModel, taskSetProvider } from "../../lib/ipc";
import { useAsyncAction } from "../../lib/hooks";
import { rememberModel, resolveActive, type ProviderChoice } from "../../lib/provider";
import { Menu, MenuEmpty, MenuItem } from "../ui/Menu";
import { StatusBar } from "../ui/StatusBar";
import { IconChevronDown, IconPlus } from "../icons";

interface Props {
  taskId: string;
  providerName: string | null;
  model: string | null;
  choices: ProviderChoice[];
  fallback: string;
  running: boolean;
  onChanged: () => void;
  /** bar：会话顶栏的紧凑样式；pill：输入区的胶囊样式 */
  variant?: "bar" | "pill";
  openRequest?: number;
}

interface PendingSwitch {
  provider: ProviderChoice;
  model: string;
}

export function ModelSwitcher({
  taskId,
  providerName,
  model,
  choices,
  fallback,
  running,
  onChanged,
  variant = "bar",
  openRequest,
}: Props) {
  const [pending, setPending] = useState<PendingSwitch | null>(null);
  const [customFor, setCustomFor] = useState<string | null>(null);
  const [customValue, setCustomValue] = useState("");

  const active = resolveActive(choices, fallback, providerName, model);

  const apply = useAsyncAction(async (provider: ProviderChoice, nextModel: string) => {
    // 换服务会在后端清掉旧的模型覆盖，所以顺序必须是先 provider 后 model。
    if (provider.name !== active.name) {
      await taskSetProvider(taskId, provider.name);
    }
    await taskSetModel(taskId, nextModel === provider.model ? null : nextModel);
    rememberModel(provider.name, nextModel);
    setPending(null);
    onChanged();
  }, { label: "切换模型" });

  const choose = (provider: ProviderChoice, nextModel: string) => {
    if (running || !provider.ready) return;
    if (provider.name === active.name) {
      void apply.run(provider, nextModel);   // 同服务内换模型，无需确认
      return;
    }
    setPending({ provider, model: nextModel });  // 跨服务：保留二次确认
  };

  const submitCustom = (provider: ProviderChoice) => {
    const value = customValue.trim();
    if (!value) return;
    setCustomFor(null);
    setCustomValue("");
    choose(provider, value);
  };

  const title = running
    ? "当前运行结束后可切换模型"
    : `本会话使用：${active.provider?.label ?? "未选择"} / ${active.model || "未配置"}`;

  const trigger =
    variant === "pill" ? (
      <button type="button" className="provider-pill ready" title={title} disabled={running}>
        <span>{active.provider?.label ?? "选择模型"}</span>
        {active.model && <small>{active.model}</small>}
        <IconChevronDown width={12} height={12} />
      </button>
    ) : (
      <button type="button" className="room-provider-trigger" title={title} disabled={running}>
        <span>模型</span>
        <b>{active.provider?.label ?? "未选择"}</b>
        {active.model && <small>{active.model}</small>}
      </button>
    );

  return (
    <div className="room-provider">
      <Menu
        trigger={trigger}
        label="选择模型服务与模型"
        placement={variant === "pill" ? "up" : "down"}
        align={variant === "pill" ? "left" : "right"}
        disabled={running || apply.busy}
        menuClassName="model-menu"
        scroll
        openRequest={openRequest}
      >
        {({ close }) => (
          <>
            {choices.length === 0 && <MenuEmpty>没有可用模型服务</MenuEmpty>}
            {choices.map((choice) => (
              <div className="model-group" key={choice.name}>
                <div className="model-group-head">
                  <span>{choice.label}</span>
                  {!choice.ready && <small>尚未完成配置</small>}
                </div>
                {choice.ready &&
                  choice.models.map((candidate) => (
                    <MenuItem
                      key={candidate}
                      close={close}
                      checked={choice.name === active.name && candidate === active.model}
                      hint={candidate === choice.model ? "服务默认" : undefined}
                      onSelect={() => choose(choice, candidate)}
                    >
                      {candidate}
                    </MenuItem>
                  ))}
                {choice.ready &&
                  (customFor === choice.name ? (
                    <div className="model-custom">
                      <input
                        className="input"
                        autoFocus
                        value={customValue}
                        aria-label={`${choice.label} 的自定义模型名`}
                        placeholder="输入模型名…"
                        onChange={(event) => setCustomValue(event.target.value)}
                        onKeyDown={(event) => {
                          if (event.key === "Enter") {
                            event.preventDefault();
                            submitCustom(choice);
                            close();
                          }
                          if (event.key === "Escape") {
                            event.preventDefault();
                            setCustomFor(null);
                          }
                        }}
                      />
                    </div>
                  ) : (
                    <MenuItem
                      close={close}
                      closeOnSelect={false}
                      className="model-custom-open"
                      onSelect={() => {
                        setCustomFor(choice.name);
                        setCustomValue("");
                      }}
                    >
                      <IconPlus width={12} height={12} /> 自定义模型…
                    </MenuItem>
                  ))}
              </div>
            ))}
          </>
        )}
      </Menu>

      {pending && (
        <div className="room-provider-confirm" role="status">
          <span>
            下次运行将使用 {pending.provider.label} / {pending.model}
          </span>
          <button type="button" className="quiet-link" disabled={apply.busy} onClick={() => setPending(null)}>
            取消
          </button>
          <button
            type="button"
            className="btn accent sm"
            disabled={apply.busy}
            onClick={() => void apply.run(pending.provider, pending.model)}
          >
            {apply.busy ? "切换中…" : "确认切换"}
          </button>
        </div>
      )}
      {apply.error && (
        <StatusBar kind="error" compact onDismiss={apply.clearError}>
          {apply.error}
        </StatusBar>
      )}
    </div>
  );
}
