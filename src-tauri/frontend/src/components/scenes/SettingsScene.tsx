import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import { errText } from "../../lib/format";
import { useAppStore, type SettingsPane } from "../../store/app";
import { usePoll } from "../../lib/poll";
import {
  codexCliPreferences,
  codexIntegrationStatus,
  codexSaveCliPreferences,
  codexSetupCollaboration,
  codexStartDeviceLogin,
  codexStartLogin,
  logsTail,
  providerModels,
  settingsDeleteProvider,
  settingsGet,
  settingsSaveProvider,
  settingsSelectProvider,
  settingsSet,
  supportBundle,
  supportPreview,
} from "../../lib/ipc";
import type {
  AppConfig,
  CodexCliPreferences,
  CodexIntegrationStatus,
  CodexModelOption,
  LogEntry,
  ProviderCategory,
  ProviderConfig,
  ProviderPreset,
  ProviderProtocol,
  ProviderStatus,
  SupportBundlePreview,
} from "../../lib/types";
import { clockTime } from "../../lib/format";
import { catalogPresets, loadCatalog, presetOf, providerLabel } from "../../lib/provider";
import { useCodexCliGate } from "../codex/CodexCliGate";
import { IconCheck, IconRefresh } from "../icons";

const LOG_LEVELS = ["debug", "info", "warn", "error"];
const LOG_FILTERS = ["all", "error", "warn", "info", "debug"] as const;
const EMPTY_PROVIDERS: NonNullable<AppConfig["providers"]> = {};
const OUTPUT_DEFAULT = "8192";
/** 自建网关：不套用任何预设，全部字段手填。 */
const CUSTOM_PRESET = "custom";

const SETTINGS_PANES: Array<{
  key: SettingsPane;
  label: string;
  description: string;
}> = [
  { key: "providers", label: "模型服务", description: "配置 R-Code 对话使用的模型与凭据。" },
  { key: "preferences", label: "应用偏好", description: "调整外观、缩放和辅助阅读方式。" },
  { key: "diagnostics", label: "诊断", description: "查看运行日志，或导出脱敏支持信息。" },
  { key: "codex", label: "Codex CLI", description: "连接本机 Codex，并管理它的运行偏好。" },
];

const CATEGORY_LABELS: Record<ProviderCategory, string> = {
  official: "海外官方",
  cn_official: "国内厂商",
  cloud_provider: "云厂商托管",
  aggregator: "路由 / 聚合",
};

const PROTOCOL_LABELS: Record<ProviderProtocol, string> = {
  anthropic_messages: "Anthropic Messages",
  openai_chat: "OpenAI Chat Completions",
  openai_responses: "OpenAI Responses",
};

/** 按 category 分组，保持目录里的原始顺序。 */
function groupByCategory(presets: ProviderPreset[]) {
  const groups = new Map<ProviderCategory, ProviderPreset[]>();
  for (const preset of presets) {
    const bucket = groups.get(preset.category);
    if (bucket) bucket.push(preset);
    else groups.set(preset.category, [preset]);
  }
  return [...groups.entries()];
}

const ALL_PROTOCOLS: ProviderProtocol[] = ["openai_chat", "anthropic_messages", "openai_responses"];

/**
 * 新建（还没有后端状态）时下拉框的初值。
 *
 * 与后端 `infer_protocol_never_responses` 同规则：预设推荐值，但 Responses 降级为
 * Chat。Responses 与 Chat 常在同一地址上都可用而计费不同，必须由用户主动选。
 */
function fallbackProtocol(preset: ProviderPreset | undefined): ProviderProtocol {
  const inferred = preset?.protocol ?? "openai_chat";
  return inferred === "openai_responses" ? "openai_chat" : inferred;
}

const normalizeUrl = (url: string) => url.trim().replace(/\/+$/, "").toLowerCase();

/**
 * 该地址允许选哪些协议，`null` = 目录管不到、不设限。
 *
 * 必须与后端 `provider_catalog::allowed_protocols` 逐条对齐，否则 UI 会拦下后端愿意
 * 接受的选择、或者放行后端会拒绝的。规则：主入口给 `native`；备用线路只给它自己
 * 那一个（我们对候选地址的了解仅限目录里写的那条）；改到目录以外则不设限。
 */
function allowedProtocols(
  preset: ProviderPreset | undefined,
  baseUrl: string
): ProviderProtocol[] | null {
  if (!preset) return null;
  const url = normalizeUrl(baseUrl);
  // 留空 = 保存时回填预设地址，按主入口算
  if (!url || url === normalizeUrl(preset.base_url)) return preset.native;
  const candidate = preset.endpoint_candidates.find((item) => normalizeUrl(item.url) === url);
  return candidate ? candidate.native : null;
}

function protocolChoices(
  preset: ProviderPreset | undefined,
  baseUrl: string,
  current: ProviderProtocol
): ProviderProtocol[] {
  const choices = allowedProtocols(preset, baseUrl) ?? ALL_PROTOCOLS;
  // 当前值必须在选项里，否则 <select> 会显示第一项而 state 仍是旧值，
  // 用户看到的和即将提交的对不上。
  return choices.includes(current) ? [...choices] : [...choices, current];
}

/** base_url 里还有没填的 `${VAR}` 占位符。 */
function unresolvedTemplateVars(preset: ProviderPreset | undefined, baseUrl: string) {
  if (!preset) return [];
  return preset.template_vars.filter((variable) => baseUrl.includes(`\${${variable.name}}`));
}

function optionalInteger(value: string) {
  const normalized = value.trim();
  if (!normalized) return null;
  const parsed = Number(normalized);
  if (!Number.isInteger(parsed) || parsed <= 0) {
    throw new Error("最大输出 Token 必须是大于 0 的整数");
  }
  return parsed;
}

function optionalDecimal(value: string) {
  const normalized = value.trim();
  if (!normalized) return null;
  const parsed = Number(normalized);
  if (!Number.isFinite(parsed)) throw new Error("随机性必须是数字");
  return parsed;
}

function displayNumber(value: number | undefined) {
  if (value == null) return "";
  return Number(value.toFixed(4)).toString();
}

function isDeepSeekV4(baseUrl: string, model: string, preset: string) {
  return (preset === "deepseek" || baseUrl.includes("api.deepseek.com")) &&
    model.trim().toLowerCase().startsWith("deepseek-v4-");
}

function providerStateLabel(status: ProviderStatus | undefined) {
  if (!status?.ready) return "待完成";
  return status.source === "environment" ? "环境变量" : "可使用";
}

