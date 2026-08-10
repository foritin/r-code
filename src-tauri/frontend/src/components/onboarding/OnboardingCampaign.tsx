import { createPortal } from "react-dom";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useAppStore } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import {
  codexIntegrationStatus,
  providerCatalog,
  settingsGet,
  settingsSaveProvider,
  settingsSelectProvider,
  settingsSet,
  workspaceChoose,
  workspaceSetAccessMode,
} from "../../lib/ipc";
import { errText } from "../../lib/format";
import {
  announceRuntimeSettingsChanged,
  ONBOARDING_OPEN_EVENT,
  saveOnboardingReceipt,
  shouldOpenOnboarding,
} from "../../lib/onboarding";
import type {
  CodexIntegrationStatus,
  ProjectAccessMode,
  ProviderPreset,
  SettingsResponse,
  TaskAgentEngine,
  Workspace,
} from "../../lib/types";
import brandIcon from "../../../../../icons/512x512.png";

const STEPS = ["欢迎", "主 Agent", "Provider", "工作区", "开始"] as const;
const FEATURED_PROVIDER_IDS = ["deepseek", "openai", "anthropic"];

const ACCESS_OPTIONS: ReadonlyArray<{
  value: ProjectAccessMode;
  label: string;
  detail: string;
}> = [
  { value: "request_approval", label: "请求批准", detail: "命令、写入与外部访问均询问" },
  { value: "risk_based", label: "替我审批", detail: "仅在中高风险操作时询问" },
  { value: "full_access", label: "完全访问", detail: "自动批准工作区内操作" },
];

type ScopeMode = "chat" | "workspace";
type BootstrapResource = "settings" | "catalog" | "codex";
type BootstrapErrors = Partial<Record<BootstrapResource, string>>;

function providerCode(preset: ProviderPreset | undefined): string {
  if (!preset) return "--";
  const known: Record<string, string> = { deepseek: "DS", openai: "OA", anthropic: "AN" };
  if (known[preset.id]) return known[preset.id];
  return preset.label
    .split(/\s+/)
    .map((part) => part[0])
    .join("")
    .slice(0, 2)
    .toUpperCase();
}

function focusableElements(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>(
    "button:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex='-1'])",
  )).filter((element) => !element.hasAttribute("hidden") && !element.closest("[inert]"));
}

