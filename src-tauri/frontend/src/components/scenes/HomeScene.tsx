import { useCallback, useEffect, useRef, useState } from "react";
import { useAppStore } from "../../store/app";
import { useTasksStore, selectNeedsYou } from "../../store/tasks";
import {
  agentSend,
  codexIntegrationStatus,
  recoveryCleanup,
  recoveryData,
  settingsGet,
  taskCreate,
  taskSetInference,
  taskSetModel,
  workspaceChoose,
  workspaceSetAccessMode,
  workflowSkillsList,
} from "../../lib/ipc";
import { usePoll } from "../../lib/poll";
import { errText } from "../../lib/format";
import { RUNTIME_SETTINGS_CHANGED_EVENT } from "../../lib/onboarding";
import type {
  CodexCliPreferences,
  CodexIntegrationStatus,
  InferenceOptions,
  AttachmentInput,
  ProjectAccessMode,
  RecoveryPageData,
  TaskAgentEngine,
  WorkflowSkill,
} from "../../lib/types";
import {
  ProjectAccessSelector,
  projectAccessModeLabel,
} from "../ProjectAccessSelector";
import { useProviders, type ProviderChoice } from "../../lib/provider";
import { Menu, MenuItem, MenuSeparator } from "../ui/Menu";
import { StatusBar } from "../ui/StatusBar";
import { SlashCommandMenu } from "../SlashCommandMenu";
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
  IconArrowRight,
  IconAttach,
  IconChevronDown,
  IconProjects,
  IconSend,
  IconShield,
  IconSubagent,
  IconTerminal,
} from "../icons";
import { ModelSwitcher } from "../room/ModelSwitcher";
import { CodexModelConfiguration } from "../room/CodexModelConfiguration";
import {
  AttachmentButton,
  AttachmentTray,
  firstBlockedAttachmentReason,
  sendableAttachmentInputs,
  useAttachments,
  type DraftAttachment,
} from "../Attachments";
import {
  attachmentCapabilityFor,
  codexImageCapability,
  imageCapabilityFor,
} from "../room/model-capabilities";

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
  const [draftModel, setDraftModel] = useState<string | null>(null);
  const [draftInference, setDraftInference] = useState<InferenceOptions>({});
  const [codexPreferences, setCodexPreferences] = useState<CodexCliPreferences | null>(null);
  const [agentEngine, setAgentEngine] = useState<TaskAgentEngine>("r_code");
  const [codexStatus, setCodexStatus] = useState<CodexIntegrationStatus | null>(null);
  const [launching, setLaunching] = useState(false);
  const [selectingFolder, setSelectingFolder] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [recovery, setRecovery] = useState<RecoveryPageData | null>(null);
  const [cleaning, setCleaning] = useState(false);
  const [recoveryNotice, setRecoveryNotice] = useState<string | null>(null);
  const [commandNotice, setCommandNotice] = useState<string | null>(null);
  const [slashActive, setSlashActive] = useState(0);
  const [slashDismissed, setSlashDismissed] = useState(false);
  const [modelMenuRequest, setModelMenuRequest] = useState(0);
  const [permissionMenuRequest, setPermissionMenuRequest] = useState(0);
  const [workflowSkills, setWorkflowSkills] = useState<WorkflowSkill[]>([]);

  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const composerRef = useRef<HTMLDivElement>(null);
  const attachments = useAttachments();
  const { choices: providerChoices, fallback, error: providerError } = useProviders([]);
  // provider 列表可能在 HomeScene 重新挂载后的下一拍才写回本地选择；直接从
  // fallback 派生当前项，避免用户已输入目标但发送按钮短暂保持禁用。
  const activeProvider = provider ?? providerChoices.find((choice) => choice.name === fallback) ?? null;
  const activeModel = draftModel ?? activeProvider?.model ?? "";
  const imageCapability = agentEngine === "codex"
    ? codexImageCapability(codexPreferences)
    : imageCapabilityFor(activeProvider ?? undefined, activeModel);
  const capabilityForAttachment = useCallback(
    (attachment: DraftAttachment) => attachmentCapabilityFor(
      attachment.kind,
      imageCapability,
      agentEngine,
      activeProvider ?? undefined,
    ),
    [activeProvider, agentEngine, imageCapability],
  );
  const sendableAttachments = sendableAttachmentInputs(
    attachments.attachments,
    capabilityForAttachment,
  );
  const attachmentBlockedReason = firstBlockedAttachmentReason(
    attachments.attachments,
    capabilityForAttachment,
  );
  const currentWorkspace = workspaces.find((w) => w.canonical_path === currentWorkspacePath);
  const providerReady = activeProvider?.ready ?? false;
  const codexReady = Boolean(codexStatus?.integration_ready);
  const engineReady = agentEngine === "r_code"
    ? providerReady
    : Boolean(currentWorkspace && codexReady);
  const slashContext = {
    location: "home" as const,
    workspaceAttached: Boolean(currentWorkspace),
    running: false,
  };
  const slashItems = slashDismissed ? [] : matchingSlashCommands(goal, slashContext, workflowSkills);
  const slashOpen = slashItems.length > 0;
  const canSend = (goal.trim().length > 0 || sendableAttachments.length > 0)
    && !launching
    && !attachmentBlockedReason
    && (engineReady || goal.trim().startsWith("/"));

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

  // A model-created Skill is written to AppData through `save_skill`. Keep the home
  // composer catalog fresh so it becomes callable immediately, without restarting R-Code.
  usePoll(async () => {
    setWorkflowSkills(await workflowSkillsList());
  }, 2000);

  const loadRuntimeDefaults = useCallback(() => {
    let alive = true;
    void Promise.all([settingsGet(), codexIntegrationStatus()]).then(([settings, status]) => {
      if (!alive) return;
      setAgentEngine(settings.config.orchestration?.default_agent_engine ?? "r_code");
      setCodexStatus(status);
    }).catch(() => {
      // Provider/Codex readiness already has its own visible error and setup entry.
    });
    return () => { alive = false; };
  }, []);

  useEffect(() => {
    let cancel = loadRuntimeDefaults();
    const refresh = () => {
      cancel();
      cancel = loadRuntimeDefaults();
    };
    const focusComposer = () => textareaRef.current?.focus({ preventScroll: true });
    window.addEventListener(RUNTIME_SETTINGS_CHANGED_EVENT, refresh);
    window.addEventListener("r-code:new-session-ready", focusComposer);
    return () => {
      cancel();
      window.removeEventListener(RUNTIME_SETTINGS_CHANGED_EVENT, refresh);
      window.removeEventListener("r-code:new-session-ready", focusComposer);
    };
  }, [loadRuntimeDefaults]);

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

  const launchConversation = async (
    message: string,
    title: string,
    files: AttachmentInput[] = [],
  ) => {
    if (agentEngine === "r_code" && !providerReady) {
      setError("先连接并保存一个模型服务，随后即可直接开始聊天。");
      return;
    }
    if (agentEngine === "codex" && !currentWorkspace) {
      setError("Codex 主 Agent 需要先附加一个本地工作区。");
      return;
    }
    if (agentEngine === "codex" && !codexReady) {
      setError("Codex CLI 尚未完成安装、登录或 R-Code 协作配置。请先前往设置完成连接。");
      return;
    }
    const draft = goal;
    setLaunching(true);
    setError(null);
    // 会话创建与首轮发送可能涉及多次 IPC。用户按下 Enter 后立即释放输入框，
    // 若链路失败且用户没有继续输入，再恢复原草稿。
    setGoal("");
    setSlashDismissed(false);
    let stage = "创建会话";
    try {
      const task = await taskCreate(
        currentWorkspacePath,
        title.slice(0, 48),
        message || "分析附加文件",
        currentWorkspacePath ? "edit" : "ask",
        activeProvider?.name ?? null,
        agentEngine,
      );
      if (agentEngine === "r_code" && activeProvider) {
        if (activeModel && activeModel !== activeProvider.model) await taskSetModel(task.id, activeModel);
        if (Object.keys(draftInference).length > 0) await taskSetInference(task.id, draftInference);
      }
      stage = "发送消息";
      await agentSend(task.id, message, "auto", files);
      await refreshTasks().catch(() => {});
      attachments.clear();
      openRoom(task.id);
    } catch (cause) {
      setError(`${stage}失败：${errText(cause)}`);
      setGoal((current) => current.length > 0 ? current : draft);
    } finally {
      setLaunching(false);
    }
  };

  const send = async () => {
    const text = goal.trim();
    if (attachmentBlockedReason) {
      setError(attachmentBlockedReason);
      return;
    }
    if ((!text && sendableAttachments.length === 0) || launching) return;
    const parsed = text ? parseSlashCommand(text, workflowSkills) : null;
    if (!parsed) {
      const title = text || `分析 ${sendableAttachments[0]?.name ?? "附加文件"}`;
      await launchConversation(text, title, sendableAttachments);
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
          (agentEngine === "codex"
            ? "主 Agent Codex CLI；子任务可按编排策略委派给 R-Code。"
            : `主 Agent R-Code；模型 ${activeProvider?.label ?? "尚未选择"} / ${activeProvider?.model ?? "—"}。`),
        );
        return;
      case "model":
        setGoal("");
        setModelMenuRequest((value) => value + 1);
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
        setScene("knowledge");
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
        <div className="home-intro">
          <div className="home-eyebrow">
            <span className={`status-dot${engineReady ? " ready" : ""}`} />
            {engineReady ? `NEW TASK · ${agentEngine === "codex" ? "CODEX" : "R-CODE"}` : "CONNECT AGENT"}
            {needsYou.length > 0 && (
              <button className="quiet-link" onClick={() => setScene("inbox")}>
                {needsYou.length} 项待处理
              </button>
            )}
          </div>

          <h1>从结果开始，而不是从工具开始。</h1>
          <p className="home-subtitle">
            {engineReady
              ? `描述你要完成的事情。${agentEngine === "codex" ? "Codex CLI" : "R-Code"} 会在可见权限边界内执行，并把路由、子智能体与复核过程留给你查看。`
              : agentEngine === "codex"
                ? "Codex 主 Agent 需要本机 CLI、登录状态和一个已附加的工作区。"
                : "连接任意兼容模型服务后，直接描述目标；工作区仍可在需要读取或修改代码时再附加。"}
          </p>

          {engineReady && (
            <div className="home-suggestions" aria-label="任务示例">
              <button className="home-suggestion" type="button" onClick={() => setGoal("定位失败的测试，说明根因并修复。")}>
                <IconTerminal width={15} height={15} />
                <span>定位失败的测试，说明根因并修复</span>
                <IconArrowRight width={15} height={15} />
              </button>
              <button className="home-suggestion" type="button" onClick={() => setGoal("解释模块调用路径并标出关键文件。")}>
                <IconProjects width={15} height={15} />
                <span>解释模块调用路径并标出关键文件</span>
                <IconArrowRight width={15} height={15} />
              </button>
              <button className="home-suggestion" type="button" onClick={() => setGoal("审核未提交变更并指出行为回归。")}>
                <IconShield width={15} height={15} />
                <span>审核未提交变更并指出行为回归</span>
                <IconArrowRight width={15} height={15} />
              </button>
            </div>
          )}
        </div>

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

        <div className="chat-composer home-composer" ref={composerRef}>
          {slashOpen && (
            <SlashCommandMenu
              anchorRef={composerRef}
              value={goal}
              context={slashContext}
              skills={workflowSkills}
              activeIndex={slashActive}
              onActiveIndexChange={setSlashActive}
              onPick={pickSlash}
              onDismiss={() => setSlashDismissed(true)}
            />
          )}
          <textarea
            ref={textareaRef}
            rows={1}
            value={goal}
            aria-label="描述新任务"
            aria-controls={slashOpen ? "slash-command-menu" : undefined}
            aria-activedescendant={slashOpen ? `slash-command-option-${slashActive}` : undefined}
            placeholder={engineReady ? "描述你想完成的事…" : agentEngine === "codex" ? "先连接 Codex 并附加工作区…" : "先在设置中连接模型服务…"}
            onChange={(event) => {
              setGoal(event.target.value);
              setSlashActive(0);
              setSlashDismissed(false);
            }}
            onPaste={attachments.onPaste}
            onKeyDown={(event) => {
              if (event.nativeEvent.isComposing) return;
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
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault();
                void send();
              }
            }}
          />
          <AttachmentTray
            attachments={attachments.attachments}
            capabilityFor={capabilityForAttachment}
            onRemove={attachments.remove}
          />
          <div className="chat-composer-foot">
            <div className="composer-context">
              <AttachmentButton onFiles={attachments.addFiles} disabled={launching} />
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
                className="agent-engine-control"
                label="选择主 Agent"
                placement="up"
                align="left"
                trigger={
                  <button className={`provider-pill ready agent-engine-pill engine-${agentEngine}`}>
                    <IconSubagent width={14} height={14} />
                    <span>{agentEngine === "codex" ? "Codex CLI" : "R-Code"}</span>
                    <IconChevronDown width={12} height={12} />
                  </button>
                }
              >
                {({ close }) => (
                  <>
                    <MenuItem
                      close={close}
                      checked={agentEngine === "r_code"}
                      hint="使用自定义 Provider；支持宿主路由与质量复核"
                      onSelect={() => setAgentEngine("r_code")}
                    >
                      R-Code
                    </MenuItem>
                    <MenuItem
                      close={close}
                      checked={agentEngine === "codex"}
                      disabled={!codexReady || !currentWorkspace}
                      hint={!codexReady ? "先完成 Codex CLI 协作配置" : !currentWorkspace ? "先附加工作区" : "使用本机登录的 Codex CLI"}
                      onSelect={() => setAgentEngine("codex")}
                    >
                      Codex CLI
                    </MenuItem>
                    <MenuSeparator />
                    <MenuItem close={close} onSelect={() => setSettingsPane("agents")}>管理 Agent 编排</MenuItem>
                    {!codexReady && <MenuItem close={close} onSelect={() => setSettingsPane("codex")}>连接 Codex CLI</MenuItem>}
                  </>
                )}
              </Menu>

              {agentEngine === "r_code" ? (
                <ModelSwitcher
                  taskId={null}
                  providerName={activeProvider?.name ?? null}
                  model={activeModel || null}
                  inference={draftInference}
                  choices={providerChoices}
                  fallback={fallback}
                  running={launching}
                  scopeLabel="仅作用于即将创建的对话"
                  variant="pill"
                  openRequest={modelMenuRequest}
                  onDraftChanged={(selection) => {
                    setError(null);
                    setProvider(providerChoices.find((choice) => choice.name === selection.providerName) ?? null);
                    setDraftModel(selection.model);
                    setDraftInference(selection.inference);
                  }}
                />
              ) : (
                <CodexModelConfiguration
                  running={launching}
                  openRequest={modelMenuRequest}
                  preload
                  onPreferencesChange={setCodexPreferences}
                />
              )}

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
              {!engineReady && (
                <button className="provider-link" onClick={() => setSettingsPane(agentEngine === "codex" ? "codex" : "providers")}>
                  {agentEngine === "codex" ? "连接 Codex CLI" : "连接模型服务"}
                </button>
              )}
              <span className="send-hint">Enter 发送 · Shift+Enter 换行</span>
              <button
                className="send-button"
                disabled={!canSend}
                title={attachmentBlockedReason ?? "发送（Enter）"}
                onClick={() => void send()}
                aria-label="发送"
              >
                <IconSend width={15} height={15} />
                <span>{launching ? "发送中" : "发送"}</span>
              </button>
            </div>
          </div>
        </div>

        {attachments.error && (
          <StatusBar kind="error" onDismiss={attachments.clearError}>{attachments.error}</StatusBar>
        )}
        {(error || providerError) && (
          <StatusBar kind="error" onDismiss={error ? () => setError(null) : undefined}>
            {error ?? providerError}
          </StatusBar>
        )}
      </div>

    </div>
  );
}
