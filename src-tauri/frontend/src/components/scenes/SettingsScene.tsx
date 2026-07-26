import { useCallback, useEffect, useRef, useState } from "react";
import { useAppStore } from "../../store/app";
import { usePoll } from "../../lib/poll";
import {
  codexInstallSkill,
  codexIntegrationStatus,
  codexStartLogin,
  logsTail,
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
  CodexIntegrationStatus,
  LogEntry,
  ProviderConfig,
  ProviderStatus,
  SupportBundlePreview,
} from "../../lib/types";
import { clockTime } from "../../lib/format";

const PROVIDER_PRESETS = [
  {
    name: "anthropic",
    label: "Anthropic",
    baseUrl: "https://api.anthropic.com",
    model: "claude-sonnet-4",
  },
  {
    name: "openai",
    label: "OpenAI",
    baseUrl: "https://api.openai.com/v1",
    model: "gpt-5",
  },
  {
    name: "deepseek",
    label: "DeepSeek",
    baseUrl: "https://api.deepseek.com",
    model: "deepseek-v4-flash",
  },
  {
    name: "openrouter",
    label: "OpenRouter",
    baseUrl: "https://openrouter.ai/api/v1",
    model: "openai/gpt-4.1-mini",
  },
] as const;
const LOG_LEVELS = ["debug", "info", "warn", "error"];
const LOG_FILTERS = ["all", "error", "warn", "info", "debug"] as const;
const EMPTY_PROVIDERS: NonNullable<AppConfig["providers"]> = {};
const OUTPUT_DEFAULT = "8192";

function providerPreset(name: string) {
  return PROVIDER_PRESETS.find((preset) => preset.name === name);
}