/**
 * 设置页：模型服务、外观、无障碍、日志、支持包与外部 Agent。
 * settingsGet 失败（配置损坏等）时表单区显示错误条而非空白。
 */
export function SettingsScene() {
  const activePane = useAppStore((state) => state.settingsPane);
  const setActivePane = useAppStore((state) => state.setSettingsPane);
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [configErr, setConfigErr] = useState<string | null>(null);
  const [validation, setValidation] = useState<string | null>(null);
  const [providerStatus, setProviderStatus] = useState<Record<string, ProviderStatus>>({});

  const loadConfig = useCallback(async () => {
    try {
      const res = await settingsGet();
      setConfig(res.config);
      setValidation(res.validation);
      setProviderStatus(res.provider_status ?? {});
      setConfigErr(null);
    } catch (e) {
      setConfigErr(errText(e));
    }
  }, []);

  useEffect(() => {
    void loadConfig();
  }, [loadConfig]);

  const pane = SETTINGS_PANES.find((item) => item.key === activePane) ?? SETTINGS_PANES[0];

  return (
    <div className="scene">
      <div className="scene-scroll">
        <div className="page-head">
          <h1>设置</h1>
        </div>

        <div className="settings-layout">
          <nav className="settings-nav" aria-label="设置分类">
            {SETTINGS_PANES.map((item) => (
              <button
                key={item.key}
                className={activePane === item.key ? "active" : ""}
                aria-current={activePane === item.key ? "page" : undefined}
                onClick={() => setActivePane(item.key)}
              >
                {item.label}
              </button>
            ))}
          </nav>

          <div className="settings-detail">
            <header className="settings-detail-head">
              <h2>{pane.label}</h2>
              <p>{pane.description}</p>
            </header>

            {configErr && (activePane === "providers" || activePane === "diagnostics") && (
              <div className="errbar" role="alert">
                读取配置失败：{configErr}
                <span className="spacer" />
                <button className="btn" onClick={() => void loadConfig()}>
                  重试
                </button>
              </div>
            )}
            {activePane === "providers" && validation && !configErr && (
              <div className="notebar" role="status">
                选择模型服务并保存访问密钥后即可开始对话。
                <span className="dim">{validation}</span>
              </div>
            )}

            {activePane === "providers" && (
              <div className="settings-sheet">
                {config ? (
                  <ProviderSection config={config} providerStatus={providerStatus} reload={loadConfig} />
                ) : (
                  !configErr && <div className="settings-loading">正在读取模型服务…</div>
                )}
              </div>
            )}

            {activePane === "preferences" && (
              <div className="settings-sheet">
                <AppearanceSection />
                <AccessibilitySection />
              </div>
            )}

            {activePane === "diagnostics" && (
              <div className="settings-sheet">
                {config && <LogLevelSection config={config} reload={loadConfig} />}
                <LogSection />
                <SupportSection />
              </div>
            )}

            {activePane === "codex" && (
              <div className="settings-sheet">
                <CodexIntegrationSection />
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

// ---------- Provider ----------

function ProviderSection({
  config,
  providerStatus,
  reload,
}: {
  config: AppConfig;
  providerStatus: Record<string, ProviderStatus>;
  reload: () => Promise<void>;
}) {
  const configDefault = config.default_provider ?? "";
  const providers = config.providers ?? EMPTY_PROVIDERS;
  const names = Object.keys(providers).sort((a, b) => a.localeCompare(b));
  const [catalog, setCatalog] = useState<ProviderPreset[]>([]);
  const [selectedProvider, setSelectedProvider] = useState<string | null>(null);
  const [presetName, setPresetName] = useState(CUSTOM_PRESET);
  const [profileName, setProfileName] = useState("");
  const [fields, setFields] = useState({
    base_url: "",
    model: "",
    max_tokens: OUTPUT_DEFAULT,
    temperature: "0.2",
    protocol: "openai_chat" as ProviderProtocol,
  });
  const [keyInput, setKeyInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const [remoteModels, setRemoteModels] = useState<string[]>([]);
  const [modelsBusy, setModelsBusy] = useState(false);
  const [modelsMessage, setModelsMessage] = useState<string | null>(null);
  const [modelsError, setModelsError] = useState<string | null>(null);
  const modelRequest = useRef(0);

  // 目录来自后端 provider_catalog.rs：预设一旦分散成两份就会漂移，
  // 前端不再自带硬编码表。
  useEffect(() => {
    let alive = true;
    void loadCatalog().then(() => {
      if (!alive) return;
      const presets = catalogPresets();
      setCatalog(presets);
      setPresetName((current) =>
        current === CUSTOM_PRESET && presets.length > 0 ? presets[0].id : current
      );
    });
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => {
    setSelectedProvider((current) => {
      if (current && providers[current]) return current;
      if (configDefault && providers[configDefault]) return configDefault;
      return names[0] ?? null;
    });
  }, [configDefault, names.join("|")]);

  const applyPreset = useCallback((nextPreset: string) => {
    const preset = presetOf(nextPreset);
    setPresetName(nextPreset);
    setProfileName(nextPreset === CUSTOM_PRESET ? "" : nextPreset);
    setFields({
      base_url: preset?.base_url ?? "",
      model: preset?.model ?? "",
      // 预设声明了单次输出上限时用它，避免保存后被服务端 400
      max_tokens: preset?.max_output_tokens != null
        ? String(Math.min(preset.max_output_tokens, Number(OUTPUT_DEFAULT)))
        : OUTPUT_DEFAULT,
      temperature: "0.2",
      // 新建同样不预选 Responses：下拉框里"看得见"不等于用户确认过。想用 Responses
      // 就自己去选一下，这条规矩对新建和编辑一视同仁。
      protocol: fallbackProtocol(preset),
    });
  }, []);

  useEffect(() => {
    if (!selectedProvider) {
      setKeyInput("");
      setSaved(null);
      setErr(null);
      applyPreset(presetName);
      return;
    }
    const profile = providers[selectedProvider] as ProviderConfig | undefined;
    const preset = presetOf(selectedProvider);
    setProfileName(selectedProvider);
    setPresetName(preset?.id ?? CUSTOM_PRESET);
    setFields({
      base_url: profile?.base_url ?? preset?.base_url ?? "",
      model: profile?.model ?? preset?.model ?? "",
      max_tokens: profile?.max_tokens != null ? String(profile.max_tokens) : OUTPUT_DEFAULT,
      temperature: displayNumber(profile?.temperature) || "0.2",
      // 编辑已有配置时以后端算出的 effective_protocol 为准——它已经把"存过的值"
      // 和"地址被改写后的推断"都算进去了。前端再推一遍只会和后端对不上，而用户
      // 随手点个保存就会把错的那个存下来。
      protocol:
        profile?.protocol ??
        providerStatus[selectedProvider]?.effective_protocol ??
        fallbackProtocol(preset),
    });
    setKeyInput("");
    setSaved(null);
    setErr(null);
  }, [applyPreset, providers, providerStatus, selectedProvider]);

  // 地址、协议或编辑对象变化后，旧请求结果不再属于当前表单。
  useEffect(() => {
    modelRequest.current += 1;
    setRemoteModels([]);
    setModelsMessage(null);
    setModelsError(null);
    setModelsBusy(false);
  }, [selectedProvider, presetName, fields.base_url, fields.protocol]);

  const activePreset = presetOf(presetName);
  const pendingVars = unresolvedTemplateVars(activePreset, fields.base_url);
  const modelChoices = Array.from(
    new Set(
      [fields.model, ...remoteModels, ...(activePreset?.models ?? [])]
        .map((model) => model.trim())
        .filter(Boolean)
    )
  );

  const fetchModels = async () => {
    if (modelsBusy || busy) return;
    const requestId = ++modelRequest.current;
    setModelsBusy(true);
    setModelsMessage(null);
    setModelsError(null);
    try {
      const response = await providerModels({
        name: profileName.trim(),
        preset: activePreset?.id ?? null,
        baseUrl: fields.base_url,
        apiKey: keyInput.trim() || null,
        protocol: fields.protocol,
      });
      if (modelRequest.current !== requestId) return;
      setRemoteModels(response.models);
      if (!fields.model.trim() && response.models[0]) {
        setFields((value) => ({ ...value, model: response.models[0] }));
      }
      setModelsMessage(`服务返回 ${response.models.length} 个可用模型`);
    } catch (cause) {
      if (modelRequest.current !== requestId) return;
      setModelsError(errText(cause));
    } finally {
      if (modelRequest.current === requestId) setModelsBusy(false);
    }
  };

  const run = async (fn: () => Promise<void>, message: string) => {
    if (busy) return;
    setBusy(true);
    setErr(null);
    setSaved(null);
    try {
      await fn();
      await reload();
      setSaved(message);
    } catch (e) {
      setErr(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const saveProvider = (activate: boolean) =>
    void run(async () => {
      const name = profileName.trim();
      if (!name) throw new Error("请为这项配置填写名称");
      await settingsSaveProvider({
        name,
        baseUrl: fields.base_url,
        model: fields.model,
        apiKey: keyInput.trim() || null,
        maxTokens: optionalInteger(fields.max_tokens),
        temperature: optionalDecimal(fields.temperature),
        protocol: fields.protocol,
        activate,
      });
      setSelectedProvider(name);
      setKeyInput("");
    }, activate ? "已保存，并用于后续新对话" : "配置已保存");

  const selectProvider = (name: string) =>
    void run(() => settingsSelectProvider(name), "已切换，新对话将使用这项服务");

  const deleteProvider = (name: string) => {
    if (!window.confirm(`删除“${providerLabel(name)}”及其本机凭据？此操作无法撤销。`)) return;
    void run(async () => {
      await settingsDeleteProvider(name);
      if (selectedProvider === name) setSelectedProvider(null);
    }, "配置已删除");
  };

  const editing = selectedProvider ? (providers[selectedProvider] as ProviderConfig | undefined) : undefined;
  const credential = selectedProvider ? providerStatus[selectedProvider] : undefined;
  const credentialLabel = credential?.configured
    ? credential.source === "environment"
      ? "由环境变量提供"
      : "已安全保存"
    : "尚未保存";
  const deepSeekV4 = isDeepSeekV4(fields.base_url, fields.model, presetName);
  const outputValue = Number(fields.max_tokens.trim());
  const outputExceedsDeepSeekLimit = deepSeekV4 && Number.isFinite(outputValue) && outputValue > 393_216;
  // 占位符没替换就保存 = 一个必然 404 的地址进了配置
  const saveBlocked = busy || outputExceedsDeepSeekLimit || pendingVars.length > 0;

  return (
    <section className="settings-block provider-settings">
      <div className="section-heading">
        <div>
          <h3>对话模型</h3>
          <p className="desc">R-Code 对话使用的模型服务。密钥仅保存在系统凭据库。</p>
        </div>
        <button
          className="btn"
          disabled={busy}
          onClick={() => {
            setSelectedProvider(null);
            applyPreset(catalog[0]?.id ?? CUSTOM_PRESET);
          }}
        >
          新建服务
        </button>
      </div>

      {err && <div className="errbar" role="alert">{err}</div>}
      {saved && <div className="okbar" role="status">{saved}</div>}

      <div className="provider-layout">
        <div className="provider-list" aria-label="已保存的模型服务">
          <div className="provider-list-label">已保存的服务</div>
          {names.length === 0 ? (
            <div className="provider-empty">还没有服务。选择一个预设，填入密钥即可开始聊天。</div>
          ) : (
            names.map((name) => {
              const profile = providers[name] as ProviderConfig;
              const active = name === configDefault;
              const status = providerStatus[name];
              return (
                <button
                  key={name}
                  className={`provider-row${name === selectedProvider ? " selected" : ""}`}
                  disabled={busy}
                  onClick={() => setSelectedProvider(name)}
                >
                  <span className="provider-row-title">
                    {providerLabel(name)}
                    {active && <em>正在使用</em>}
                  </span>
                  <span className="provider-row-model">{profile.model || "尚未设置模型"}</span>
                  <span className={`provider-row-state${status?.ready ? " ready" : ""}`}>{providerStateLabel(status)}</span>
                </button>
              );
            })
          )}
        </div>

        <div className="provider-editor">
          <div className="provider-editor-head">
            <div>
              <span className="provider-editor-kicker">{editing ? "编辑服务" : "新建服务"}</span>
              <h4>{editing ? providerLabel(selectedProvider ?? "") : "添加一个模型服务"}</h4>
            </div>
            {editing && selectedProvider && selectedProvider !== configDefault && (
              <button className="quiet-link danger-link" disabled={busy} onClick={() => deleteProvider(selectedProvider)}>
                删除
              </button>
            )}
          </div>

          <div className="field">
            <label htmlFor="set-preset">预设</label>
            <select id="set-preset"
              className="input"
              value={presetName}
              disabled={busy || Boolean(editing)}
              onChange={(event) => applyPreset(event.target.value)}
            >
              {groupByCategory(catalog).map(([category, presets]) => (
                <optgroup key={category} label={CATEGORY_LABELS[category] ?? category}>
                  {presets.map((preset) => (
                    <option key={preset.id} value={preset.id}>{preset.label}</option>
                  ))}
                </optgroup>
              ))}
              <option value={CUSTOM_PRESET}>自建 / 其它 OpenAI 兼容接口</option>
            </select>
            {activePreset && (
              <span className="hint">
                {`推荐 ${PROTOCOL_LABELS[activePreset.protocol]}`}
                {activePreset.context_window != null &&
                  ` · 上下文 ${activePreset.context_window.toLocaleString()}`}
                {activePreset.api_key_url && (
                  <>
                    {" · "}
                    <a href={activePreset.api_key_url} target="_blank" rel="noreferrer">获取密钥</a>
                  </>
                )}
              </span>
            )}
            {activePreset?.note && <span className="field-warning">{activePreset.note}</span>}
            {editing && <span className="hint">已有服务保留其当前设置；如需新预设，请新建一项。</span>}
          </div>
          <div className="field">
            <label htmlFor="set-profile-name">配置名称</label>
            <input id="set-profile-name"
              className="input"
              value={profileName}
              readOnly={Boolean(editing)}
              placeholder="例如：DeepSeek 工作账户"
              onChange={(event) => setProfileName(event.target.value)}
            />
            {editing && <span className="hint">名称用于区分配置；需要改名时新建后删除旧项。</span>}
          </div>
          <div className="field">
            <label htmlFor="set-base-url">接口地址</label>
            <input id="set-base-url"
              className="input"
              value={fields.base_url}
              placeholder="https://api.example.com/v1"
              onChange={(event) => setFields((value) => ({ ...value, base_url: event.target.value }))}
            />
            <span className="hint">填写服务根地址，不要填写完整的 /chat/completions 路径。</span>
            {pendingVars.length > 0 && (
              <span className="field-warning">
                地址里还有占位符待替换：
                {pendingVars.map((variable) => `\${${variable.name}}（${variable.label}）`).join("、")}
              </span>
            )}
            {activePreset && activePreset.endpoint_candidates.length > 0 && (
              <span className="hint">
                备用线路：
                {activePreset.endpoint_candidates.map((candidate, index) => (
                  <Fragment key={candidate.url}>
                    {index > 0 && " · "}
                    <button
                      className="quiet-link"
                      type="button"
                      disabled={busy}
                      title={`${candidate.url}（${PROTOCOL_LABELS[candidate.protocol]}）`}
                      // 协议必须跟着地址一起切：多数备用线路是同一厂商的另一个协议口，
                      // 只改地址会把 Anthropic 的请求发到一个只有 Chat 的 endpoint 上。
                      onClick={() =>
                        setFields((value) => ({
                          ...value,
                          base_url: candidate.url,
                          protocol: candidate.protocol,
                        }))
                      }
                    >
                      {candidate.label}
                    </button>
                  </Fragment>
                ))}
              </span>
            )}
          </div>
          <div className="field">
            <label htmlFor="set-protocol">线路协议</label>
            <select id="set-protocol"
              className="input"
              disabled={busy}
              value={fields.protocol}
              onChange={(event) =>
                setFields((value) => ({
                  ...value,
                  protocol: event.target.value as ProviderProtocol,
                }))
              }
            >
              {protocolChoices(activePreset, fields.base_url, fields.protocol).map((protocol) => (
                <option key={protocol} value={protocol}>{PROTOCOL_LABELS[protocol]}</option>
              ))}
            </select>
            <span className="hint">
              决定请求体形状与鉴权头。同一地址支持多种协议时计费和能力可能不同，由你决定走哪个。
            </span>
            {fields.protocol === "openai_responses" && (
              <span className="field-warning">
                Responses 仅部分服务实现完整；不确定时选 Chat Completions。
                {activePreset && !activePreset.reasoning_replay &&
                  " 该服务不支持加密推理回放，多轮工具调用间的思维链不连续。"}
              </span>
            )}
            {(() => {
              const allowed = allowedProtocols(activePreset, fields.base_url);
              if (!allowed) {
                return activePreset ? (
                  <span className="hint">
                    地址已改写，协议不再受预设约束——请按你这条线路实际实现的接口选择。
                  </span>
                ) : null;
              }
              if (allowed.includes(fields.protocol)) return null;
              return (
                <span className="field-warning">
                  该地址不支持这个协议，保存会被拒绝。可选：
                  {allowed.map((protocol) => PROTOCOL_LABELS[protocol]).join(" / ")}
                </span>
              );
            })()}
          </div>
          <div className="field">
            <label htmlFor="set-model">模型</label>
            <div className="provider-model-controls">
              {/* 保留自由输入：并不是所有兼容网关都实现 /models。 */}
              <input id="set-model"
                className="input"
                value={fields.model}
                placeholder="模型名称"
                onChange={(event) => setFields((value) => ({ ...value, model: event.target.value }))}
              />
              <select
                className="input provider-model-select"
                aria-label="从模型列表选择"
                title="从模型列表选择"
                value=""
                disabled={busy || modelChoices.length === 0}
                onChange={(event) => {
                  if (event.target.value) {
                    setFields((value) => ({ ...value, model: event.target.value }));
                  }
                }}
              >
                <option value="">选择模型（{modelChoices.length}）</option>
                {modelChoices.map((model) => (
                  <option key={model} value={model}>{model}</option>
                ))}
              </select>
              <button
                className={`btn provider-model-refresh${modelsBusy ? " loading" : ""}`}
                type="button"
                disabled={busy || modelsBusy || pendingVars.length > 0 || !fields.base_url.trim()}
                title="从当前接口实时获取模型列表"
                onClick={() => void fetchModels()}
              >
                <IconRefresh width={15} height={15} />
                {modelsBusy ? "获取中" : "获取列表"}
              </button>
            </div>
            <span className="hint">可从当前接口实时获取并选择；接口不支持时仍可直接填写。</span>
            {modelsMessage && <span className="field-success" role="status">{modelsMessage}</span>}
            {modelsError && <span className="field-warning" role="alert">{modelsError}</span>}
          </div>
          <div className="field">
            <label htmlFor="set-api-key">访问密钥</label>
            <span className="val">{credentialLabel}</span>
            <input id="set-api-key"
              className="input"
              type="password"
              placeholder={credential?.configured ? "留空则保留当前密钥" : "粘贴访问密钥"}
              value={keyInput}
              onChange={(event) => setKeyInput(event.target.value)}
            />
          </div>
          <div className="field token-field">
            <label htmlFor="set-max-tokens">最大输出</label>
            <input id="set-max-tokens"
              className="input"
              inputMode="numeric"
              value={fields.max_tokens}
              onChange={(event) => setFields((value) => ({ ...value, max_tokens: event.target.value }))}
            />
            <span className="hint">
              {deepSeekV4
                ? "单次回复上限。V4 的上下文为 1,000,000，最大输出为 393,216；通常建议 8,192。"
                : "单次回复上限，不是上下文窗口。通常 8,192 已足够。"}
            </span>
            {outputExceedsDeepSeekLimit && (
              <span className="field-warning">
                当前旧值超出服务限制；请求会暂时按 393,216 发送。请改为建议值后保存。
                <button className="quiet-link" type="button" onClick={() => setFields((value) => ({ ...value, max_tokens: OUTPUT_DEFAULT }))}>
                  恢复为 8,192
                </button>
              </span>
            )}
          </div>
          <div className="field">
            <label htmlFor="set-temperature">随机性</label>
            <input id="set-temperature"
              className="input"
              inputMode="decimal"
              value={fields.temperature}
              onChange={(event) => setFields((value) => ({ ...value, temperature: event.target.value }))}
            />
            <span className="hint">建议 0.1–0.3，用于稳定的编码与问答。</span>
          </div>

          <div className="footbar provider-actions">
            {editing && selectedProvider && selectedProvider !== configDefault && (
              <button className="btn" disabled={busy || !providerStatus[selectedProvider]?.ready} onClick={() => selectProvider(selectedProvider)}>
                用于新对话
              </button>
            )}
            <span className="spacer" />
            <button className="btn" disabled={saveBlocked} onClick={() => saveProvider(false)}>保存</button>
            <button className="btn accent" disabled={saveBlocked} onClick={() => saveProvider(true)}>保存并用于新对话</button>
          </div>
        </div>
      </div>
    </section>
  );
}

// ---------- 通用 ----------

function LogLevelSection({ config, reload }: { config: AppConfig; reload: () => Promise<void> }) {
  const [err, setErr] = useState<string | null>(null);

  const setLevel = async (v: string) => {
    setErr(null);
    try {
      await settingsSet("log_level", v);
      await reload();
    } catch (e) {
      setErr(errText(e));
    }
  };

  return (
    <section className="settings-block">
      <h3>日志记录</h3>
      {err && <div className="errbar" role="alert">{err}</div>}
      <div className="field">
        <label htmlFor="set-log-level">记录级别</label>
        <select id="set-log-level"
          className="input"
          value={config.log_level ?? "info"}
          onChange={(e) => void setLevel(e.target.value)}
        >
          {LOG_LEVELS.map((l) => (
            <option key={l} value={l}>
              {l}
            </option>
          ))}
        </select>
      </div>
    </section>
  );
}

// ---------- 外观 ----------

function AppearanceSection() {
  const themeMode = useAppStore((s) => s.themeMode);
  const setThemeMode = useAppStore((s) => s.setThemeMode);
  const zoomLevel = useAppStore((s) => s.zoomLevel);
  const setZoom = useAppStore((s) => s.setZoom);
  const zoomReset = useAppStore((s) => s.zoomReset);

  const modes: { key: "light" | "dark" | "system"; label: string; hint: string }[] = [
    { key: "light", label: "亮色", hint: "干净的浅色界面" },
    { key: "dark", label: "暗色", hint: "适合低光环境" },
    { key: "system", label: "跟随系统", hint: "随操作系统明暗切换" },
  ];

  return (
    <section className="settings-block">
      <h3>外观</h3>
      <div className="field">
        <label id="set-theme-label">主题</label>
        <div className="chips" role="radiogroup" aria-labelledby="set-theme-label">
          {modes.map((m) => (
            <button
              key={m.key}
              role="radio"
              aria-checked={themeMode === m.key}
              className={`chipbtn${themeMode === m.key ? " on" : ""}`}
              onClick={() => setThemeMode(m.key)}
              title={m.hint}
            >
              {m.label}
            </button>
          ))}
        </div>
      </div>
      <div className="field">
        <label htmlFor="set-zoom">界面缩放</label>
        <input id="set-zoom"
          type="range"
          min={80}
          max={200}
          step={10}
          value={zoomLevel}
          onChange={(e) => setZoom(Number(e.target.value))}
        />
        <span className="val">{zoomLevel}%</span>
        <button className="btn ghost" onClick={zoomReset}>
          复位
        </button>
      </div>
    </section>
  );
}

// ---------- 无障碍 ----------

function AccessibilitySection() {
  const accessibleDiffMode = useAppStore((s) => s.accessibleDiffMode);
  const toggleDiffMode = useAppStore((s) => s.toggleDiffMode);

  return (
    <section className="settings-block">
      <h3>无障碍</h3>
      <div className="field">
        <label htmlFor="set-diff-mode">文本差异视图</label>
        <input id="set-diff-mode"
          className="switch"
          type="checkbox"
          role="switch"
          aria-label="文本差异视图"
          checked={accessibleDiffMode}
          onChange={toggleDiffMode}
        />
        <span className="hint">以文本列表呈现文件变更；使用 F7 和 Shift + F7 在变更间导航。</span>
      </div>
    </section>
  );
}

// ---------- 日志查看器 ----------

function LogSection() {
  const [filter, setFilter] = useState<(typeof LOG_FILTERS)[number]>("all");
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const boxRef = useRef<HTMLDivElement>(null);

  usePoll(async () => {
    try {
      setLogs(await logsTail(200, filter === "all" ? undefined : filter));
      setErr(null);
    } catch (e) {
      setErr(errText(e));
    }
  }, 1500);

  // 仅当用户停留在底部附近时才跟随最新日志
  useEffect(() => {
    const el = boxRef.current;
    if (!el) return;
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 48) {
      el.scrollTop = el.scrollHeight;
    }
  }, [logs]);

  return (
    <section className="settings-block">
      <h3>实时日志</h3>
      <div className="field">
        <div className="chips" role="radiogroup" aria-label="日志级别过滤">
          {LOG_FILTERS.map((l) => (
            <button
              key={l}
              role="radio"
              aria-checked={filter === l}
              className={`chipbtn${filter === l ? " on" : ""}`}
              onClick={() => setFilter(l)}
            >
              {l === "all" ? "全部" : l}
            </button>
          ))}
        </div>
      </div>
      {err && <div className="errbar" role="alert">{err}</div>}
      <div className="logbox" role="log" aria-live="off" ref={boxRef}>
        {logs.length === 0 ? (
          <div className="empty">暂无日志</div>
        ) : (
          logs.map((l, i) => (
            <div className="logline" key={i}>
              <span className="t">{clockTime(l.timestamp)}</span>
              <span className={`lv ${l.level.toLowerCase()}`}>{l.level}</span>
              <span className="tg">{l.target}</span>
              <span className="msg">{l.message}</span>
            </div>
          ))
        )}
      </div>
    </section>
  );
}

// ---------- 支持包 ----------

function SupportSection() {
  const [preview, setPreview] = useState<SupportBundlePreview | null>(null);
  const [outDir, setOutDir] = useState("%APPDATA%/r-code/support");
  const [bundlePath, setBundlePath] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const doPreview = async () => {
    setBusy(true);
    setErr(null);
    try {
      setPreview(await supportPreview());
    } catch (e) {
      setErr(`预览失败：${errText(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const doExport = async () => {
    const dir = outDir.trim();
    if (!dir) return;
    setBusy(true);
    setErr(null);
    setBundlePath(null);
    try {
      setBundlePath(await supportBundle(dir));
    } catch (e) {
      setErr(`导出失败：${errText(e)}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="settings-block">
      <h3>支持包</h3>
      <p className="desc">导出版本、平台、近期日志和本地统计，便于提交问题；预览不会写入文件。</p>
      {err && <div className="errbar" role="alert">{err}</div>}
      <div className="footbar">
        <button className="btn" disabled={busy} onClick={() => void doPreview()}>
          生成预览
        </button>
      </div>
      {preview && (
        <dl className="kv">
          <dt>版本</dt>
          <dd>{preview.version}</dd>
          <dt>平台</dt>
          <dd>{preview.platform}</dd>
          <dt>生成时间</dt>
          <dd>{preview.generated_at}</dd>
          <dt>日志条数</dt>
          <dd>{preview.logs.length}</dd>
          <dt>本地统计</dt>
          <dd>
            任务 {preview.db_stats.task_count}，运行 {preview.db_stats.run_count}，工具调用{" "}
            {preview.db_stats.tool_call_count}
          </dd>
        </dl>
      )}
      <div className="field export-row">
        <label htmlFor="set-output-dir">输出目录</label>
        <input id="set-output-dir" className="input" value={outDir} onChange={(e) => setOutDir(e.target.value)} />
        <button className="btn accent" disabled={busy || !outDir.trim()} onClick={() => void doExport()}>
          导出
        </button>
      </div>
      {bundlePath && (
        <div className="okbar" role="status">
          已生成：<span className="val">{bundlePath}</span>
        </div>
      )}
    </section>
  );
}

// ---------- 外部 Agent ----------

type CodexSetupState = NonNullable<CodexIntegrationStatus["setup_state"]>;

function resolveCodexSetupState(status: CodexIntegrationStatus): CodexSetupState {
  if (status.setup_state) return status.setup_state;
  if (!status.cli_available) return "install_cli";
  if (status.auth_status === "not_authenticated") return "login";
  if (status.auth_status !== "authenticated") return "check";
  if (status.skill_status !== "up_to_date" || !status.mcp_server_configured) return "configure";
  return "ready";
}

function codexSetupCopy(status: CodexIntegrationStatus | null, state: CodexSetupState | "loading") {
  if (!status || state === "loading") {
    return { title: "正在检测 Codex", detail: "检查 CLI、登录和协作配置。", action: "正在检测…" };
  }
  if (state === "install_cli") {
    return {
      title: "还需要安装 Codex CLI",
      detail: status.installer_available === false
        ? "当前没有可用的 npm，请按下方说明手动安装。"
        : "R-Code 会先展示官方安装命令，确认后再执行。",
      action: status.installer_available === false ? "无法自动安装" : "安装并继续",
    };
  }
  if (state === "login") {
    return { title: "还需要登录 Codex", detail: "使用浏览器登录；设备码仅在浏览器回调不可用时使用。", action: "登录并继续" };
  }
  if (state === "check") {
    return { title: "暂时无法确认登录状态", detail: "不会重复打开登录页，先重新读取 Codex 的认证状态。", action: "重新检测" };
  }
  if (state === "configure") {
    return { title: "还差最后一步", detail: "一次更新协作 Skill，并补齐 R-Code 只读 MCP 配置。", action: "完成协作配置" };
  }
  return {
    title: "Codex 已就绪",
    detail: `已通过${status.auth_method ? ` ${status.auth_method}` : " Codex"} 登录，R-Code 只读协作已连接。`,
    action: "已就绪",
  };
}

type CodexPreferenceDraft = {
  model: string;
  reasoningEffort: string;
  verbosity: string;
};

const REASONING_LABELS: Record<string, string> = {
  minimal: "最少",
  low: "低",
  medium: "中等",
  high: "高",
  xhigh: "极高",
  max: "最大",
  ultra: "超强",
};

function codexPreferenceDraft(preferences: CodexCliPreferences): CodexPreferenceDraft {
  return {
    model: preferences.model ?? "",
    reasoningEffort: preferences.reasoning_effort ?? "",
    verbosity: preferences.verbosity ?? "",
  };
}

function sameCodexPreference(
  left: CodexPreferenceDraft,
  right: CodexPreferenceDraft
) {
  return left.model === right.model
    && left.reasoningEffort === right.reasoningEffort
    && left.verbosity === right.verbosity;
}

function uniqueReasoningOptions(models: CodexModelOption[]) {
  const seen = new Set<string>();
  return models.flatMap((model) => model.supported_reasoning_efforts).filter((option) => {
    if (seen.has(option.effort)) return false;
    seen.add(option.effort);
    return true;
  });
}

function CodexRuntimePreferences() {
  const [preferences, setPreferences] = useState<CodexCliPreferences | null>(null);
  const [draft, setDraft] = useState<CodexPreferenceDraft>({
    model: "",
    reasoningEffort: "",
    verbosity: "",
  });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setErr(null);
    try {
      const next = await codexCliPreferences();
      setPreferences(next);
      setDraft(codexPreferenceDraft(next));
    } catch (e) {
      setErr(errText(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const selectedModel = preferences?.models.find((model) => model.slug === draft.model);
  const reasoningOptions = selectedModel
    ? selectedModel.supported_reasoning_efforts
    : uniqueReasoningOptions(preferences?.models ?? []);
  const reasoningValues = new Set(reasoningOptions.map((option) => option.effort));
  const displayedReasoningOptions = draft.reasoningEffort && !reasoningValues.has(draft.reasoningEffort)
    ? [{ effort: draft.reasoningEffort, description: "当前配置" }, ...reasoningOptions]
    : reasoningOptions;
  const savedDraft = preferences ? codexPreferenceDraft(preferences) : null;
  const dirty = savedDraft ? !sameCodexPreference(savedDraft, draft) : false;

  const changeModel = (model: string) => {
    const nextModel = preferences?.models.find((item) => item.slug === model);
    setDraft((current) => ({
      ...current,
      model,
      reasoningEffort:
        current.reasoningEffort
        && nextModel
        && !nextModel.supported_reasoning_efforts.some((option) => option.effort === current.reasoningEffort)
          ? ""
          : current.reasoningEffort,
    }));
    setNotice(null);
  };

  const save = async () => {
    if (!dirty || saving) return;
    setSaving(true);
    setErr(null);
    setNotice(null);
    try {
      const next = await codexSaveCliPreferences(
        draft.model,
        draft.reasoningEffort,
        draft.verbosity
      );
      setPreferences(next);
      setDraft(codexPreferenceDraft(next));
      setNotice("运行偏好已保存。");
    } catch (e) {
      setErr(errText(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="codex-runtime-preferences">
      <div className="codex-runtime-head">
        <div>
          <h4>运行偏好</h4>
          <p>保存到 Codex 的全局配置，也会用于其他 Codex CLI 会话。</p>
        </div>
        <button className="quiet-link" disabled={loading || saving} onClick={() => void load()}>
          重新读取
        </button>
      </div>

      {loading && <div className="settings-loading">正在读取 Codex 可用模型…</div>}
      {err && (
        <div className="errbar" role="alert">
          {err}
          <span className="spacer" />
          <button className="btn sm" disabled={loading || saving} onClick={() => void load()}>
            重试
          </button>
        </div>
      )}

      {!loading && preferences && (
        <>
          <div className="settings-control-list">
            <label className="settings-control-row" htmlFor="codex-model">
              <span>
                <strong>模型</strong>
                <small>{selectedModel ? "可用列表由当前 Codex 账户与 CLI 版本提供。" : "留空时由 Codex 选择默认模型。"}</small>
              </span>
              <select
                id="codex-model"
                className="input"
                value={draft.model}
                onChange={(event) => changeModel(event.target.value)}
              >
                <option value="">Codex 默认</option>
                {draft.model && !preferences.models.some((model) => model.slug === draft.model) && (
                  <option value={draft.model}>{draft.model}（当前配置）</option>
                )}
                {preferences.models.map((model) => (
                  <option key={model.slug} value={model.slug}>{model.display_name}</option>
                ))}
              </select>
            </label>

            <label className="settings-control-row" htmlFor="codex-reasoning">
              <span>
                <strong>思考强度</strong>
                <small>{selectedModel ? `该模型默认：${REASONING_LABELS[selectedModel.default_reasoning_effort] ?? selectedModel.default_reasoning_effort}` : "留空时跟随所用模型的默认值。"}</small>
              </span>
              <select
                id="codex-reasoning"
                className="input"
                value={draft.reasoningEffort}
                onChange={(event) => {
                  setDraft((current) => ({ ...current, reasoningEffort: event.target.value }));
                  setNotice(null);
                }}
              >
                <option value="">随模型默认</option>
                {displayedReasoningOptions.map((option) => (
                  <option key={option.effort} value={option.effort}>
                    {REASONING_LABELS[option.effort] ?? option.effort}
                  </option>
                ))}
              </select>
            </label>

            <label className="settings-control-row" htmlFor="codex-verbosity">
              <span>
                <strong>回复详略</strong>
                <small>控制 Codex 最终回复的展开程度，不改变代码质量要求。</small>
              </span>
              <select
                id="codex-verbosity"
                className="input"
                value={draft.verbosity}
                onChange={(event) => {
                  setDraft((current) => ({ ...current, verbosity: event.target.value }));
                  setNotice(null);
                }}
              >
                <option value="">Codex 默认</option>
                <option value="low">精简</option>
                <option value="medium">标准</option>
                <option value="high">详细</option>
              </select>
            </label>
          </div>

          <div className="codex-runtime-actions">
            {notice && <span role="status">{notice}</span>}
            <button className="btn accent" disabled={!dirty || saving} onClick={() => void save()}>
              {saving ? "正在保存…" : "应用"}
            </button>
          </div>
        </>
      )}
    </div>
  );
}

function CodexIntegrationSection() {
  const { runWithCodexCli } = useCodexCliGate();
  const [status, setStatus] = useState<CodexIntegrationStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [checking, setChecking] = useState(true);
  const [awaitingLogin, setAwaitingLogin] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const setupState: CodexSetupState | "loading" = status ? resolveCodexSetupState(status) : "loading";

  const refresh = useCallback(async (quiet = false) => {
    if (!quiet) setChecking(true);
    try {
      const next = await codexIntegrationStatus();
      setStatus(next);
      if (!quiet) setErr(null);
      return next;
    } catch (e) {
      if (!quiet) setErr(errText(e));
      return null;
    } finally {
      if (!quiet) setChecking(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (!awaitingLogin) return;
    let active = true;
    let attempts = 0;
    const check = async () => {
      attempts += 1;
      const next = await refresh(true);
      if (!active) return;
      if (next?.auth_status === "authenticated") {
        setAwaitingLogin(false);
        setNotice("已确认 Codex 登录，下一步可以完成协作配置。");
      } else if (attempts >= 60) {
        setAwaitingLogin(false);
        setNotice("暂时没有检测到登录完成；你可以稍后点击重新检测。");
      }
    };
    void check();
    const timer = window.setInterval(() => void check(), 2_000);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, [awaitingLogin, refresh]);

  const startLogin = async (mode: "browser" | "device") => {
    setBusy(true);
    setErr(null);
    setNotice(null);
    try {
      await runWithCodexCli({ feature: "Codex 登录" }, async () => {
        if (mode === "browser") await codexStartLogin();
        else await codexStartDeviceLogin();
        setAwaitingLogin(true);
        setNotice("等待 Codex 完成登录；R-Code 会自动检测，不需要手动刷新。");
      });
    } catch (e) {
      setErr(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const completeSetup = async () => {
    if (setupState === "check") {
      await refresh();
      return;
    }
    if (!status || setupState === "ready") return;
    setBusy(true);
    setErr(null);
    setNotice(null);
    try {
      await runWithCodexCli({ feature: "完成 Codex 设置", requireAuth: true }, async () => {
        const next = await codexSetupCollaboration();
        setStatus(next);
        setNotice("Codex 已就绪，可以作为 R-Code 的只读协作代理使用。");
      });
    } catch (e) {
      setErr(errText(e));
      void refresh(true);
    } finally {
      setBusy(false);
    }
  };

  const skillLabel =
    status?.skill_status === "up_to_date"
      ? "已安装"
      : status?.skill_status === "update_available"
        ? "可以更新"
        : "尚未安装";
  const loginLabel = status?.auth_status === "authenticated"
    ? `已登录${status.auth_method ? ` · ${status.auth_method}` : ""}`
    : status?.auth_status === "not_authenticated"
      ? "尚未登录"
      : "暂时无法确认";
  const skillReady = status?.skill_status === "up_to_date";
  const authReady = status?.auth_status === "authenticated";
  const collaborationReady = Boolean(skillReady && status?.mcp_server_configured);
  const copy = codexSetupCopy(status, setupState);
  const mainDisabled = busy
    || checking
    || awaitingLogin
    || !status
    || setupState === "ready"
    || (setupState === "install_cli" && status.installer_available === false);
  const loginDisabled = busy
    || checking
    || awaitingLogin
    || !status?.cli_available
    || status.auth_status !== "not_authenticated";
  const loginDisabledReason = authReady
    ? "当前已经登录，无需重复操作"
    : status?.auth_status === "unknown"
      ? "请先重新检测登录状态"
      : !status?.cli_available
        ? "请先安装 Codex CLI"
        : undefined;

  return (
    <section className="settings-block codex-setup">
      <div className="codex-setup-heading">
        <div>
          <h3>Codex 协作</h3>
          <p className="desc">连接本机 Codex CLI，启用只读代理协作。登录凭据始终由 Codex 管理。</p>
        </div>
        <button
          className={`codex-status-refresh${checking ? " checking" : ""}`}
          disabled={busy || checking || awaitingLogin}
          onClick={() => void refresh()}
          aria-label="重新检测 Codex 状态"
          title="重新检测 Codex 状态"
        >
          <IconRefresh width={16} height={16} />
        </button>
      </div>
      {err && <div className="errbar" role="alert">{err}</div>}
      <div className={`codex-setup-status state-${setupState}`} role="status" aria-live="polite">
        <div className="codex-setup-status-copy">
          <span className="codex-status-dot" aria-hidden="true" />
          <div>
            <strong>{copy.title}</strong>
            <p>{copy.detail}</p>
          </div>
        </div>
        <button
          className={`btn codex-primary-action${setupState === "ready" ? "" : " accent"}`}
          disabled={mainDisabled}
          onClick={() => void completeSetup()}
        >
          {busy ? "正在处理…" : awaitingLogin ? "等待登录…" : copy.action}
        </button>
      </div>

      <ol className="codex-setup-steps" aria-label="Codex 设置进度">
        <li className={status?.cli_available ? "done" : setupState === "install_cli" ? "current" : "pending"}>
          <span className="codex-step-mark">{status?.cli_available && <IconCheck width={12} height={12} />}</span>
          <div><strong>Codex CLI</strong><small>{status?.cli_available ? "可运行" : "待安装"}</small></div>
        </li>
        <li className={authReady ? "done" : setupState === "login" || setupState === "check" ? "current" : "pending"}>
          <span className="codex-step-mark">{authReady && <IconCheck width={12} height={12} />}</span>
          <div><strong>登录</strong><small>{loginLabel}</small></div>
        </li>
        <li className={collaborationReady ? "done" : setupState === "configure" ? "current" : "pending"}>
          <span className="codex-step-mark">{collaborationReady && <IconCheck width={12} height={12} />}</span>
          <div><strong>R-Code 协作</strong><small>{collaborationReady ? "Skill 与 MCP 已连接" : "待配置"}</small></div>
        </li>
      </ol>

      {notice && <p className="codex-inline-note" role="status"><IconCheck width={14} height={14} />{notice}</p>}
      {status?.cli_error && setupState === "install_cli" && <p className="codex-inline-warning">{status.cli_error}</p>}

      {setupState === "ready" && <CodexRuntimePreferences />}

      {status && (
        <details className="codex-advanced">
          <summary>高级选项 <span>登录方式与配置详情</span></summary>
          <div className="codex-advanced-body">
            <div className="codex-login-options">
              <div>
                <strong>登录方式</strong>
                <small>{authReady ? "当前已登录，按钮已停用。" : "浏览器登录优先；设备码用于远程或回调受阻环境。"}</small>
              </div>
              <div>
                <button className="btn sm" disabled={loginDisabled} title={loginDisabledReason} onClick={() => void startLogin("browser")}>
                  浏览器登录
                </button>
                <button className="btn sm ghost" disabled={loginDisabled} title={loginDisabledReason} onClick={() => void startLogin("device")}>
                  设备码（备用）
                </button>
              </div>
            </div>
            <dl className="codex-details-list">
              <dt>CLI</dt>
              <dd>{status.cli_available ? `${status.cli_version || "可运行"}` : "不可用"}</dd>
              <dt>登录</dt>
              <dd>{loginLabel}</dd>
              <dt>协作 Skill</dt>
              <dd>{skillLabel}</dd>
              <dt>只读 MCP</dt>
              <dd>{status.mcp_server_configured ? "已启用" : "尚未启用"}</dd>
              <dt>配置位置</dt>
              <dd className="val">{status.config_path}</dd>
            </dl>
            {!status.cli_available && (
              <p className="codex-manual-install">手动安装：<code>{status.installer_command || "npm install -g @openai/codex"}</code></p>
            )}
          </div>
        </details>
      )}
    </section>
  );
}
