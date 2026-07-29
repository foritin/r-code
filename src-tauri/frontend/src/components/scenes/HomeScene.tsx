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
import { SlashCommandMenu } from "../SlashCommandMenu";
import { keyLabel } from "../../lib/keys";
import {
  commandUnavailableReason,
  matchingSlashCommands,
  parseSlashCommand,
  slashCommandInsertion,
  slashSearchQuery,
  workflowPrompt,
  type SlashCommandDefinition,
} from "../../lib/slash-commands";
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
  const setSearchOpen = useAppStore((s) => s.setSearchOpen);
  const openRoom = useAppStore((s) => s.openRoom);
  const setSettingsPane = useAppStore((s) => s.setSettingsPane);
  const themeMode = useAppStore((s) => s.themeMode);
  const setThemeMode = useAppStore((s) => s.setThemeMode);
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
  const [recoveryNotice, setRecoveryNotice] = useState<string | null>(null);
  const [commandNotice, setCommandNotice] = useState<string | null>(null);
  const [slashActive, setSlashActive] = useState(0);
  const [slashDismissed, setSlashDismissed] = useState(false);
  const [providerMenuRequest, setProviderMenuRequest] = useState(0);
  const [permissionMenuRequest, setPermissionMenuRequest] = useState(0);

  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const { choices: providerChoices, fallback, error: providerError } = useProviders([]);
  const currentWorkspace = workspaces.find((w) => w.canonical_path === currentWorkspacePath);
  const providerReady = provider?.ready ?? false;
  const slashContext = {
    location: "home" as const,
    workspaceAttached: Boolean(currentWorkspace),
    running: false,
  };
  const slashItems = slashDismissed ? [] : matchingSlashCommands(goal, slashContext);
  const slashOpen = slashItems.length > 0;
  const canSend = goal.trim().length > 0 && !launching && (providerReady || goal.trim().startsWith("/"));

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

  useEffect(() => {
    if (slashActive < slashItems.length) return;
    setSlashActive(Math.max(0, slashItems.length - 1));
  }, [slashActive, slashItems.length]);

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

  const launchConversation = async (message: string, title: string) => {
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
        title.slice(0, 48),
        message,
        currentWorkspacePath ? "edit" : "ask",
        provider?.name ?? null
      );
      stage = "发送消息";
      await agentSend(task.id, message);
      await refreshTasks().catch(() => {});
      setGoal("");
      openRoom(task.id);
    } catch (cause) {
      setError(`${stage}失败：${errText(cause)}`);
    } finally {
      setLaunching(false);
    }
  };

  const send = async () => {
    const text = goal.trim();
    if (!text || launching) return;
    const parsed = parseSlashCommand(text);
    if (!parsed) {
      await launchConversation(text, text);
      return;
    }

    setError(null);
    setCommandNotice(null);
    const command = parsed.command;
    if (!command) {
      setError(`未知命令 /${parsed.rawName}。输入 /help 查看可用命令。`);
      return;
    }
    const unavailable = commandUnavailableReason(command, slashContext);
    if (unavailable) {
      setError(`/${command.name} ${unavailable}`);
      return;
    }
    if (!parsed.args && command.argumentHint?.startsWith("<")) {
      setError(`/${command.name} 需要参数：${command.argumentHint}`);
      return;
    }
    if (command.kind === "workflow") {
      const title = `/${command.name}${parsed.args ? ` ${parsed.args}` : ""}`;
      await launchConversation(workflowPrompt(command, parsed.args), title);
      return;
    }

    switch (command.name) {
      case "clear":
        setGoal("");
        setCommandNotice("输入已清空；这里还没有会话上下文，不需要另外创建 session。");
        return;
      case "resume":
        setGoal("");
        setScene("conversations");
        return;
      case "context":
        setGoal("");
        setCommandNotice(
          `${currentWorkspace ? `将附加 ${currentWorkspace.display_name} · ${projectAccessModeLabel(currentWorkspace.access_mode)}` : "将以纯聊天开始"}；` +
          `模型 ${provider?.label ?? "尚未选择"} / ${provider?.model ?? "—"}。`,
        );
        return;
      case "model":
        setGoal("");
        setProviderMenuRequest((value) => value + 1);
        return;
      case "search":
        setGoal("");
        setSearchOpen(true);
        return;
      case "pending":
        setGoal("");
        setScene("inbox");
        return;
      case "activity":
        setGoal("");
        setScene("deck");
        return;
      case "projects":
        setGoal("");
        setScene("projects");
        return;
      case "permissions":
        setGoal("");
        setPermissionMenuRequest((value) => value + 1);
        return;
      case "memory":
        setGoal("");
        setScene("projects");
        return;
      case "theme": {
        const requested = parsed.args.toLowerCase();
        const next = requested || (themeMode === "light" ? "dark" : themeMode === "dark" ? "system" : "light");
        if (next !== "light" && next !== "dark" && next !== "system") {
          setError("主题只支持 light、dark 或 system");
          return;
        }
        setThemeMode(next);
        setGoal("");
        setCommandNotice(`外观已切换为 ${next === "light" ? "亮色" : next === "dark" ? "暗色" : "跟随系统"}。`);
        return;
      }
      case "settings":
        setGoal("");
        setSettingsPane("providers");
        return;
      case "mcp":
      case "plugins":
        setGoal("");
        setSettingsPane("codex");
        return;
      case "skills":
        setGoal("");
        setCommandNotice("内置工作流：/plan、/doctor、/debug、/fix、/explain、/init、/code-review、/security-review、/simplify、/docs、/research、/qa。输入 / 后可搜索并查看说明。");
        return;
      case "help":
        setGoal("");
        setCommandNotice("会话：/clear /resume /context；导航：/search /pending /activity /projects /model /permissions；工作流与扩展可输入 / 搜索，或使用 /skills、/mcp、/plugins。");
        return;
      default:
        setError(`命令 /${command.name} 需要先进入一个会话`);
    }
  };

  const pickSlash = (command: SlashCommandDefinition) => {
    const next = slashCommandInsertion(command);
    setGoal(next);
    setSlashActive(0);
    setSlashDismissed(false);
    requestAnimationFrame(() => {
      const textarea = textareaRef.current;
      if (!textarea) return;
      textarea.focus();
      textarea.setSelectionRange(next.length, next.length);
    });
  };

  const cleanRecovery = async () => {
    if (cleaning) return;
    setCleaning(true);
    setRecoveryNotice(null);
    try {
      const result = await recoveryCleanup();
      await loadRecovery();
      await refreshTasks();
      setRecoveryNotice(
        `已收束 ${result.runs_closed} 个遗留运行、结束 ${result.tool_calls_closed} 个工具调用，并取消 ${result.permissions_denied} 项未完成授权。`,
      );
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
            上次退出留下 {recovery.interrupted_tasks.length} 个中断任务、{recovery.orphaned_permissions} 项待处理；不会影响本次新运行。
          </StatusBar>
        )}

        {recoveryNotice && <StatusBar kind="ok">{recoveryNotice}</StatusBar>}
        {commandNotice && (
          <StatusBar kind="info" onDismiss={() => setCommandNotice(null)}>
            {commandNotice}
          </StatusBar>
        )}

        <div className="chat-composer home-composer">
          {slashOpen && (
            <SlashCommandMenu
              value={goal}
              context={slashContext}
              activeIndex={slashActive}
              onActiveIndexChange={setSlashActive}
              onPick={pickSlash}
            />
          )}
          <textarea
            ref={textareaRef}
            rows={1}
            value={goal}
            aria-label="描述新任务"
            aria-controls={slashOpen ? "slash-command-menu" : undefined}
            aria-activedescendant={slashOpen ? `slash-command-option-${slashActive}` : undefined}
            placeholder={providerReady ? "描述你想完成的事…" : "先在设置中连接模型服务…"}
            onChange={(event) => {
              setGoal(event.target.value);
              setSlashActive(0);
              setSlashDismissed(false);
            }}
            onKeyDown={(event) => {
              if (slashOpen) {
                if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                  event.preventDefault();
                  const delta = event.key === "ArrowDown" ? 1 : -1;
                  setSlashActive((slashActive + delta + slashItems.length) % slashItems.length);
                  return;
                }
                if (event.key === "Escape") {
                  event.preventDefault();
                  setSlashDismissed(true);
                  return;
                }
                if (event.key === "Tab") {
                  event.preventDefault();
                  const command = slashItems[slashActive];
                  const unavailable = commandUnavailableReason(command, slashContext);
                  if (unavailable) setError(`/${command.name} ${unavailable}`);
                  else pickSlash(command);
                  return;
                }
                if (event.key === "Enter" && !event.shiftKey) {
                  const query = slashSearchQuery(goal);
                  const command = slashItems[slashActive];
                  const exact = query === command.name || command.aliases?.includes(query ?? "");
                  if (!exact) {
                    event.preventDefault();
                    const unavailable = commandUnavailableReason(command, slashContext);
                    if (unavailable) setError(`/${command.name} ${unavailable}`);
                    else pickSlash(command);
                    return;
                  }
                }
              }
              if (event.key === "Enter" && !event.shiftKey && parseSlashCommand(goal.trim())) {
                event.preventDefault();
                void send();
                return;
              }
              if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
                event.preventDefault();
                void send();
              }
            }}
          />
          <div className="chat-composer-foot">
            <div className="composer-context">
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
                openRequest={providerMenuRequest}
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
                  openRequest={permissionMenuRequest}
                />
              )}
            </div>

            <div className="composer-actions">
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