function providerLabel(name: string) {
  return providerPreset(name)?.label ?? name;
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
      setConfigErr(String(e));
    }
  }, []);

  useEffect(() => {
    void loadConfig();
  }, [loadConfig]);

  return (
    <div className="scene">
      <div className="scene-scroll">
        <div className="page-head">
          <h1>设置</h1>
          <span className="meta">连接、偏好与诊断</span>
        </div>

        <div className="set-grid">
          {configErr && (
            <div className="errbar">
              读取配置失败：{configErr}
              <span className="spacer" />
              <button className="btn" onClick={() => void loadConfig()}>
                重试
              </button>
            </div>
          )}
          {validation && !configErr && (
            <div className="notebar">
              还不能开始对话。选择模型服务并保存访问密钥后即可使用。
              <span className="dim">{validation}</span>
            </div>
          )}
          {config && (
            <>
              <ProviderSection config={config} providerStatus={providerStatus} reload={loadConfig} />
              <GeneralSection config={config} reload={loadConfig} />
            </>
          )}

          <AppearanceSection />
          <AccessibilitySection />
          <LogSection />
          <SupportSection />
          <CodexIntegrationSection />
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
  const [selectedProvider, setSelectedProvider] = useState<string | null>(null);
  const [presetName, setPresetName] = useState("deepseek");
  const [profileName, setProfileName] = useState("");
  const [fields, setFields] = useState({ base_url: "", model: "", max_tokens: OUTPUT_DEFAULT, temperature: "0.2" });
  const [keyInput, setKeyInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);

  useEffect(() => {
    setSelectedProvider((current) => {
      if (current && providers[current]) return current;
      if (configDefault && providers[configDefault]) return configDefault;
      return names[0] ?? null;
    });
  }, [configDefault, names.join("|")]);

  const applyPreset = useCallback((nextPreset: string) => {
    const preset = providerPreset(nextPreset);
    setPresetName(nextPreset);
    setFields({
      base_url: preset?.baseUrl ?? "",
      model: preset?.model ?? "",
      max_tokens: OUTPUT_DEFAULT,
      temperature: "0.2",
    });
  }, []);

  useEffect(() => {
    if (!selectedProvider) {
      setProfileName("");
      setKeyInput("");
      setSaved(null);
      setErr(null);
      applyPreset(presetName);
      return;
    }
    const profile = providers[selectedProvider] as ProviderConfig | undefined;
    const preset = providerPreset(selectedProvider);
    setProfileName(selectedProvider);
    setPresetName(preset?.name ?? "custom");
    setFields({
      base_url: profile?.base_url ?? preset?.baseUrl ?? "",
      model: profile?.model ?? preset?.model ?? "",
      max_tokens: profile?.max_tokens != null ? String(profile.max_tokens) : OUTPUT_DEFAULT,
      temperature: displayNumber(profile?.temperature) || "0.2",
    });
    setKeyInput("");
    setSaved(null);
    setErr(null);
  }, [applyPreset, providers, selectedProvider]);

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
      setErr(String(e));
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

  return (
    <section className="pane setcard provider-settings">
      <div className="section-heading">
        <div>
          <h3>R-Code Agent 模型服务</h3>
          <p className="desc">管理 R-Code 自己发起对话所用的服务。保存后仍可随时修改、切换或删除；密钥只保存在系统凭据库。</p>
        </div>
        <button
          className="btn"
          disabled={busy}
          onClick={() => {
            setSelectedProvider(null);
            setPresetName("deepseek");
          }}
        >
          新建服务
        </button>
      </div>

      {err && <div className="errbar">{err}</div>}
      {saved && <div className="okbar">{saved}</div>}

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
            <label>预设</label>
            <select
              className="input"
              value={presetName}
              disabled={busy || Boolean(editing)}
              onChange={(event) => applyPreset(event.target.value)}
            >
              {PROVIDER_PRESETS.map((preset) => (
                <option key={preset.name} value={preset.name}>{preset.label}</option>
              ))}
              <option value="custom">OpenAI 兼容接口</option>
            </select>
            {editing && <span className="hint">已有服务保留其当前设置；如需新预设，请新建一项。</span>}
          </div>
          <div className="field">
            <label>配置名称</label>
            <input
              className="input"
              value={profileName}
              readOnly={Boolean(editing)}
              placeholder="例如：DeepSeek 工作账户"
              onChange={(event) => setProfileName(event.target.value)}
            />
            {editing && <span className="hint">名称用于区分配置；需要改名时新建后删除旧项。</span>}
          </div>
          <div className="field">
            <label>接口地址</label>
            <input
              className="input"
              value={fields.base_url}
              placeholder="https://api.example.com/v1"
              onChange={(event) => setFields((value) => ({ ...value, base_url: event.target.value }))}
            />
            <span className="hint">填写服务根地址，不要填写完整的 /chat/completions 路径。</span>
          </div>
          <div className="field">
            <label>模型</label>
            <input
              className="input"
              value={fields.model}
              placeholder="模型名称"
              onChange={(event) => setFields((value) => ({ ...value, model: event.target.value }))}
            />
          </div>
          <div className="field">
            <label>访问密钥</label>
            <span className="val">{credentialLabel}</span>
            <input
              className="input"
              type="password"
              placeholder={credential?.configured ? "留空则保留当前密钥" : "粘贴访问密钥"}
              value={keyInput}
              onChange={(event) => setKeyInput(event.target.value)}
            />
          </div>
          <div className="field token-field">
            <label>最大输出</label>
            <input
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
            <label>随机性</label>
            <input
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
            <button className="btn" disabled={busy || outputExceedsDeepSeekLimit} onClick={() => saveProvider(false)}>保存</button>
            <button className="btn accent" disabled={busy || outputExceedsDeepSeekLimit} onClick={() => saveProvider(true)}>保存并用于新对话</button>
          </div>
        </div>
      </div>
    </section>
  );
}

// ---------- 通用 ----------

