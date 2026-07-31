import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { codexCliPreferences, codexSaveCliPreferences } from "../../lib/ipc";
import type { CodexCliPreferences, CodexModelOption } from "../../lib/types";
import { errText } from "../../lib/format";
import { Menu, MenuEmpty, MenuItem, MenuSeparator } from "../ui/Menu";
import { IconChevronDown } from "../icons";

interface Props {
  running: boolean;
  openRequest?: number;
  preload?: boolean;
  onPreferencesChange?: (preferences: CodexCliPreferences) => void;
  placement?: "up" | "down";
}

type View = "root" | "models" | "reasoning" | "verbosity";

const REASONING_LABELS: Record<string, string> = {
  none: "无",
  minimal: "最少",
  low: "低",
  medium: "中等",
  high: "高",
  xhigh: "极高",
  max: "最大",
  ultra: "超强",
};

const VERBOSITY = [
  { value: "low", label: "简洁" },
  { value: "medium", label: "适中" },
  { value: "high", label: "详细" },
];

function selectedModel(preferences: CodexCliPreferences | null): CodexModelOption | undefined {
  if (!preferences?.model) return undefined;
  return preferences.models.find((model) => model.slug === preferences.model);
}

function reasoningOptions(preferences: CodexCliPreferences | null): CodexModelOption["supported_reasoning_efforts"] {
  const selected = selectedModel(preferences);
  if (selected) return selected.supported_reasoning_efforts;
  const seen = new Set<string>();
  return (preferences?.models ?? []).flatMap((model) => model.supported_reasoning_efforts).filter((option) => {
    if (seen.has(option.effort)) return false;
    seen.add(option.effort);
    return true;
  });
}

