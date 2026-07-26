import { useCallback, useEffect, useRef, useState } from "react";
import { useAppStore } from "../../store/app";
import { useTasksStore, selectNeedsYou } from "../../store/tasks";
import {
  agentSend,
  recoveryCleanup,
  recoveryData,
  settingsGet,
  taskCreate,
  workspaceChoose,
  workspaceSetAccessMode,
} from "../../lib/ipc";
import { usePoll } from "../../lib/poll";
import { errText } from "../../lib/format";
import type { ProjectAccessMode, RecoveryPageData } from "../../lib/types";
import {
  ProjectAccessSelector,
  projectAccessModeLabel,
} from "../ProjectAccessSelector";
import {
  IconAlert,
  IconAttach,
  IconChevronDown,
  IconProjects,
  IconSend,
} from "../icons";

interface ProviderChoice {
  name: string;
  model: string;
  ready: boolean;
}

function providerLabel(name: string) {
  return ({
    anthropic: "Anthropic",
    openai: "OpenAI",
    deepseek: "DeepSeek",
    openrouter: "OpenRouter",
  } as Record<string, string>)[name] ?? name;
}

/**
 * 新对话页：Provider-first。
 *
 * 工作区只是一项可选的本地能力范围：未附加时照常聊天；附加后本地能力始终
 * 限于该文件夹，Agent 的自主操作由项目级权限模式决定。
 */
