/** 会话级 Provider、模型与模型专属推理参数的紧凑配置入口。 */
import { ConfigBack, ConfigRow } from "./model-config-ui";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { providerBalance, taskSetInference, taskSetModel, taskSetProvider } from "../../lib/ipc";
import { useAsyncAction } from "../../lib/hooks";
import { errText } from "../../lib/format";
import { rememberModel, resolveActive, type ProviderChoice } from "../../lib/provider";
import type { InferenceOptions, ProviderBalanceResponse } from "../../lib/types";
import { Menu, MenuEmpty, MenuItem, MenuSeparator } from "../ui/Menu";
import { StatusBar } from "../ui/StatusBar";
import { AnchoredSurface } from "../ui/AnchoredSurface";
import { IconChevronDown } from "../icons";
import {
  capabilitiesFor,
  inferenceSummary,
  normalizeInference,
  optionLabel,
  type CapabilityControl,
} from "./model-capabilities";

interface Props {
  taskId: string | null;
  providerName: string | null;
  model: string | null;
  inference: InferenceOptions;
  choices: ProviderChoice[];
  fallback: string;
  running: boolean;
  onChanged?: () => void;
  onDraftChanged?: (selection: {
    providerName: string;
    model: string | null;
    inference: InferenceOptions;
  }) => void;
  scopeLabel?: string;
  variant?: "bar" | "pill";
  openRequest?: number;
}

interface PendingSwitch {
  provider: ProviderChoice;
  model: string | null;
}

type View = "root" | "models" | "thinking" | "reasoning" | "verbosity";

const VIEW_TITLES: Record<Exclude<View, "root">, string> = {
  models: "模型",
  thinking: "思考模式",
  reasoning: "推理强度",
  verbosity: "输出详略",
};