export function CodexModelConfiguration({
  running,
  openRequest,
  preload = false,
  onPreferencesChange,
  placement = "up",
}: Props) {
  const [preferences, setPreferences] = useState<CodexCliPreferences | null>(null);
  const [view, setView] = useState<View>("root");
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const loadingRef = useRef(false);
  const preloadAttemptedRef = useRef(false);
  const model = selectedModel(preferences);
  const efforts = useMemo(() => reasoningOptions(preferences), [preferences]);

  const load = useCallback(async () => {
    if (loadingRef.current) return;
    loadingRef.current = true;
    setLoading(true);
    setError(null);
    try {
      const loaded = await codexCliPreferences();
      setPreferences(loaded);
      onPreferencesChange?.(loaded);
    } catch (cause) {
      setError(errText(cause));
    } finally {
      loadingRef.current = false;
      setLoading(false);
    }
  }, [onPreferencesChange]);

  useEffect(() => {
    if (!preload || preloadAttemptedRef.current) return;
    preloadAttemptedRef.current = true;
    void load();
  }, [load, preload]);

  const save = async (next: {
    model?: string | null;
    reasoning?: string | null;
    verbosity?: string | null;
  }) => {
    if (!preferences || saving) return;
    setSaving(true);
    setError(null);
    try {
      const updated = await codexSaveCliPreferences(
        next.model === undefined ? preferences.model ?? "" : next.model ?? "",
        next.reasoning === undefined ? preferences.reasoning_effort ?? "" : next.reasoning ?? "",
        next.verbosity === undefined ? preferences.verbosity ?? "" : next.verbosity ?? "",
        preferences.permission_mode,
      );
      setPreferences(updated);
      onPreferencesChange?.(updated);
      setView("root");
    } catch (cause) {
      setError(errText(cause));
    } finally {
      setSaving(false);
    }
  };

  const selectModel = (next: CodexModelOption) => {
    const currentEffort = preferences?.reasoning_effort;
    const compatible = !currentEffort
      || next.supported_reasoning_efforts.some((option) => option.effort === currentEffort);
    void save({ model: next.slug, reasoning: compatible ? currentEffort : null });
  };

  const modelLabel = model?.display_name ?? preferences?.model ?? "Codex 默认";
  const effortValue = preferences?.reasoning_effort;
  const effortLabel = effortValue
    ? REASONING_LABELS[effortValue] ?? effortValue
    : model?.default_reasoning_effort
      ? `默认（${REASONING_LABELS[model.default_reasoning_effort] ?? model.default_reasoning_effort}）`
      : "模型默认";
  const verbosityLabel = VERBOSITY.find((option) => option.value === preferences?.verbosity)?.label ?? "模型默认";

  return (
    <Menu
      trigger={
        <button className="provider-pill ready model-config-trigger" type="button" disabled={running} title="Codex CLI 模型与推理配置">
          <span>Codex 配置</span>
          <small>{preferences ? `${modelLabel} · ${effortLabel}` : "模型 · 推理"}</small>
          <IconChevronDown width={12} height={12} />
        </button>
      }
      role="dialog"
      label="Codex 模型与推理配置"
      placement={placement}
      align="left"
      disabled={running || saving}
      menuClassName="model-menu model-config-menu codex-model-config"
      scroll
      openRequest={openRequest}
      onOpenChange={(open) => {
        if (open && !preferences) void load();
        if (!open) setView("root");
      }}
    >
      {loading ? (
        <MenuEmpty>正在读取 Codex 可用模型…</MenuEmpty>
      ) : error && !preferences ? (
        <div className="model-config-error" role="alert">
          <span>{error}</span><button type="button" className="quiet-link" onClick={() => void load()}>重试</button>
        </div>
      ) : !preferences ? (
        <MenuEmpty>暂时无法读取 Codex 配置</MenuEmpty>
      ) : view === "root" ? (
        <>
          <div className="model-config-head">
            <div><strong>Codex CLI</strong><small>使用本机实际可用模型</small></div>
            {saving && <span>保存中…</span>}
          </div>
          <ConfigRow label="模型" value={modelLabel} onSelect={() => setView("models")} />
          <ConfigRow label="推理强度" value={effortLabel} onSelect={() => setView("reasoning")} />
          <ConfigRow label="输出详略" value={verbosityLabel} onSelect={() => setView("verbosity")} />
          <p className="model-config-note">保存到 Codex CLI 运行偏好；模型列表来自当前已登录的 CLI。</p>
          {(preferences.model || preferences.reasoning_effort || preferences.verbosity) && (
            <>
              <MenuSeparator />
              <button className="model-config-reset" type="button" onClick={() => void save({ model: null, reasoning: null, verbosity: null })}>
                重置为 Codex 默认
              </button>
            </>
          )}
          {error && <p className="model-config-inline-error" role="alert">{error}</p>}
        </>
      ) : view === "models" ? (
        <>
          <ConfigBack title="模型" onBack={() => setView("root")} />
          <MenuItem closeOnSelect={false} checked={!preferences.model} onSelect={() => void save({ model: null, reasoning: null })}>
            Codex 默认
          </MenuItem>
          {preferences.models.map((option) => (
            <MenuItem
              key={option.slug}
              closeOnSelect={false}
              checked={preferences.model === option.slug}
              hint={option.description || undefined}
              onSelect={() => selectModel(option)}
            >
              {option.display_name}
            </MenuItem>
          ))}
        </>
      ) : view === "reasoning" ? (
        <>
          <ConfigBack title="推理强度" onBack={() => setView("root")} />
          <MenuItem closeOnSelect={false} checked={!preferences.reasoning_effort} onSelect={() => void save({ reasoning: null })}>
            模型默认
          </MenuItem>
          {efforts.map((option) => (
            <MenuItem
              key={option.effort}
              closeOnSelect={false}
              checked={preferences.reasoning_effort === option.effort}
              hint={option.description || undefined}
              onSelect={() => void save({ reasoning: option.effort })}
            >
              {REASONING_LABELS[option.effort] ?? option.effort}
            </MenuItem>
          ))}
        </>
      ) : (
        <>
          <ConfigBack title="输出详略" onBack={() => setView("root")} />
          <MenuItem closeOnSelect={false} checked={!preferences.verbosity} onSelect={() => void save({ verbosity: null })}>
            模型默认
          </MenuItem>
          {VERBOSITY.map((option) => (
            <MenuItem
              key={option.value}
              closeOnSelect={false}
              checked={preferences.verbosity === option.value}
              onSelect={() => void save({ verbosity: option.value })}
            >
              {option.label}
            </MenuItem>
          ))}
        </>
      )}
    </Menu>
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
