/** 会话级 Provider、模型与模型专属推理参数的紧凑配置入口。 */
import { useMemo, useState } from "react";
import { taskSetInference, taskSetModel, taskSetProvider } from "../../lib/ipc";
import { useAsyncAction } from "../../lib/hooks";
import { rememberModel, resolveActive, type ProviderChoice } from "../../lib/provider";
import type { InferenceOptions } from "../../lib/types";
import { Menu, MenuEmpty, MenuItem, MenuSeparator } from "../ui/Menu";
import { StatusBar } from "../ui/StatusBar";
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
    setExpandedProvider(active.name || configuredChoices[0]?.name || null);
    setView("models");
  };

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
    if (running || !provider.ready) return;
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
    const next = { ...normalized };
    if (value) next[field] = value;
    else delete next[field];
    void saveInference.run(next);
    setView("root");
  };

  const summary = inferenceSummary(capabilities, normalized);
  const title = running
    ? "当前运行结束后可修改模型配置"
    : `${active.provider?.label ?? "未选择"} / ${active.model || "未配置"} / ${summary}`;
  const readyClass = active.provider?.ready ? " ready" : "";
  const trigger = variant === "pill" ? (
    <button type="button" className={`provider-pill model-config-trigger${readyClass}`} title={title} disabled={running}>
      <span>{active.provider?.label ?? "模型配置"}</span>
      <small>{active.model || "未配置"} · {summary}</small>
      <IconChevronDown width={12} height={12} />
    </button>
  ) : (
    <button type="button" className="room-provider-trigger" title={title} disabled={running}>
      <span>模型</span>
      <b>{active.provider?.label ?? "未选择"}</b>
      <small>{active.model || "未配置"} · {summary}</small>
    </button>
  );

  const renderOptionView = (
    control: CapabilityControl | undefined,
    field: "thinking" | "reasoning_effort" | "verbosity",
    current: string | null | undefined,
  ) => (
    <>
      <ConfigBack title={VIEW_TITLES[view as Exclude<View, "root">]} onBack={() => setView("root")} />
      {!control ? (
        <MenuEmpty>当前模型没有声明这项能力</MenuEmpty>
      ) : (
        <>
          <MenuItem closeOnSelect={false} checked={!current} onSelect={() => chooseOption(field, null)}>
            {control.defaultLabel}
          </MenuItem>
          {control.options.map((option) => (
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

  return (
    <div className="room-provider model-config-root">
      <Menu
        trigger={trigger}
        role="dialog"
        label="模型与推理配置"
        placement={variant === "pill" ? "up" : "down"}
        align={variant === "pill" ? "left" : "right"}
        disabled={running || applyModel.busy || saveInference.busy}
        menuClassName="model-menu model-config-menu"
        scroll
        openRequest={openRequest}
        onOpenChange={(open) => {
          if (!open) {
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
            {!normalized.thinking && !normalized.reasoning_effort && !normalized.verbosity ? null : (
              <>
                <MenuSeparator />
                <button className="model-config-reset" type="button" onClick={() => void saveInference.run({})}>
                  重置为服务默认
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

      {(applyModel.error || saveInference.error) && (
        <StatusBar kind="error" compact onDismiss={() => { applyModel.clearError(); saveInference.clearError(); }}>
          {applyModel.error ?? saveInference.error}
        </StatusBar>
      )}
    </div>
  );
}

function ConfigRow({ label, value, onSelect }: { label: string; value: string; onSelect: () => void }) {
  return (
    <button className="model-config-row ring-inset" type="button" onClick={onSelect}>
      <span>{label}</span><strong title={value}>{value}</strong><span aria-hidden="true">›</span>
    </button>
  );
}

function ConfigBack({ title, onBack }: { title: string; onBack: () => void }) {
  return (
    <button className="model-config-back ring-inset" type="button" onClick={onBack}>
      <span aria-hidden="true">←</span><strong>{title}</strong>
    </button>
  );
}