export function ModelSwitcher({
  taskId,
  providerName,
  model,
  inference,
  choices,
  fallback,
  running,
  onChanged,
  onDraftChanged,
  scopeLabel,
  variant = "bar",
  openRequest,
}: Props) {
  const [view, setView] = useState<View>("root");
  const [pending, setPending] = useState<PendingSwitch | null>(null);
  const [expandedProvider, setExpandedProvider] = useState<string | null>(null);
  const [balanceHover, setBalanceHover] = useState(false);
  const balanceAnchorRef = useRef<HTMLElement | null>(null);
  const [balance, setBalance] = useState<ProviderBalanceResponse | null>(null);
  const [balanceState, setBalanceState] = useState<"idle" | "loading" | "ready" | "error">("idle");
  const [balanceError, setBalanceError] = useState<string | null>(null);
  const balanceRequest = useRef(0);
  const balanceTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const active = resolveActive(choices, fallback, providerName, model);
  const capabilities = useMemo(
    () => capabilitiesFor(active.provider, active.model),
    [active.provider, active.model],
  );
  const normalized = normalizeInference(inference, capabilities);
  const configuredChoices = useMemo(
    () => choices
      .filter((choice) => choice.ready)
      .sort((left, right) => Number(right.name === active.name) - Number(left.name === active.name)),
    [choices, active.name],
  );

  const openModels = () => {
    // 分组默认全部收起，避免长列表一眼铺开；用户按需展开某个 Provider。
    setExpandedProvider(null);
    setView("models");
  };

  const deepseekHover = active.provider?.kind?.toLowerCase() === "deepseek" && !running;

  const refreshBalance = useCallback(async () => {
    const request = ++balanceRequest.current;
    setBalanceState("loading");
    setBalanceError(null);
    try {
      const next = await providerBalance({ name: active.name });
      if (request !== balanceRequest.current) return;
      setBalance(next);
      setBalanceState("ready");
    } catch (cause) {
      if (request !== balanceRequest.current) return;
      setBalance(null);
      setBalanceError(errText(cause));
      setBalanceState("error");
    }
  }, [active.name]);

  useEffect(() => {
    balanceRequest.current += 1;
    setBalance(null);
    setBalanceState("idle");
    setBalanceError(null);
    setBalanceHover(false);
  }, [active.name]);

  useEffect(() => {
    if (!balanceHover) {
      if (balanceTimer.current) {
        clearTimeout(balanceTimer.current);
        balanceTimer.current = null;
      }
      return;
    }
    if (balanceState !== "idle") return;
    balanceTimer.current = setTimeout(() => {
      balanceTimer.current = null;
      void refreshBalance();
    }, 350);
    return () => {
      if (balanceTimer.current) {
        clearTimeout(balanceTimer.current);
        balanceTimer.current = null;
      }
    };
  }, [balanceHover, balanceState, refreshBalance]);

  useEffect(() => () => {
    if (balanceTimer.current) clearTimeout(balanceTimer.current);
  }, []);

  const applyModel = useAsyncAction(async (provider: ProviderChoice, nextModel: string | null) => {
    if (taskId) {
      if (provider.name !== active.name) await taskSetProvider(taskId, provider.name);
      await taskSetModel(taskId, nextModel);
    } else {
      onDraftChanged?.({ providerName: provider.name, model: nextModel, inference: {} });
    }
    if (nextModel) rememberModel(provider.name, nextModel);
    setPending(null);
    setView("root");
    onChanged?.();
  }, { label: "切换模型" });

  const saveInference = useAsyncAction(async (next: InferenceOptions) => {
    if (taskId) await taskSetInference(taskId, next);
    else onDraftChanged?.({ providerName: active.name, model, inference: next });
    onChanged?.();
  }, { label: "保存模型配置" });

  const chooseModel = (provider: ProviderChoice, nextModel: string | null) => {
    if (running || applyModel.busy || !provider.ready) return;
    if (provider.name === active.name && nextModel === model) {
      setView("root");
      return;
    }
    if (!taskId || provider.name === active.name) {
      void applyModel.run(provider, nextModel);
      return;
    }
    setPending({ provider, model: nextModel });
  };

  const chooseOption = (
    field: "thinking" | "reasoning_effort" | "verbosity",
    value: string | null,
  ) => {
    if (saveInference.busy) return;
    const next = { ...normalized };
    if (field === "thinking") {
      if (value) next.thinking = value;
      else delete next.thinking;
      // The effort row is a fixed-depth choice. Changing the thinking policy clears a stale
      // effort so returning to Smart Balance truly re-enables the local governor, while
      // choosing Always On starts from DeepSeek's native high default.
      delete next.reasoning_effort;
    } else if (field === "reasoning_effort") {
      if (value) {
        next.reasoning_effort = value;
        // A selected effort is an explicit fixed-depth preference. DeepSeek's smart-balance
        // marker must not remain alongside it or the runtime cannot distinguish user intent.
        if (capabilities.thinking?.defaultValue === "adaptive") next.thinking = "enabled";
      } else {
        delete next.reasoning_effort;
        // In DeepSeek's UI this row is labelled “Follow Smart Balance”. Clearing a fixed effort
        // must therefore clear the implicit Always On marker as well; users who want native high
        // on every round can still choose Always On explicitly from the thinking row.
        if (capabilities.thinking?.defaultValue === "adaptive") delete next.thinking;
      }
    } else if (value) next.verbosity = value;
    else delete next.verbosity;
    void saveInference.run(next);
    setView("root");
  };

  const summary = inferenceSummary(capabilities, normalized);
  const resetLabel = capabilities.thinking?.defaultValue
    ? "恢复智能平衡"
    : "重置为服务默认";
  const hasCustomInference = Boolean(
    (normalized.thinking && normalized.thinking !== capabilities.thinking?.defaultValue)
    || normalized.reasoning_effort
    || normalized.verbosity,
  );
  const title = running
    ? "当前运行结束后可修改模型配置"
    : `${active.provider?.label ?? "未选择"} / ${active.model || "未配置"} / ${summary}`;
  const readyClass = active.provider?.ready ? " ready" : "";
  const triggerTitle = deepseekHover ? undefined : title;
  const triggerRef = deepseekHover
    ? (node: HTMLElement | null) => { balanceAnchorRef.current = node; }
    : undefined;
  const triggerHover = deepseekHover
    ? {
        onMouseEnter: () => setBalanceHover(true),
        onMouseLeave: () => setBalanceHover(false),
        onFocus: () => setBalanceHover(true),
        onBlur: () => setBalanceHover(false),
      }
    : {};
  const trigger = variant === "pill" ? (
    <button type="button" ref={triggerRef} className={`provider-pill model-config-trigger${readyClass}`} title={triggerTitle} disabled={running} {...triggerHover}>
      <span>{active.provider?.label ?? "模型配置"}</span>
      <small>{active.model || "未配置"} · {summary}</small>
      <IconChevronDown width={12} height={12} />
    </button>
  ) : (
    <button type="button" ref={triggerRef} className="room-provider-trigger" title={triggerTitle} disabled={running} {...triggerHover}>
      <span>模型</span>
      <b>{active.provider?.label ?? "未选择"}</b>
      <small>{active.model || "未配置"} · {summary}</small>
    </button>
  );

  const renderOptionView = (
    control: CapabilityControl | undefined,
    field: "thinking" | "reasoning_effort" | "verbosity",
    current: string | null | undefined,
  ) => {
    const defaultSelected = !current || current === control?.defaultValue;
    return (
      <>
        <ConfigBack title={VIEW_TITLES[view as Exclude<View, "root">]} onBack={() => setView("root")} />
        {!control ? (
          <MenuEmpty>当前模型没有声明这项能力</MenuEmpty>
        ) : (
          <>
            <MenuItem closeOnSelect={false} checked={defaultSelected} onSelect={() => chooseOption(field, null)}>
              {control.defaultLabel}
            </MenuItem>
            {control.options.filter((option) => option.value !== control.defaultValue).map((option) => (
              <MenuItem
                key={option.value}
                closeOnSelect={false}
                checked={current === option.value}
                hint={option.description}
                onSelect={() => chooseOption(field, option.value)}
              >
                {option.label}
              </MenuItem>
            ))}
          </>
        )}
      </>
    );
  };

  return (
    <div className="room-provider model-config-root">
      <Menu
        trigger={trigger}
        role="dialog"
        label="模型与推理配置"
        placement={variant === "pill" ? "up" : "down"}
        align={variant === "pill" ? "left" : "right"}
        disabled={running}
        menuClassName="model-menu model-config-menu"
        scroll
        openRequest={openRequest}
        onOpenChange={(open) => {
          if (open) setBalanceHover(false); else {
            setView("root");
            setExpandedProvider(null);
          }
        }}
      >
        {view === "root" ? (
          <>
            <div className="model-config-head">
              <div><strong>{active.provider?.label ?? "模型配置"}</strong><small>{scopeLabel ?? (taskId ? "仅作用于当前会话" : "仅作用于新对话")}</small></div>
              {saveInference.busy && <span>保存中…</span>}
            </div>
            <ConfigRow label="模型" value={active.model || "未配置"} onSelect={openModels} />
            {capabilities.thinking && (
              <ConfigRow
                label={capabilities.thinking.label}
                value={optionLabel(capabilities.thinking, normalized.thinking)}
                onSelect={() => setView("thinking")}
              />
            )}
            {capabilities.reasoning && (
              <ConfigRow
                label={capabilities.reasoning.label}
                value={optionLabel(capabilities.reasoning, normalized.reasoning_effort)}
                onSelect={() => setView("reasoning")}
              />
            )}
            {capabilities.verbosity && (
              <ConfigRow
                label={capabilities.verbosity.label}
                value={optionLabel(capabilities.verbosity, normalized.verbosity)}
                onSelect={() => setView("verbosity")}
              />
            )}
            <p className="model-config-note">{capabilities.note}</p>
            {!hasCustomInference ? null : (
              <>
                <MenuSeparator />
                <button className="model-config-reset" type="button" onClick={() => void saveInference.run({})}>
                  {resetLabel}
                </button>
              </>
            )}
          </>
        ) : view === "models" ? (
          <>
            <ConfigBack title="模型" onBack={() => setView("root")} />
            {pending && (
              <div className="model-switch-confirm" role="status">
                <span>切换到 {pending.provider.label}<small>{pending.model ?? `Provider 默认 · ${pending.provider.model}`}</small></span>
                <div>
                  <button type="button" className="quiet-link" disabled={applyModel.busy} onClick={() => setPending(null)}>取消</button>
                  <button type="button" className="btn accent sm" disabled={applyModel.busy} onClick={() => void applyModel.run(pending.provider, pending.model)}>
                    {applyModel.busy ? "切换中…" : "确认"}
                  </button>
                </div>
              </div>
            )}
            {configuredChoices.length === 0 && <MenuEmpty>没有已完成配置的模型服务</MenuEmpty>}
            {configuredChoices.map((choice) => {
              const expanded = expandedProvider === choice.name;
              const current = choice.name === active.name;
              const candidates = current && model && !choice.models.includes(model)
                ? [model, ...choice.models]
                : choice.models;
              return (
                <section className={`model-group${expanded ? " expanded" : ""}${current ? " current" : ""}`} key={choice.name}>
                  <button
                    type="button"
                    className="model-group-toggle ring-inset"
                    aria-expanded={expanded}
                    aria-current={current ? "true" : undefined}
                    onClick={() => {
                      setExpandedProvider(expanded ? null : choice.name);
                    }}
                  >
                    <span className="model-group-title">{choice.label}</span>
                    {current && <span className="model-current-provider-badge">当前使用</span>}
                    <IconChevronDown className="model-group-chevron" width={13} height={13} />
                  </button>
                  {expanded && (
                    <div className="model-group-body">
                      <MenuItem
                        closeOnSelect={false}
                        checked={current && model === null}
                        hint={`随服务设置变化 · 当前 ${choice.model}`}
                        onSelect={() => chooseModel(choice, null)}
                      >
                        使用服务默认模型
                      </MenuItem>
                      {candidates.map((candidate) => (
                        <MenuItem
                          key={candidate}
                          closeOnSelect={false}
                          checked={current && model === candidate}
                          hint={candidate === choice.model ? "固定使用，不随服务默认变化" : undefined}
                          onSelect={() => chooseModel(choice, candidate)}
                        >
                          <span className="model-name" title={candidate}>{candidate}</span>
                        </MenuItem>
                      ))}
                    </div>
                  )}
                </section>
              );
            })}
          </>
        ) : view === "thinking" ? (
          renderOptionView(capabilities.thinking, "thinking", normalized.thinking)
        ) : view === "reasoning" ? (
          renderOptionView(capabilities.reasoning, "reasoning_effort", normalized.reasoning_effort)
        ) : (
          renderOptionView(capabilities.verbosity, "verbosity", normalized.verbosity)
        )}
      </Menu>

      {deepseekHover && balanceHover && balanceAnchorRef.current && (balanceState === "loading" || balanceState === "ready" || balanceState === "error") && (
        <AnchoredSurface
          anchorRef={balanceAnchorRef}
          placement={variant === "pill" ? "up" : "down"}
          align={variant === "pill" ? "left" : "right"}
          className="provider-balance-tooltip"
        >
          {balanceState === "loading" ? (
            <span className="provider-balance-loading">查询余额…</span>
          ) : balanceState === "error" ? (
            <span className="provider-balance-error">{balanceError ?? "余额查询失败"}</span>
          ) : balance ? (
            <>
              <span className="provider-balance-total">
                {balance.currency} {balance.total_balance}
              </span>
              {(balance.granted_balance || balance.topped_up_balance) && (
                <span className="provider-balance-detail">
                  赠送 {balance.granted_balance} · 充值 {balance.topped_up_balance}
                </span>
              )}
            </>
          ) : null}
        </AnchoredSurface>
      )}

      {(applyModel.error || saveInference.error) && (
        <StatusBar kind="error" compact onDismiss={() => { applyModel.clearError(); saveInference.clearError(); }}>
          {applyModel.error ?? saveInference.error}
        </StatusBar>
      )}
    </div>
  );
}