function GeneralSection({ config, reload }: { config: AppConfig; reload: () => Promise<void> }) {
  const [err, setErr] = useState<string | null>(null);

  const setLevel = async (v: string) => {
    setErr(null);
    try {
      await settingsSet("log_level", v);
      await reload();
    } catch (e) {
      setErr(String(e));
    }
  };

  return (
    <section className="pane setcard">
      <h3>通用</h3>
      {err && <div className="errbar">{err}</div>}
      <div className="field">
        <label>日志级别</label>
        <select
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
    { key: "dark", label: "暗色", hint: "默认深色界面" },
    { key: "system", label: "跟随系统", hint: "随操作系统明暗切换" },
  ];

  return (
    <section className="pane setcard">
      <h3>外观</h3>
      <div className="field">
        <label>主题</label>
        <div className="chips">
          {modes.map((m) => (
            <button
              key={m.key}
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
        <label>界面缩放</label>
        <input
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
    <section className="pane setcard">
      <h3>无障碍</h3>
      <div className="field">
        <label>文本差异视图</label>
        <input
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
      setErr(String(e));
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
    <section className="pane setcard">
      <h3>日志</h3>
      <div className="field">
        <div className="chips">
          {LOG_FILTERS.map((l) => (
            <button
              key={l}
              className={`chipbtn${filter === l ? " on" : ""}`}
              onClick={() => setFilter(l)}
            >
              {l === "all" ? "全部" : l}
            </button>
          ))}
        </div>
      </div>
      {err && <div className="errbar">{err}</div>}
      <div className="logbox" ref={boxRef}>
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
      setErr(`预览失败：${String(e)}`);
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
      setErr(`导出失败：${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="pane setcard">
      <h3>支持包</h3>
      <p className="desc">导出版本、平台、近期日志和本地统计，便于提交问题；预览不会写入文件。</p>
      {err && <div className="errbar">{err}</div>}
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
        <label>输出目录</label>
        <input className="input" value={outDir} onChange={(e) => setOutDir(e.target.value)} />
        <button className="btn accent" disabled={busy || !outDir.trim()} onClick={() => void doExport()}>
          导出
        </button>
      </div>
      {bundlePath && (
        <div className="okbar">
          已生成：<span className="val">{bundlePath}</span>
        </div>
      )}
    </section>
  );
}

// ---------- 外部 Agent ----------

function CodexIntegrationSection() {
  const [status, setStatus] = useState<CodexIntegrationStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setStatus(await codexIntegrationStatus());
      setErr(null);
    } catch (e) {
      setErr(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const install = async () => {
    setBusy(true);
    setErr(null);
    setNotice(null);
    try {
      await codexInstallSkill();
      await refresh();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const startLogin = async () => {
    setBusy(true);
    setErr(null);
    setNotice(null);
    try {
      await codexStartLogin();
      setNotice("已在系统终端打开 Codex 登录。完成浏览器授权后，点击“刷新状态”。");
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const skillLabel =
    status?.skill_status === "up_to_date"
      ? "已连接"
      : status?.skill_status === "update_available"
        ? "可以更新"
        : "尚未连接";

  return (
    <section className="pane setcard">
      <h3>Codex CLI</h3>
      <p className="desc">
        这是独立于 R-Code Agent 模型服务的 Codex 登录与协作入口。R-Code 的 Provider 列表不会修改 Codex 的登录态、模型或第三方路由。
      </p>
      {err && <div className="errbar">{err}</div>}
      {notice && <div className="okbar">{notice}</div>}
      {status && (
        <dl className="kv">
          <dt>Codex CLI</dt>
          <dd>{status.cli_available ? "已检测到" : "未检测到"}</dd>
          <dt>登录状态</dt>
          <dd>{status.authenticated ? "已发现本地登录凭据" : "尚未登录"}</dd>
          <dt>Codex 配置</dt>
          <dd>{status.config_exists ? "已发现配置文件" : "尚未创建配置文件"}</dd>
          <dt>R-Code 协作 Skill</dt>
          <dd>{skillLabel}</dd>
          <dt>配置位置</dt>
          <dd className="val">{status.config_path}</dd>
        </dl>
      )}
      <div className="footbar">
        <button className="btn accent" disabled={busy || !status?.cli_available} onClick={() => void startLogin()}>
          在终端中登录
        </button>
        <button className="btn" disabled={busy} onClick={() => void install()}>
          {status?.skill_status === "up_to_date" ? "更新协作 Skill" : "安装协作 Skill"}
        </button>
        <button className="btn ghost" disabled={busy} onClick={() => void refresh()}>
          刷新状态
        </button>
      </div>
    </section>
  );
}