export function OnboardingCampaign() {
  const [open, setOpen] = useState(shouldOpenOnboarding);
  const [step, setStep] = useState(0);
  const [confirmClose, setConfirmClose] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [bootstrapErrors, setBootstrapErrors] = useState<BootstrapErrors>({});
  const [engineNotice, setEngineNotice] = useState<string | null>(null);
  const [providerNotice, setProviderNotice] = useState<string | null>(null);
  const [workspaceNotice, setWorkspaceNotice] = useState<string | null>(null);
  const [settings, setSettings] = useState<SettingsResponse | null>(null);
  const [presets, setPresets] = useState<ProviderPreset[]>([]);
  const [codexStatus, setCodexStatus] = useState<CodexIntegrationStatus | null>(null);
  const [engine, setEngine] = useState<TaskAgentEngine>("r_code");
  const [selectedProviderId, setSelectedProviderId] = useState("deepseek");
  const [apiKey, setApiKey] = useState("");
  const [scope, setScope] = useState<ScopeMode>("chat");
  const [selectedWorkspace, setSelectedWorkspace] = useState<Workspace | null>(null);
  const [accessMode, setAccessMode] = useState<ProjectAccessMode>("risk_based");
  const [dragOffset, setDragOffset] = useState(0);
  const dialogRef = useRef<HTMLElement>(null);
  const confirmRef = useRef<HTMLElement>(null);
  const viewportRef = useRef<HTMLElement>(null);
  const dragRef = useRef<{ id: number; x: number; at: number } | null>(null);
  const dragOffsetRef = useRef(0);
  const loadRequestRef = useRef(0);
  const settingsRef = useRef<SettingsResponse | null>(null);
  const codexStatusRef = useRef<CodexIntegrationStatus | null>(null);
  const selectionTouchedRef = useRef({ engine: false, provider: false, scope: false });

  const setScene = useAppStore((state) => state.setScene);
  const setCurrentWorkspace = useTasksStore((state) => state.setCurrentProject);
  const refreshWorkspaces = useTasksStore((state) => state.refreshWorkspaces);

  const selectedPreset = useMemo(
    () => presets.find((preset) => preset.id === selectedProviderId),
    [presets, selectedProviderId],
  );
  const providerConfig = settings?.config.providers?.[selectedProviderId];
  const providerReady = Boolean(settings?.provider_status[selectedProviderId]?.ready);
  const providerActive = settings?.config.default_provider === selectedProviderId;
  const codexReady = Boolean(codexStatus?.integration_ready);
  const selectedAccess = ACCESS_OPTIONS.find((option) => option.value === accessMode) ?? ACCESS_OPTIONS[1];

  const applyEngineDefaults = useCallback(() => {
    if (selectionTouchedRef.current.engine || selectionTouchedRef.current.scope) return;
    const nextSettings = settingsRef.current;
    const nextCodex = codexStatusRef.current;
    if (!nextSettings || !nextCodex) return;

    const defaultEngine = nextSettings.config.orchestration?.default_agent_engine ?? "r_code";
    const nextEngine = defaultEngine === "codex" && !nextCodex.integration_ready ? "r_code" : defaultEngine;
    setEngine(nextEngine);
    setScope(nextEngine === "codex" ? "workspace" : "chat");
  }, []);

  const load = useCallback((resetChoices = true) => {
    const requestId = ++loadRequestRef.current;
    const isCurrent = () => loadRequestRef.current === requestId;
    const reportFailure = (resource: BootstrapResource, cause: unknown) => {
      if (!isCurrent()) return;
      setBootstrapErrors((current) => ({ ...current, [resource]: errText(cause) }));
    };

    setBootstrapErrors({});
    if (resetChoices) {
      selectionTouchedRef.current = { engine: false, provider: false, scope: false };
      settingsRef.current = null;
      codexStatusRef.current = null;
      setSettings(null);
      setPresets([]);
      setCodexStatus(null);
      setEngine("r_code");
      setSelectedProviderId("deepseek");
      setScope("chat");
      setSelectedWorkspace(null);
      setAccessMode("risk_based");
      setError(null);
      setApiKey("");
      setEngineNotice(null);
      setProviderNotice(null);
      setWorkspaceNotice(null);
    }

    // Each resource hydrates its own controls as soon as it is ready. In particular,
    // a slow Codex CLI probe must never hold the welcome slide or Provider catalog.
    void settingsGet().then((nextSettings) => {
      if (!isCurrent()) return;
      settingsRef.current = nextSettings;
      setSettings(nextSettings);
      const defaultProvider = nextSettings.config.default_provider;
      if (
        !selectionTouchedRef.current.provider
        && defaultProvider
        && FEATURED_PROVIDER_IDS.includes(defaultProvider)
      ) {
        setSelectedProviderId(defaultProvider);
      }
      applyEngineDefaults();
    }, (cause) => reportFailure("settings", cause));

    void providerCatalog().then((catalog) => {
      if (!isCurrent()) return;
      const featured = FEATURED_PROVIDER_IDS
        .map((id) => catalog.presets.find((preset) => preset.id === id))
        .filter((preset): preset is ProviderPreset => Boolean(preset));
      setPresets(featured);
      if (!selectionTouchedRef.current.provider) {
        setSelectedProviderId((current) => (
          featured.some((preset) => preset.id === current) ? current : featured[0]?.id ?? ""
        ));
      }
    }, (cause) => reportFailure("catalog", cause));

    void codexIntegrationStatus().then((nextCodex) => {
      if (!isCurrent()) return;
      codexStatusRef.current = nextCodex;
      setCodexStatus(nextCodex);
      applyEngineDefaults();
    }, (cause) => reportFailure("codex", cause));
  }, [applyEngineDefaults]);

  useEffect(() => {
    const reopen = () => {
      setStep(0);
      setConfirmClose(false);
      setError(null);
      setApiKey("");
      setOpen(true);
    };
    window.addEventListener(ONBOARDING_OPEN_EVENT, reopen);
    return () => window.removeEventListener(ONBOARDING_OPEN_EVENT, reopen);
  }, []);

  useEffect(() => {
    if (!open) return;
    load();
    return () => {
      loadRequestRef.current += 1;
    };
  }, [load, open]);

  useEffect(() => {
    if (!open) return;
    const previousOverflow = document.body.style.overflow;
    const appRoot = document.getElementById("app");
    const previousAriaHidden = appRoot?.getAttribute("aria-hidden");
    document.body.style.overflow = "hidden";
    if (appRoot) {
      appRoot.inert = true;
      appRoot.setAttribute("aria-hidden", "true");
    }
    const timer = window.setTimeout(() => viewportRef.current?.focus({ preventScroll: true }), 0);
    return () => {
      window.clearTimeout(timer);
      document.body.style.overflow = previousOverflow;
      if (appRoot) {
        appRoot.inert = false;
        if (previousAriaHidden == null) appRoot.removeAttribute("aria-hidden");
        else appRoot.setAttribute("aria-hidden", previousAriaHidden);
      }
    };
  }, [open]);

  useEffect(() => {
    if (!confirmClose) return;
    const timer = window.setTimeout(() => confirmRef.current?.querySelector<HTMLElement>("button")?.focus(), 0);
    return () => window.clearTimeout(timer);
  }, [confirmClose]);

  useEffect(() => {
    if (!open) return;
    const keydown = (event: KeyboardEvent) => {
      const trapTab = (root: HTMLElement | null) => {
        if (event.key !== "Tab" || !root) return;
        const focusable = focusableElements(root);
        if (!focusable.length) return;
        const first = focusable[0];
        const last = focusable[focusable.length - 1];
        if (event.shiftKey && document.activeElement === first) {
          event.preventDefault();
          last.focus();
        } else if (!event.shiftKey && document.activeElement === last) {
          event.preventDefault();
          first.focus();
        }
      };
      if (event.key === "Escape") {
        event.preventDefault();
        setConfirmClose((current) => !current);
        return;
      }
      if (confirmClose) {
        trapTab(confirmRef.current);
        return;
      }
      const editing = Boolean((event.target as HTMLElement | null)?.closest("input, textarea, select, [contenteditable='true']"));
      if (editing && (event.key === "ArrowRight" || event.key === "ArrowLeft")) return;
      if (event.key === "ArrowRight") setStep((current) => Math.min(STEPS.length - 1, current + 1));
      if (event.key === "ArrowLeft") setStep((current) => Math.max(0, current - 1));
      trapTab(dialogRef.current);
    };
    window.addEventListener("keydown", keydown);
    return () => window.removeEventListener("keydown", keydown);
  }, [confirmClose, open]);

  const chooseEngine = (next: TaskAgentEngine) => {
    if (next === "codex" && !codexStatus) {
      setEngineNotice(null);
      return;
    }
    if (next === "codex" && !codexReady) {
      setEngineNotice("Codex CLI 尚未完成安装、登录或协作配置。");
      return;
    }
    selectionTouchedRef.current.engine = true;
    setEngine(next);
    setEngineNotice(next === "codex" ? "Codex CLI 需要附加工作区。" : "R-Code 支持纯聊天，也可附加工作区。");
    if (next === "codex") setScope("workspace");
  };

  const selectProvider = (id: string) => {
    selectionTouchedRef.current.provider = true;
    setSelectedProviderId(id);
    setApiKey("");
    setError(null);
    setProviderNotice(null);
  };

  const saveProvider = async () => {
    if (!selectedPreset || busy) return;
    if (!providerReady && !apiKey.trim()) {
      setError("请先填写访问密钥。");
      return;
    }
    setBusy(true);
    setError(null);
    setProviderNotice(null);
    try {
      await settingsSaveProvider({
        name: selectedPreset.id,
        providerKind: selectedPreset.id,
        baseUrl: providerConfig?.base_url ?? selectedPreset.base_url,
        model: providerConfig?.model ?? selectedPreset.model,
        apiKey: apiKey.trim() || null,
        protocol:
          providerConfig?.protocol
          ?? settings?.provider_status[selectedProviderId]?.effective_protocol
          ?? selectedPreset.protocol,
        activate: true,
      });
      const nextSettings = await settingsGet();
      setSettings(nextSettings);
      setApiKey("");
      setProviderNotice(`${selectedPreset.label} 已用于新对话。`);
      announceRuntimeSettingsChanged();
    } catch (cause) {
      setError(`保存 Provider 失败：${errText(cause)}`);
    } finally {
      setBusy(false);
    }
  };

  const chooseWorkspace = async () => {
    if (busy) return;
    selectionTouchedRef.current.scope = true;
    setBusy(true);
    setError(null);
    setWorkspaceNotice(null);
    try {
      const workspace = await workspaceChoose();
      if (!workspace) return;
      // Opening a known project must not silently broaden or narrow its saved
      // permission policy. Adopt it first; only an explicit option click changes it.
      setSelectedWorkspace(workspace);
      setAccessMode(workspace.access_mode);
      setScope("workspace");
      await refreshWorkspaces();
      setWorkspaceNotice(`${workspace.display_name} 已设为本次新会话的边界。`);
    } catch (cause) {
      setError(`选择工作区失败：${errText(cause)}`);
    } finally {
      setBusy(false);
    }
  };

  const changeAccessMode = async (next: ProjectAccessMode) => {
    setAccessMode(next);
    setError(null);
    if (!selectedWorkspace) return;
    setBusy(true);
    try {
      const updated = await workspaceSetAccessMode(selectedWorkspace.canonical_path, next);
      setSelectedWorkspace(updated);
      await refreshWorkspaces();
    } catch (cause) {
      setError(`保存工作区权限失败：${errText(cause)}`);
    } finally {
      setBusy(false);
    }
  };

  const finish = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    const failures: string[] = [];
    const persist = async (label: string, action: () => Promise<void>) => {
      try {
        await action();
      } catch (cause) {
        failures.push(`${label}：${errText(cause)}`);
      }
    };

    // This is an introduction, not a blocking setup wizard. Persist only choices
    // that are currently valid, then always let the user enter the product.
    if (engine === "r_code" && providerReady && !providerActive) {
      await persist("默认 Provider", () => settingsSelectProvider(selectedProviderId));
    }
    const engineReady = engine === "r_code"
      || Boolean(codexReady && scope === "workspace" && selectedWorkspace);
    if (engineReady && ((settings && codexStatus) || selectionTouchedRef.current.engine)) {
      await persist("默认主 Agent", () => settingsSet("orchestration.default_agent_engine", engine));
    }
    if (scope === "workspace" && selectedWorkspace) {
      await persist("工作区权限", async () => {
        if (selectedWorkspace.access_mode !== accessMode) {
          await workspaceSetAccessMode(selectedWorkspace.canonical_path, accessMode);
          await refreshWorkspaces();
        }
      });
      setCurrentWorkspace(selectedWorkspace.canonical_path);
    } else {
      setCurrentWorkspace(null);
    }
    if (failures.length) console.warn("部分首次设置未保存：", failures.join("；"));

    saveOnboardingReceipt("completed");
    announceRuntimeSettingsChanged();
    setScene("home");
    setApiKey("");
    setBusy(false);
    setOpen(false);
    window.setTimeout(() => window.dispatchEvent(new Event("r-code:new-session-ready")), 0);
  };

  const dismiss = () => {
    saveOnboardingReceipt("dismissed");
    setConfirmClose(false);
    setApiKey("");
    setOpen(false);
  };

  const chooseScope = (next: ScopeMode) => {
    selectionTouchedRef.current.scope = true;
    setScope(next);
    setError(null);
  };

  const pointerDown = (event: React.PointerEvent<HTMLElement>) => {
    if (event.button !== 0 || (event.target as HTMLElement).closest("button,input,label,a,select")) return;
    dragRef.current = { id: event.pointerId, x: event.clientX, at: performance.now() };
    dragOffsetRef.current = 0;
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const pointerMove = (event: React.PointerEvent<HTMLElement>) => {
    if (dragRef.current?.id !== event.pointerId) return;
    let delta = event.clientX - dragRef.current.x;
    if ((step === 0 && delta > 0) || (step === STEPS.length - 1 && delta < 0)) delta *= 0.25;
    dragOffsetRef.current = delta;
    setDragOffset(delta);
  };

  const pointerEnd = (event: React.PointerEvent<HTMLElement>) => {
    const drag = dragRef.current;
    if (!drag || drag.id !== event.pointerId) return;
    const elapsed = Math.max(1, performance.now() - drag.at);
    const delta = dragOffsetRef.current;
    const speed = delta / elapsed;
    const threshold = Math.min(120, (viewportRef.current?.clientWidth ?? 1000) * 0.12);
    if ((delta < -threshold || speed < -0.55) && step < STEPS.length - 1) setStep((current) => current + 1);
    if ((delta > threshold || speed > 0.55) && step > 0) setStep((current) => current - 1);
    dragRef.current = null;
    dragOffsetRef.current = 0;
    setDragOffset(0);
    try { event.currentTarget.releasePointerCapture(event.pointerId); } catch { /* already released */ }
  };

  if (!open) return null;

  const providerLabel = selectedPreset?.label ?? "尚未选择";
  const workspaceLabel = scope === "workspace" ? selectedWorkspace?.display_name ?? "待选择" : "纯聊天";
  const summaryLine = `${engine === "codex" ? "Codex CLI" : "R-Code"} × ${engine === "r_code" ? providerLabel : "本机登录"} × ${workspaceLabel}`;
  const providerBaseUrl = providerConfig?.base_url ?? selectedPreset?.base_url ?? "—";
  const providerModel = providerConfig?.model ?? selectedPreset?.model ?? "—";
  const providerBootstrapError = bootstrapErrors.catalog ?? bootstrapErrors.settings;

  return createPortal(
    <div className="onboarding-layer">
      <div className="onboarding-veil" aria-hidden="true" />
      <section
        className="onboarding-tour"
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label="R-Code 首次设置"
        aria-busy={busy}
      >
        <header className="onboarding-header">
          <div className="onboarding-brand"><img src={brandIcon} alt="" /><strong>R-Code</strong></div>
          <span>{STEPS[step]}</span>
          <b><i>{String(step + 1).padStart(2, "0")}</i> / 05</b>
          <button type="button" onClick={() => setConfirmClose(true)} aria-label="关闭首次设置">×</button>
        </header>

        <section
          className={`onboarding-viewport${dragRef.current ? " dragging" : ""}`}
          ref={viewportRef}
          tabIndex={0}
          aria-roledescription="轮播"
          aria-label="首次设置步骤"
          onPointerDown={pointerDown}
          onPointerMove={pointerMove}
          onPointerUp={pointerEnd}
          onPointerCancel={pointerEnd}
        >
          <div
            className={`onboarding-track${dragRef.current ? " dragging" : ""}`}
            style={{ transform: `translate3d(calc(${-step * 100}% + ${dragOffset}px), 0, 0)` }}
          >
            <article className="onboarding-slide onboarding-hero" aria-hidden={step !== 0} inert={step !== 0 ? "" : undefined}>
              <div className="onboarding-hero-copy">
                <span>R-CODE / FIRST RUN</span>
                <h1>把目标<br />交给 R-Code。</h1>
                <p>从结果开始。</p>
              </div>
              <div className="onboarding-hero-brand">
                <img src={brandIcon} alt="R-Code" />
                <strong>R</strong>
                <small>SESSION-FIRST<br />CODING</small>
              </div>
            </article>

            <article className="onboarding-slide onboarding-engine" aria-hidden={step !== 1} inert={step !== 1 ? "" : undefined}>
              <div className="onboarding-ad-title">
                <span>01 / MAIN AGENT</span>
                <h1>选执行者。</h1>
                <p>运行中不可切换。</p>
              </div>
              <div className="onboarding-engine-pick" role="radiogroup" aria-label="主 Agent">
                <button
                  className={`onboarding-engine-option${engine === "r_code" ? " selected" : ""}`}
                  type="button"
                  role="radio"
                  aria-checked={engine === "r_code"}
                  onClick={() => chooseEngine("r_code")}
                >
                  <b>R</b><strong>R-Code</strong><span>自定义 Provider</span><i>{engine === "r_code" ? "已选" : "可用"}</i>
                </button>
                <button
                  className={`onboarding-engine-option codex${engine === "codex" ? " selected" : ""}`}
                  type="button"
                  role="radio"
                  aria-checked={engine === "codex"}
                  aria-disabled={!codexReady}
                  onClick={() => chooseEngine("codex")}
                >
                  <b>C</b><strong>Codex CLI</strong><span>本机登录 · 需工作区</span><i>{codexStatus ? (codexReady ? (engine === "codex" ? "已选" : "可用") : "未连接") : bootstrapErrors.codex ? "不可用" : "检测中"}</i>
                </button>
                <small role="status" title={bootstrapErrors.codex}>
                  <span>{engineNotice ?? (bootstrapErrors.codex ? "Codex 状态暂不可用；R-Code 不受影响。" : codexReady ? "两种主 Agent 均已就绪。" : "R-Code 可直接聊天。")}</span>
                  {bootstrapErrors.codex && <button className="onboarding-retry" type="button" onClick={() => load(false)}>重试</button>}
                </small>
              </div>
            </article>

            <article className="onboarding-slide onboarding-provider" aria-hidden={step !== 2} inert={step !== 2 ? "" : undefined}>
              <div className="onboarding-ad-title onboarding-light-title">
                <span>02 / PROVIDER</span>
                <h1>接上模型。</h1>
              </div>
              <div className="onboarding-provider-pick" role="radiogroup" aria-label="Provider">
                {presets.map((preset) => (
                  <button
                    key={preset.id}
                    className={selectedProviderId === preset.id ? "selected" : ""}
                    type="button"
                    role="radio"
                    aria-checked={selectedProviderId === preset.id}
                    onClick={() => selectProvider(preset.id)}
                  >{preset.label}</button>
                ))}
              </div>
              <form className="onboarding-provider-object" onSubmit={(event) => { event.preventDefault(); void saveProvider(); }}>
                <div className="onboarding-provider-name">
                  <b>{providerCode(selectedPreset)}</b>
                  <div><small title={providerBaseUrl}>{providerBaseUrl}</small><strong>{providerLabel}</strong><span>{providerModel}</span></div>
                </div>
                <label className={`onboarding-secret${error?.includes("密钥") ? " error" : providerReady ? " saved" : ""}`}>
                  <span>访问密钥</span>
                  <div>
                    <input
                      type="password"
                      autoComplete="off"
                      value={apiKey}
                      onChange={(event) => { setApiKey(event.target.value); setError(null); }}
                      placeholder={providerReady ? "已安全保存" : "粘贴密钥"}
                    />
                    <button type="submit" disabled={!selectedPreset || busy || (providerReady && providerActive && !apiKey)}>
                      {busy ? "保存中" : providerReady && providerActive && !apiKey ? "已就绪" : providerReady && !apiKey ? "设为默认" : "保存"}
                    </button>
                  </div>
                  <small>{error?.includes("Provider") || error?.includes("密钥") ? error : providerNotice ?? (providerReady ? "已在系统凭据库中。" : "只进系统凭据库。")}</small>
                </label>
                {providerBootstrapError && (
                  <div className="onboarding-bootstrap-note" role="status" title={providerBootstrapError}>
                    <span>{bootstrapErrors.catalog ? "模型服务暂未载入；可继续浏览。" : "未读到已有配置；可重新填写。"}</span>
                    <button className="onboarding-retry" type="button" onClick={() => load(false)}>重试</button>
                  </div>
                )}
              </form>
            </article>

            <article className="onboarding-slide onboarding-scope" aria-hidden={step !== 3} inert={step !== 3 ? "" : undefined}>
              <div className="onboarding-ad-title onboarding-dark-title">
                <span>03 / WORKSPACE</span>
                <h1>圈定代码。</h1>
                <p>工作区就是边界。</p>
              </div>
              <div className={`onboarding-scope-controls${scope === "chat" ? " chat-only" : ""}`}>
                <div className="onboarding-scope-mode" role="radiogroup" aria-label="工作区模式">
                  <button type="button" role="radio" aria-checked={scope === "chat"} disabled={engine === "codex"} className={scope === "chat" ? "selected" : ""} onClick={() => chooseScope("chat")}>纯聊天</button>
                  <button type="button" role="radio" aria-checked={scope === "workspace"} className={scope === "workspace" ? "selected" : ""} onClick={() => chooseScope("workspace")}>附加工作区</button>
                </div>
                <section className="onboarding-workspace-object">
                  <span>{selectedWorkspace ? "本次工作区" : "本地代码边界"}</span>
                  <strong title={selectedWorkspace?.canonical_path}>{selectedWorkspace?.canonical_path ?? (scope === "chat" ? "不读取本地文件" : "尚未选择")}</strong>
                  <button type="button" hidden={scope === "chat"} disabled={busy} onClick={() => void chooseWorkspace()}>{selectedWorkspace ? "更换" : "选择文件夹"}</button>
                </section>
                <div className="onboarding-access-pick" role="radiogroup" aria-label="项目权限">
                  {ACCESS_OPTIONS.map((option) => (
                    <button key={option.value} type="button" role="radio" aria-checked={accessMode === option.value} className={accessMode === option.value ? "selected" : ""} disabled={scope === "chat" || busy} onClick={() => void changeAccessMode(option.value)}>{option.label}</button>
                  ))}
                </div>
                <small>{workspaceNotice ?? (scope === "chat" ? "R-Code 仍可聊天。" : selectedWorkspace ? `${selectedAccess.detail}；路径始终受工作区边界限制。` : "先选择一个文件夹。")}</small>
                {error && step === 3 && <small className="onboarding-inline-error" role="alert">{error}</small>}
              </div>
            </article>

            <article className="onboarding-slide onboarding-launch" aria-hidden={step !== 4} inert={step !== 4 ? "" : undefined}>
              <div className="onboarding-launch-copy">
                <span>{engine === "r_code" ? (providerReady ? "READY" : "CHECK") : codexReady && selectedWorkspace ? "READY" : "CHECK"}</span>
                <h1>{engine === "r_code" ? (providerReady ? "开工。" : "差一步。") : codexReady && selectedWorkspace ? "开工。" : "差一步。"}</h1>
                <p>{summaryLine}</p>
                <small>{scope === "workspace" && selectedWorkspace ? `${selectedAccess.label} · ${selectedAccess.detail}` : "不附加工作区"}</small>
                <button type="button" disabled={busy} onClick={() => void finish()}><strong>{busy ? "正在保存" : "创建会话"}</strong><i>→</i></button>
                {error && step === 4 && <em role="alert">{error}</em>}
              </div>
              <div className="onboarding-launch-brand"><img src={brandIcon} alt="" /><span>R-CODE</span></div>
            </article>
          </div>
        </section>

        <footer className="onboarding-footer">
          <button type="button" disabled={step === 0 || busy} onClick={() => setStep((current) => Math.max(0, current - 1))}><span>←</span><i>返回</i></button>
          <nav aria-label="步骤">
            {STEPS.map((label, index) => <button key={label} className={`onboarding-dot${index === step ? " active" : ""}`} type="button" aria-label={label} aria-current={index === step ? "step" : undefined} onClick={() => setStep(index)} />)}
          </nav>
          <button type="button" disabled={busy} onClick={() => step === STEPS.length - 1 ? void finish() : setStep((current) => current + 1)}><i>{step === STEPS.length - 1 ? "完成" : "下一页"}</i><span>→</span></button>
        </footer>
      </section>

      {confirmClose && (
        <section className="onboarding-confirm" ref={confirmRef} role="alertdialog" aria-modal="true" aria-labelledby="onboarding-confirm-title">
          <div><b>PAUSE</b><h2 id="onboarding-confirm-title">稍后设置？</h2><p>可从“帮助 → 首次设置”重新打开。</p><span><button type="button" onClick={() => setConfirmClose(false)}>继续设置</button><button type="button" onClick={dismiss}>进入工作台</button></span></div>
        </section>
      )}
    </div>,
    document.body,
  );
}