export function HomeScene() {
  const setScene = useAppStore((s) => s.setScene);
  const openRoom = useAppStore((s) => s.openRoom);
  const workspaces = useTasksStore((s) => s.workspaces);
  const currentWorkspacePath = useTasksStore((s) => s.currentProjectId);
  const setCurrentWorkspace = useTasksStore((s) => s.setCurrentProject);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const refreshDetails = useTasksStore((s) => s.refreshDetails);
  const refreshWorkspaces = useTasksStore((s) => s.refreshWorkspaces);
  const needsYou = useTasksStore(selectNeedsYou);

  const [goal, setGoal] = useState("");
  const [provider, setProvider] = useState<ProviderChoice | null>(null);
  const [providerChoices, setProviderChoices] = useState<ProviderChoice[]>([]);
  const [providerMenuOpen, setProviderMenuOpen] = useState(false);
  const [scopeMenuOpen, setScopeMenuOpen] = useState(false);
  const [launching, setLaunching] = useState(false);
  const [selectingFolder, setSelectingFolder] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [recovery, setRecovery] = useState<RecoveryPageData | null>(null);
  const [cleaning, setCleaning] = useState(false);

  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const scopeRef = useRef<HTMLDivElement>(null);
  const providerRef = useRef<HTMLDivElement>(null);
  const currentWorkspace = workspaces.find((w) => w.canonical_path === currentWorkspacePath);
  const providerReady = provider?.ready ?? false;
  const canSend = providerReady && goal.trim().length > 0 && !launching;

  usePoll(async () => {
    await refreshTasks();
    const activeIds = useTasksStore
      .getState()
      .tasks.filter((task) => task.state === "in_progress" || task.state === "exploring" || task.state === "review_ready")
      .map((task) => task.id);
    if (activeIds.length > 0) await refreshDetails(activeIds);
  }, 2500);

  const loadProvider = useCallback(async () => {
    try {
      const result = await settingsGet();
      const name = result.config.default_provider ?? "";
      const choices = Object.entries(result.config.providers ?? {}).map(([providerName, profile]) => ({
        name: providerName,
        model: profile.model || providerName,
        ready: Boolean(result.provider_status?.[providerName]?.ready),
      }));
      setProviderChoices(choices);
      setProvider(choices.find((choice) => choice.name === name) ?? null);
    } catch (cause) {
      setError(`读取模型服务设置失败：${errText(cause)}`);
    }
  }, []);

  useEffect(() => {
    void loadProvider();
  }, [loadProvider]);

  const loadRecovery = useCallback(async () => {
    try {
      setRecovery(await recoveryData());
    } catch (cause) {
      setError(`检查恢复状态失败：${errText(cause)}`);
    }
  }, []);

  useEffect(() => {
    void loadRecovery();
  }, [loadRecovery]);

  useEffect(() => {
    const element = textareaRef.current;
    if (!element) return;
    element.style.height = "auto";
    element.style.height = `${Math.min(element.scrollHeight, 196)}px`;
  }, [goal]);

  useEffect(() => {
    if (!scopeMenuOpen && !providerMenuOpen) return;
    const onPointerDown = (event: MouseEvent) => {
      const target = event.target as Node;
      if (scopeRef.current && !scopeRef.current.contains(target)) {
        setScopeMenuOpen(false);
      }
      if (providerRef.current && !providerRef.current.contains(target)) setProviderMenuOpen(false);
    };
    document.addEventListener("mousedown", onPointerDown);
    return () => document.removeEventListener("mousedown", onPointerDown);
  }, [providerMenuOpen, scopeMenuOpen]);

  const selectFolder = async () => {
    if (selectingFolder) return;
    setSelectingFolder(true);
    setError(null);
    try {
      const workspace = await workspaceChoose();
      if (!workspace) return;
      await refreshWorkspaces();
      setCurrentWorkspace(workspace.canonical_path);
      setScopeMenuOpen(false);
    } catch (cause) {
      setError(`选择文件夹失败：${errText(cause)}`);
    } finally {
      setSelectingFolder(false);
    }
  };

  const setWorkspaceAccessMode = async (accessMode: ProjectAccessMode) => {
    if (!currentWorkspace) return;
    try {
      await workspaceSetAccessMode(currentWorkspace.canonical_path, accessMode);
      await refreshWorkspaces();
    } catch (cause) {
      setError(`无法更新项目权限：${errText(cause)}`);
    }
  };

  const chooseProvider = (choice: ProviderChoice) => {
    if (!choice.ready) return;
    setError(null);
    // 新对话页的选择只作用于即将创建的会话，不能悄悄改写全局默认服务。
    setProvider(choice);
    setProviderMenuOpen(false);
  };

  const send = async () => {
    const text = goal.trim();
    if (!text || launching) return;
    if (!providerReady) {
      setError("先连接并保存一个模型服务，随后即可直接开始聊天。");
      return;
    }
    setLaunching(true);
    setError(null);
    let stage = "创建会话";
    try {
      const task = await taskCreate(
        currentWorkspacePath,
        text.slice(0, 48),
        text,
        currentWorkspacePath ? "edit" : "ask",
        provider?.name ?? null
      );
      stage = "发送消息";
      await agentSend(task.id, text);
      await refreshTasks().catch(() => {});
      setGoal("");
      openRoom(task.id);
    } catch (cause) {
      setError(`${stage}失败：${errText(cause)}`);
    } finally {
      setLaunching(false);
    }
  };

  const cleanRecovery = async () => {
    if (cleaning) return;
    setCleaning(true);
    try {
      await recoveryCleanup();
      await loadRecovery();
      await refreshTasks();
    } catch (cause) {
      setError(`清理恢复项失败：${errText(cause)}`);
    } finally {
      setCleaning(false);
    }
  };

  const hasRecovery = Boolean(
    recovery && (recovery.interrupted_tasks.length > 0 || recovery.orphaned_permissions > 0),
  );

  return (
    <div className="scene scene-home">
      <div className="home-stage">
        <div className="home-eyebrow">
          <span className={`status-dot${providerReady ? " ready" : ""}`} />
          {providerReady ? `${provider?.model ?? "模型服务"} 已就绪` : "需要连接模型服务"}
          {needsYou.length > 0 && (
            <button className="quiet-link" onClick={() => setScene("inbox")}>
              {needsYou.length} 项待处理
            </button>
          )}
        </div>

        <h1>从一句话开始。</h1>
        <p className="home-subtitle">
          {providerReady
            ? "先聊想法；需要读取或修改代码时，再附加一个文件夹。"
            : "连接任意兼容模型服务后即可聊天，无需先设置工作区。"}
        </p>

        {hasRecovery && recovery && (
          <div className="home-notice" role="status">
            <IconAlert width={14} height={14} />
            <span>
              上次有 {recovery.interrupted_tasks.length} 个中断任务、{recovery.orphaned_permissions} 项待清理。
            </span>
            <button className="quiet-link" disabled={cleaning} onClick={() => void cleanRecovery()}>
              {cleaning ? "清理中…" : "现在处理"}
            </button>
          </div>
        )}

        <div className="chat-composer home-composer">
          <textarea
            ref={textareaRef}
            rows={1}
            value={goal}
            placeholder={providerReady ? "描述你想完成的事…" : "先在设置中连接模型服务…"}
            onChange={(event) => setGoal(event.target.value)}
            onKeyDown={(event) => {
              if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
                event.preventDefault();
                void send();
              }
            }}
          />
          <div className="chat-composer-foot">
            <div className="scope-control" ref={scopeRef}>
              <button
                className={`scope-pill${currentWorkspace ? " attached" : ""}`}
                onClick={() => setScopeMenuOpen((open) => !open)}
                title={currentWorkspace?.canonical_path ?? "未附加工作区（纯聊天）"}
              >
                <IconProjects width={14} height={14} />
                <span>{currentWorkspace?.display_name ?? "未附加文件夹"}</span>
                <IconChevronDown width={12} height={12} />
              </button>
              {scopeMenuOpen && (
                <div className="scope-menu" role="menu">
                  <button
                    className="scope-menu-item"
                    onClick={() => {
                      setCurrentWorkspace(null);
                      setScopeMenuOpen(false);
                    }}
                  >
                    <span>仅聊天</span>
                    <small>不读取本地文件</small>
                  </button>
                  {workspaces.map((workspace) => (
                    <button
                      key={workspace.canonical_path}
                      className={`scope-menu-item${workspace.canonical_path === currentWorkspacePath ? " selected" : ""}`}
                      onClick={() => {
                        setCurrentWorkspace(workspace.canonical_path);
                        setScopeMenuOpen(false);
                      }}
                    >
                      <span>{workspace.display_name}</span>
                      <small>{projectAccessModeLabel(workspace.access_mode)}</small>
                    </button>
                  ))}
                  <div className="scope-menu-separator" />
                  <button className="scope-menu-item action" onClick={() => void selectFolder()}>
                    <IconAttach width={14} height={14} />
                    <span>{selectingFolder ? "正在打开…" : "选择文件夹…"}</span>
                  </button>
                  <button className="scope-menu-item action" onClick={() => setScene("projects")}>
                    <IconProjects width={14} height={14} />
                    <span>管理工作区</span>
                  </button>
                </div>
              )}
            </div>

            <div className="provider-control" ref={providerRef}>
              <button
                className={`provider-pill${providerReady ? " ready" : ""}`}
                onClick={() => setProviderMenuOpen((open) => !open)}
                title={providerReady ? `当前使用：${providerLabel(provider?.name ?? "")} / ${provider?.model}` : "选择模型服务"}
              >
                <span>{providerReady ? providerLabel(provider?.name ?? "") : "选择模型服务"}</span>
                {providerReady && <small>{provider?.model}</small>}
                <IconChevronDown width={12} height={12} />
              </button>
              {providerMenuOpen && (
                <div className="provider-menu" role="menu">
                  {providerChoices.length === 0 ? (
                    <div className="provider-menu-empty">还没有可用的模型服务。</div>
                  ) : (
                    providerChoices.map((choice) => (
                      <button
                        key={choice.name}
                        className={`provider-menu-item${choice.name === provider?.name ? " selected" : ""}`}
                        disabled={!choice.ready}
                        onClick={() => void chooseProvider(choice)}
                      >
                        <span>{providerLabel(choice.name)}</span>
                        <small>{choice.ready ? choice.model : "尚未完成配置"}</small>
                      </button>
                    ))
                  )}
                  <div className="scope-menu-separator" />
                  <button
                    className="provider-menu-item action"
                    onClick={() => {
                      setProviderMenuOpen(false);
                      setScene("settings");
                    }}
                  >
                    管理模型服务
                  </button>
                </div>
              )}
            </div>

            {currentWorkspace && (
              <ProjectAccessSelector
                value={currentWorkspace.access_mode}
                workspaceName={currentWorkspace.display_name}
                onChange={setWorkspaceAccessMode}
              />
            )}

            <span className="composer-spacer" />
            {!providerReady && (
              <button className="provider-link" onClick={() => setScene("settings")}>
                连接模型服务
              </button>
            )}
            <span className="send-hint">Ctrl + Enter</span>
            <button className="send-button" disabled={!canSend} onClick={() => void send()} aria-label="发送">
              <IconSend width={15} height={15} />
              <span>{launching ? "发送中" : "发送"}</span>
            </button>
          </div>
        </div>

        {error && (
          <div className="home-error" role="alert">
            <IconAlert width={14} height={14} />
            <span>{error}</span>
            <button onClick={() => setError(null)} aria-label="关闭错误提示">×</button>
          </div>
        )}
      </div>

    </div>
  );
}
