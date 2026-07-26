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
import { useProviders, type ProviderChoice } from "../../lib/provider";
import { Menu, MenuEmpty, MenuItem, MenuSeparator } from "../ui/Menu";
import { StatusBar } from "../ui/StatusBar";
import { keyLabel } from "../../lib/keys";
import {
  IconAlert,
  IconAttach,
  IconChevronDown,
  IconProjects,
  IconSend,
} from "../icons";

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
  const [launching, setLaunching] = useState(false);
  const [selectingFolder, setSelectingFolder] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [recovery, setRecovery] = useState<RecoveryPageData | null>(null);
  const [cleaning, setCleaning] = useState(false);

  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const { choices: providerChoices, fallback, error: providerError } = useProviders([]);
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

  // provider 列表来自共享 hook；这里只维护"本次要创建的会话用哪个"。
  useEffect(() => {
    setProvider((current) => {
      if (current && providerChoices.some((choice) => choice.name === current.name)) return current;
      return providerChoices.find((choice) => choice.name === fallback) ?? null;
    });
  }, [providerChoices, fallback]);

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

  const selectFolder = async () => {
    if (selectingFolder) return;
    setSelectingFolder(true);
    setError(null);
    try {
      const workspace = await workspaceChoose();
      if (!workspace) return;
      await refreshWorkspaces();
      setCurrentWorkspace(workspace.canonical_path);
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
          <StatusBar
            kind="warn"
            action={{ label: cleaning ? "清理中…" : "现在处理", onClick: () => void cleanRecovery(), disabled: cleaning }}
          >
            上次有 {recovery.interrupted_tasks.length} 个中断任务、{recovery.orphaned_permissions} 项待清理。
          </StatusBar>
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
            <Menu
              className="scope-control"
              label="会话可访问的文件夹"
              placement="up"
              align="left"
              menuClassName="scope-menu"
              trigger={
                <button
                  className={`scope-pill${currentWorkspace ? " attached" : ""}`}
                  title={currentWorkspace?.canonical_path ?? "未附加工作区（纯聊天）"}
                >
                  <IconProjects width={14} height={14} />
                  <span>{currentWorkspace?.display_name ?? "未附加文件夹"}</span>
                  <IconChevronDown width={12} height={12} />
                </button>
              }
            >
              {({ close }) => (
                <>
                  <MenuItem
                    close={close}
                    checked={currentWorkspacePath === null}
                    hint="不读取本地文件"
                    onSelect={() => setCurrentWorkspace(null)}
                  >
                    仅聊天
                  </MenuItem>
                  {workspaces.map((workspace) => (
                    <MenuItem
                      key={workspace.canonical_path}
                      close={close}
                      checked={workspace.canonical_path === currentWorkspacePath}
                      hint={projectAccessModeLabel(workspace.access_mode)}
                      onSelect={() => setCurrentWorkspace(workspace.canonical_path)}
                    >
                      {workspace.display_name}
                    </MenuItem>
                  ))}
                  <MenuSeparator />
                  <MenuItem close={close} onSelect={() => void selectFolder()}>
                    <IconAttach width={14} height={14} />
                    {selectingFolder ? "正在打开…" : "选择文件夹…"}
                  </MenuItem>
                  <MenuItem close={close} onSelect={() => setScene("projects")}>
                    <IconProjects width={14} height={14} />
                    管理工作区
                  </MenuItem>
                </>
              )}
            </Menu>

            <Menu
              className="provider-control"
              label="选择模型服务"
              placement="up"
              align="left"
              menuClassName="provider-menu"
              trigger={
                <button
                  className={`provider-pill${providerReady ? " ready" : ""}`}
                  title={
                    providerReady
                      ? `当前使用：${provider?.label} / ${provider?.model}`
                      : "选择模型服务"
                  }
                >
                  <span>{providerReady ? provider?.label : "选择模型服务"}</span>
                  {providerReady && <small>{provider?.model}</small>}
                  <IconChevronDown width={12} height={12} />
                </button>
              }
            >
              {({ close }) => (
                <>
                  {providerChoices.length === 0 && <MenuEmpty>还没有可用的模型服务。</MenuEmpty>}
                  {providerChoices.map((choice) => (
                    <MenuItem
                      key={choice.name}
                      close={close}
                      checked={choice.name === provider?.name}
                      hint={choice.ready ? choice.model : "尚未完成配置"}
                      disabled={!choice.ready}
                      onSelect={() => chooseProvider(choice)}
                    >
                      {choice.label}
                    </MenuItem>
                  ))}
                  <MenuSeparator />
                  <MenuItem close={close} onSelect={() => setScene("settings")}>
                    管理模型服务
                  </MenuItem>
                </>
              )}
            </Menu>

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
            <span className="send-hint">{keyLabel("new").replace(/ .*/, "")} + Enter</span>
            <button className="send-button" disabled={!canSend} onClick={() => void send()} aria-label="发送">
              <IconSend width={15} height={15} />
              <span>{launching ? "发送中" : "发送"}</span>
            </button>
          </div>
        </div>

        {(error || providerError) && (
          <StatusBar kind="error" onDismiss={error ? () => setError(null) : undefined}>
            {error ?? providerError}
          </StatusBar>
        )}
      </div>

    </div>
  );
}
