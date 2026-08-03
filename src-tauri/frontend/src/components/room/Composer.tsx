/**
 * Room 输入区 —— Enter 按当前选择发送 / Shift+Enter 换行；运行中可选择排队、引导或立即发送。
 * `@` 触发 quickOpen 文件下拉，选中后插入 @path 文本。
 *
 * 输入区脚下现在镜像了「模型」与「权限」两个控件（与新对话页同构）。原先这里
 * 只有一个只读的模型状态芯片，想换模型或改权限必须回到会话顶栏找。
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { flushSync } from "react-dom";
import {
  agentQueueRemove,
  agentSend,
  quickOpen,
  runVerification,
  sessionMessages,
  taskCompactContext,
  taskCreate,
  taskForkContext,
  taskRename,
  taskSetModel,
  workflowSkillsList,
} from "../../lib/ipc";
import type {
  AgentSendMode,
  AttachmentInput,
  CodexCliPreferences,
  InferenceOptions,
  ProjectAccessMode,
  QueuedMessage,
  SessionAttachmentMeta,
  TaskAgentEngine,
  WorkflowSkill,
} from "../../lib/types";
import { resolveActive, type ProviderChoice } from "../../lib/provider";
import { useAsyncAction } from "../../lib/hooks";
import { usePoll } from "../../lib/poll";
import { useTasksStore } from "../../store/tasks";
import { useAppStore } from "../../store/app";
import { Menu, MenuItem } from "../ui/Menu";
import { AnchoredSurface } from "../ui/AnchoredSurface";
import { StatusBar } from "../ui/StatusBar";
import { ProjectAccessSelector, projectAccessModeLabel } from "../ProjectAccessSelector";
import { ModelSwitcher } from "./ModelSwitcher";
import { AgentEngineSwitcher } from "./AgentEngineSwitcher";
import { CodexModelConfiguration } from "./CodexModelConfiguration";
import { IconChevronDown, IconRefresh, IconSend, IconStop } from "../icons";
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
} from "./model-capabilities";
import { SlashCommandMenu } from "../SlashCommandMenu";
import {
  SLASH_COMMANDS,
  commandUnavailableReason,
  matchingSlashCommands,
  parseSlashCommand,
  slashCommandInsertion,
  slashSearchQuery,
  workflowPrompt,
  type ParsedSlashCommand,
  type SlashCommandDefinition,
} from "../../lib/slash-commands";

function sameWorkflowSkillCatalog(left: WorkflowSkill[], right: WorkflowSkill[]): boolean {
  return left.length === right.length && left.every((skill, index) => {
    const next = right[index];
    return next != null
      && skill.id === next.id
      && skill.name === next.name
      && skill.description === next.description
      && skill.instructions === next.instructions
      && skill.source === next.source
      && skill.enabled === next.enabled
      && skill.overridden === next.overridden;
  });
}

interface Props {
  taskId: string;
  workspacePath: string | null;
  workspaceAttached: boolean;
  workspaceName: string | null;
  workspaceAccessMode: ProjectAccessMode;
  onAccessModeChange: (mode: ProjectAccessMode) => Promise<void> | void;
  scopeBusy: boolean;
  providerName: string | null;
  agentEngine: TaskAgentEngine;
  model: string | null;
  inference: InferenceOptions;
  providerChoices: ProviderChoice[];
  providerFallback: string;
  onProviderChanged: () => void;
  running: boolean;
  queuedMessages: QueuedMessage[];
  onAbort: () => Promise<void>;
  onSent: (text: string, mode: AgentSendMode, attachments?: SessionAttachmentMeta[]) => void;
  onSendFailed: () => void;
  onActivitySent: (mode: AgentSendMode) => void;
  onShowSubagents: () => void;
}

interface AtState {
  start: number; // '@' 在文本中的下标
  query: string;
  items: string[];
  active: number;
  error?: string;
}

function fileReferenceText(path: string): string {
  if (!/\s/.test(path)) return `@${path}`;
  return `@"${path.replace(/"/g, '\\"')}"`;
}

type RunningSendMode = Extract<AgentSendMode, "queue" | "steer" | "send_now">;

const RUNNING_SEND_MODE_ORDER: readonly RunningSendMode[] = ["queue", "steer", "send_now"];

function runningSendModeLabel(mode: RunningSendMode): string {
  switch (mode) {
    case "steer":
      return "引导";
    case "send_now":
      return "立即发送";
    default:
      return "排队";
  }
}

function runningSendModeTitle(mode: RunningSendMode): string {
  switch (mode) {
    case "steer":
      return "把消息补充到当前运行，不替换原任务";
    case "send_now":
      return "中断当前运行并立即处理这条消息";
    default:
      return "当前运行结束后再发送这条消息";
  }
}

function nextRunningSendMode(mode: RunningSendMode): RunningSendMode {
  const index = RUNNING_SEND_MODE_ORDER.indexOf(mode);
  return RUNNING_SEND_MODE_ORDER[(index + 1) % RUNNING_SEND_MODE_ORDER.length];
}

export function Composer({
  taskId,
  workspacePath,
  workspaceAttached,
  workspaceName,
  workspaceAccessMode,
  onAccessModeChange,
  scopeBusy,
  providerName,
  agentEngine,
  model,
  inference,
  providerChoices,
  providerFallback,
  onProviderChanged,
  running,
  queuedMessages,
  onAbort,
  onSent,
  onSendFailed,
  onActivitySent,
  onShowSubagents,
}: Props) {
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const [at, setAt] = useState<AtState | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [commandBusy, setCommandBusy] = useState(false);
  const [slashActive, setSlashActive] = useState(0);
  const [slashDismissed, setSlashDismissed] = useState(false);
  const [modelMenuRequest, setModelMenuRequest] = useState(0);
  const [permissionMenuRequest, setPermissionMenuRequest] = useState(0);
  const [codexPreferences, setCodexPreferences] = useState<CodexCliPreferences | null>(null);
  const [runningSendMode, setRunningSendMode] = useState<RunningSendMode>("queue");
  const [inputHistory, setInputHistory] = useState<string[]>([]);
  const [workflowSkills, setWorkflowSkills] = useState<WorkflowSkill[]>([]);
  const taRef = useRef<HTMLTextAreaElement>(null);
  const compBoxRef = useRef<HTMLDivElement>(null);
  const debRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const historyIndexRef = useRef<number | null>(null);
  const historyDraftRef = useRef("");
  const historyLoadedTaskRef = useRef<string | null>(null);
  const historyRequestRef = useRef<{ taskId: string; promise: Promise<string[]> } | null>(null);
  const initializedTaskRef = useRef<string | null>(null);
  const consumedFileReferencesRef = useRef(new Set<string>());

  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const setCurrentProject = useTasksStore((s) => s.setCurrentProject);
  const task = useTasksStore((s) => s.details[taskId]?.task);
  const openRoom = useAppStore((s) => s.openRoom);
  const setCanvasTab = useAppStore((s) => s.setCanvasTab);
  const setScene = useAppStore((s) => s.setScene);
  const setSearchOpen = useAppStore((s) => s.setSearchOpen);
  const setSettingsPane = useAppStore((s) => s.setSettingsPane);
  const themeMode = useAppStore((s) => s.themeMode);
  const setThemeMode = useAppStore((s) => s.setThemeMode);
  const taskFileReference = useAppStore((s) => s.taskFileReferences[taskId]);
  const acknowledgeTaskFileReference = useAppStore((s) => s.acknowledgeTaskFileReference);
  const attachments = useAttachments();
  const activeModel = resolveActive(providerChoices, providerFallback, providerName, model);
  const imageCapability = agentEngine === "codex"
    ? codexImageCapability(codexPreferences)
    : imageCapabilityFor(activeModel.provider, activeModel.model);
  const capabilityForAttachment = useCallback(
    (attachment: DraftAttachment) => attachmentCapabilityFor(
      attachment.kind,
      imageCapability,
      agentEngine,
      activeModel.provider,
    ),
    [activeModel.provider, agentEngine, imageCapability],
  );
  const sendableAttachments = sendableAttachmentInputs(
    attachments.attachments,
    capabilityForAttachment,
  );
  const capabilityBlockedReason = firstBlockedAttachmentReason(
    attachments.attachments,
    capabilityForAttachment,
  );
  const runBlockedReason = running && attachments.attachments.length > 0
    ? "当前运行结束后才能把附件作为新一轮消息发送。"
    : null;
  const attachmentBlockedReason = runBlockedReason ?? capabilityBlockedReason;

  const slashContext = {
    location: "room" as const,
    workspaceAttached,
    running,
  };
  const slashItems = slashDismissed ? [] : matchingSlashCommands(text, slashContext, workflowSkills);
  const slashOpen = slashItems.length > 0;

  useEffect(() => () => {
    if (debRef.current) window.clearTimeout(debRef.current);
  }, []);

  useEffect(() => {
    // React Strict Mode replays mount effects. Reset only for a genuine task transition,
    // otherwise the replay can erase a file reference that the following effect consumed.
    if (initializedTaskRef.current === taskId) return;
    initializedTaskRef.current = taskId;
    setText("");
    setInputHistory([]);
    historyIndexRef.current = null;
    historyDraftRef.current = "";
    historyLoadedTaskRef.current = null;
    historyRequestRef.current = null;
    setError(null);
    setNotice(null);
    setAt(null);
    setSlashActive(0);
    setSlashDismissed(false);
    setRunningSendMode("queue");
    attachments.clear();
  }, [taskId, attachments.clear]);

  useEffect(() => {
    // 每轮运行重新从最安全的“排队”开始；用户仍可在当前运行内显式切换。
    if (!running) setRunningSendMode("queue");
  }, [running]);

  useEffect(() => {
    if (!taskFileReference) return;
    const requestKey = `${taskId}:${taskFileReference.requestId}`;
    if (consumedFileReferencesRef.current.has(requestKey)) return;
    consumedFileReferencesRef.current.add(requestKey);
    const reference = fileReferenceText(taskFileReference.path);
    setText((current) => `${current}${current && !/\s$/.test(current) ? " " : ""}${reference}`);
    setAt(null);
    setSlashDismissed(true);
    requestAnimationFrame(() => {
      const textarea = taRef.current;
      if (!textarea) return;
      textarea.focus();
      textarea.setSelectionRange(textarea.value.length, textarea.value.length);
    });
    acknowledgeTaskFileReference(taskId, taskFileReference.requestId);
  }, [acknowledgeTaskFileReference, taskFileReference, taskId]);

  const loadInputHistory = useCallback(() => {
    const inFlight = historyRequestRef.current;
    if (inFlight?.taskId === taskId) return inFlight.promise;
    const promise = sessionMessages(taskId).then((messages) => messages.flatMap((message) => {
      const value = message.kind === "message" && message.role === "user"
        ? message.text?.trim()
        : null;
      return value && !value.startsWith("[system]") ? [value] : [];
    }));
    historyRequestRef.current = { taskId, promise };
    void promise.catch(() => {
      if (historyRequestRef.current?.promise === promise) historyRequestRef.current = null;
    });
    return promise;
  }, [taskId]);

  useEffect(() => {
    let current = true;
    void loadInputHistory()
      .then((history) => {
        if (!current) return;
        historyLoadedTaskRef.current = taskId;
        setInputHistory(history);
      })
      .catch(() => {
        // 时间线仍会显示加载错误；输入历史只是增强能力，不应阻断发送。
      });
    return () => {
      current = false;
    };
  }, [loadInputHistory, taskId]);

  const leaveInputHistory = useCallback(() => {
    historyIndexRef.current = null;
    historyDraftRef.current = "";
  }, []);

  // `save_skill` can be called by the running model. Refresh the user catalog while this
  // composer is visible so a newly registered Skill appears in `/` completion without a
  // reload or a new conversation.
  usePoll(async () => {
    const skills = await workflowSkillsList();
    setWorkflowSkills((current) => sameWorkflowSkillCatalog(current, skills) ? current : skills);
  }, 2000);

  const rememberInput = useCallback((value: string) => {
    const normalized = value.trim();
    if (normalized) setInputHistory((history) => [...history, normalized]);
    leaveInputHistory();
  }, [leaveInputHistory]);

  const showHistoryValue = useCallback((value: string) => {
    // History navigation and the next keystroke can happen in adjacent browser tasks.
    // Commit the controlled value before returning from the key handler so a fast edit
    // cannot race React's batched render and append to the value being restored.
    flushSync(() => {
      setText(value);
      setAt(null);
      setSlashDismissed(true);
    });
    const textarea = taRef.current;
    if (!textarea) return;
    textarea.focus();
    textarea.setSelectionRange(value.length, value.length);
  }, []);

  useEffect(() => {
    if (slashActive < slashItems.length) return;
    setSlashActive(Math.max(0, slashItems.length - 1));
  }, [slashActive, slashItems.length]);

  // `@` 文件引用：仅在已附加工作区中触发文件搜索。
  const detectAt = useCallback((value: string, pos: number) => {
    const m = /(?:^|\s)@([^\s@]*)$/.exec(value.slice(0, pos));
    if (!m) {
      setAt(null);
      return;
    }
    const query = m[1];
    const start = pos - query.length - 1;
    if (!workspacePath || !workspaceAttached) {
      setAt({ start, query, items: [], active: 0, error: "附加一个文件夹后才能引用本地文件" });
      return;
    }
    if (debRef.current) clearTimeout(debRef.current);
    debRef.current = setTimeout(() => {
      void quickOpen(workspacePath, query, 8)
        .then((items) => setAt({ start, query, items, active: 0 }))
        .catch((e) => setAt({ start, query, items: [], active: 0, error: String(e) }));
    }, 150);
  }, [workspacePath, workspaceAttached]);

  const pickAt = useCallback(
    (path: string) => {
      const ta = taRef.current;
      const pos = ta?.selectionStart ?? text.length;
      const insert = `@${path} `;
      const next = text.slice(0, at?.start ?? pos) + insert + text.slice(pos);
      setText(next);
      setAt(null);
      requestAnimationFrame(() => {
        if (ta) {
          ta.focus();
          const p = (at?.start ?? 0) + insert.length;
          ta.setSelectionRange(p, p);
        }
      });
    },
    [text, at]
  );

  const transmit = useCallback(async (
    message: string,
    mode: AgentSendMode,
    files: AttachmentInput[] = [],
  ) => {
    if ((!message.trim() && files.length === 0) || sending) return false;
    setSending(true);
    setError(null);
    setNotice(null);
    try {
      // 不等待 IPC 往返：运行中的引导、排队和立即发送都应立即可见。
      onSent(
        message || `已附加 ${files.length} 个文件`,
        mode,
        files.map((file) => ({
          name: file.name,
          media_type: file.mediaType,
          kind: file.mediaType.startsWith("image/")
            ? "image"
            : file.mediaType === "application/pdf"
              ? "pdf"
              : "text",
        })),
      );
      await agentSend(taskId, message, mode, files);
      // IPC 成功后才把引导标为"已接纳"，失败时由下方 catch 回滚时间线。
      onActivitySent(mode);
      rememberInput(message);
      await refreshDetail(taskId);
      return true;
    } catch (e) {
      // 后端在无 provider 配置等情况下返回错误字符串 —— 必须可见
      setError(String(e));
      onSendFailed();
      return false;
    } finally {
      setSending(false);
    }
  }, [sending, taskId, onSent, onSendFailed, onActivitySent, refreshDetail, rememberInput]);

  const executeSlash = useCallback(async (
    parsed: ParsedSlashCommand,
    mode: AgentSendMode,
  ) => {
    const command = parsed.command;
    if (!command) throw new Error(`未知命令 /${parsed.rawName}。输入 /help 查看可用命令。`);
    const unavailable = commandUnavailableReason(command, {
      location: "room",
      workspaceAttached,
      running,
    });
    if (unavailable) throw new Error(`/${command.name} ${unavailable}`);
    if (!parsed.args && command.argumentHint?.startsWith("<")) {
      throw new Error(`/${command.name} 需要参数：${command.argumentHint}`);
    }

    if (command.kind === "workflow") {
      await transmit(workflowPrompt(command, parsed.args), mode);
      return;
    }

    switch (command.name) {
      case "clear": {
        const next = await taskCreate(
          workspacePath,
          "新对话",
          "",
          task?.mode ?? (workspaceAttached ? "edit" : "ask"),
          providerName,
          agentEngine,
        );
        if (model) await taskSetModel(next.id, model);
        await refreshTasks();
        await refreshDetail(next.id);
        openRoom(next.id);
        return;
      }
      case "resume":
        setScene("conversations");
        return;
      case "compact": {
        const result = await taskCompactContext(taskId, parsed.args);
        if (result.compacted) {
          setNotice(`上下文已从 ${result.before_messages} 条压缩为 ${result.after_messages} 条；完整聊天记录仍可回看。`);
          onSendFailed();
        } else {
          setNotice(`当前只有 ${result.before_messages} 条上下文消息，暂时无需压缩。`);
        }
        await refreshDetail(taskId);
        return;
      }
      case "fork": {
        const branch = await taskForkContext(taskId);
        await refreshDetail(taskId);
        onSendFailed();
        setNotice(`已创建会话分支 ${branch.id.slice(0, 8)}；原分支与完整记录保持不变。`);
        return;
      }
      case "rename":
        await taskRename(taskId, parsed.args);
        await Promise.all([refreshTasks(), refreshDetail(taskId)]);
        setNotice(`会话已重命名为“${parsed.args.trim()}”。`);
        return;
      case "context": {
        const messages = await sessionMessages(taskId);
        const messageCount = messages.filter((item) => item.kind === "message").length;
        const activeAgents = (useTasksStore.getState().details[taskId]?.runs ?? [])
          .filter((run) => run.ended_at == null).length;
        setNotice(
          `${workspaceAttached ? `项目 ${workspaceName ?? "已附加"} · ${projectAccessModeLabel(workspaceAccessMode)}` : "纯聊天 · 未附加项目"}；` +
          `主 Agent ${agentEngine === "codex" ? "Codex CLI" : "R-Code"}；` +
          (agentEngine === "r_code" ? `模型 ${providerName ?? "默认服务"} / ${model ?? "服务默认"}；` : "模型使用 Codex CLI 设置；") +
          `${messageCount} 条消息 · ${activeAgents} 个运行中 Agent · ${queuedMessages.length} 条排队。`,
        );
        return;
      }
      case "usage": {
        const messages = await sessionMessages(taskId);
        const visibleMessages = messages.filter((item) => item.kind === "message" && item.text?.trim());
        const text = visibleMessages.map((item) => item.text ?? "").join("\n");
        let ascii = 0;
        let nonAscii = 0;
        for (const character of text) {
          if (character.charCodeAt(0) <= 0x7f) ascii += 1;
          else nonAscii += 1;
        }
        const estimatedTokens = Math.ceil(ascii / 4 + nonAscii * 1.1);
        const toolEvents = messages.filter((item) => item.kind === "tool_call").length;
        setNotice(
          `${visibleMessages.length} 条可见消息 · ${text.length.toLocaleString()} 字符 · 约 ${estimatedTokens.toLocaleString()} tokens · ${toolEvents} 次工具调用。` +
          "这是本地粗略估算，不等同于模型服务商账单。",
        );
        return;
      }
      case "copy": {
        const messages = await sessionMessages(taskId);
        const latest = [...messages].reverse().find(
          (item) => item.kind === "message" && item.role === "assistant" && item.text?.trim(),
        );
        if (!latest?.text) throw new Error("当前会话还没有可复制的 Agent 回复");
        await navigator.clipboard.writeText(latest.text);
        setNotice("已复制最近一条 Agent 回复。");
        return;
      }
      case "export": {
        const messages = await sessionMessages(taskId);
        const transcript = messages
          .filter((item) => item.kind === "message" && item.text?.trim())
          .map((item) => `${item.role === "user" ? "## You" : "## R-Code"}\n\n${item.text?.trim()}`)
          .join("\n\n");
        if (!transcript) throw new Error("当前会话还没有可导出的对话");
        await navigator.clipboard.writeText(`# ${task?.title ?? "R-Code 会话"}\n\n${transcript}\n`);
        setNotice("当前会话已按 Markdown 复制到剪贴板。");
        return;
      }
      case "stop":
        await onAbort();
        setNotice("已请求停止当前运行和它的子代理。");
        return;
      case "model":
        setModelMenuRequest((value) => value + 1);
        return;
      case "search":
        setSearchOpen(true);
        return;
      case "pending":
        setScene("inbox");
        return;
      case "activity":
        setScene("deck");
        return;
      case "projects":
        setCurrentProject(workspacePath);
        setScene("projects");
        return;
      case "permissions":
        setPermissionMenuRequest((value) => value + 1);
        return;
      case "agents":
        onShowSubagents();
        return;
      case "diff":
        setCanvasTab("changes");
        return;
      case "undo":
        setCanvasTab("changes");
        setNotice("已打开变更页。选择文件回滚或整任务回滚后，仍需再次确认。");
        return;
      case "files":
        setCanvasTab("files");
        return;
      case "terminal":
        setCanvasTab("terminal");
        return;
      case "review":
        if (parsed.args) {
          const reviewWorkflow = SLASH_COMMANDS.find((item) => item.name === "code-review");
          if (!reviewWorkflow) throw new Error("代码审查工作流不可用");
          await transmit(workflowPrompt(reviewWorkflow, parsed.args), mode);
        } else {
          setCanvasTab("review");
        }
        return;
      case "verify":
        setCanvasTab("review");
        if (parsed.args) {
          await runVerification(taskId, parsed.args);
          await refreshDetail(taskId);
          setNotice(`验证已完成：${parsed.args}`);
        }
        return;
      case "memory":
        setCurrentProject(workspacePath);
        setScene("knowledge");
        return;
      case "theme": {
        const requested = parsed.args.toLowerCase();
        const next = requested || (themeMode === "light" ? "dark" : themeMode === "dark" ? "system" : "light");
        if (next !== "light" && next !== "dark" && next !== "system") {
          throw new Error("主题只支持 light、dark 或 system");
        }
        setThemeMode(next);
        setNotice(`外观已切换为 ${next === "light" ? "亮色" : next === "dark" ? "暗色" : "跟随系统"}。`);
        return;
      }
      case "settings":
        setSettingsPane("providers");
        return;
      case "mcp":
        setSettingsPane("codex");
        return;
      case "skills":
        setNotice("内置工作流：/plan、/doctor、/debug、/fix、/explain、/init、/code-review、/security-review、/simplify、/docs、/research、/qa。输入 / 后可搜索并查看说明。");
        return;
      case "plugins":
        setSettingsPane("codex");
        return;
      case "help":
        setNotice("会话：/clear /resume /compact /fork /rename /context /usage /copy /export /stop；控制：/search /pending /activity /projects /model /permissions /agents /diff /undo /files /terminal /review /verify；输入 / 可搜索工作流和扩展。");
        return;
      default:
        throw new Error(`命令 /${command.name} 尚未接入当前页面`);
    }
  }, [
    model,
    onAbort,
    onSendFailed,
    onShowSubagents,
    openRoom,
    providerName,
    queuedMessages.length,
    refreshDetail,
    refreshTasks,
    running,
    setCanvasTab,
    setCurrentProject,
    setScene,
    setSearchOpen,
    setSettingsPane,
    setThemeMode,
    task?.mode,
    task?.title,
    taskId,
    themeMode,
    transmit,
    workspaceAccessMode,
    workspaceAttached,
    workspaceName,
    workspacePath,
  ]);

  const send = useCallback(async (mode: AgentSendMode = "auto") => {
    const draft = text;
    const msg = text.trim();
    if (attachmentBlockedReason) {
      setError(attachmentBlockedReason);
      return;
    }
    if ((!msg && sendableAttachments.length === 0) || sending || commandBusy) return;
    const parsed = msg ? parseSlashCommand(msg, workflowSkills) : null;
    // 提交动作在前端被接纳时就清空受控草稿，不能让 IPC/Agent 启动时延把旧文本
    // 留在输入框里；失败时仅在用户尚未开始新草稿的情况下恢复。
    setText("");
    setAt(null);
    setSlashDismissed(false);
    leaveInputHistory();
    if (!parsed) {
      const sent = await transmit(msg, mode, sendableAttachments);
      if (sent && attachments.attachments.length > 0) attachments.clear();
      if (!sent) setText((current) => current.length > 0 ? current : draft);
      return;
    }

    setCommandBusy(true);
    setError(null);
    setNotice(null);
    try {
      await executeSlash(parsed, mode);
    } catch (cause) {
      setError(String(cause));
      setText((current) => current.length > 0 ? current : draft);
    } finally {
      setCommandBusy(false);
    }
  }, [
    attachmentBlockedReason,
    attachments.attachments.length,
    attachments.clear,
    commandBusy,
    executeSlash,
    leaveInputHistory,
    sendableAttachments,
    sending,
    text,
    transmit,
    workflowSkills,
  ]);

  const removeQueued = useAsyncAction(async (queueId: string) => {
    await agentQueueRemove(taskId, queueId);
    await refreshDetail(taskId);
  }, { label: "移除队列消息" });

  const abort = useAsyncAction(onAbort, { label: "中断" });

  const pickSlash = useCallback((command: SlashCommandDefinition) => {
    const next = slashCommandInsertion(command);
    setText(next);
    setAt(null);
    setSlashDismissed(false);
    setSlashActive(0);
    requestAnimationFrame(() => {
      const textarea = taRef.current;
      if (!textarea) return;
      textarea.focus();
      textarea.setSelectionRange(next.length, next.length);
    });
  }, []);

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.nativeEvent.isComposing) return;
    if (slashOpen) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const delta = e.key === "ArrowDown" ? 1 : -1;
        setSlashActive((slashActive + delta + slashItems.length) % slashItems.length);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setSlashDismissed(true);
        return;
      }
      if (e.key === "Tab") {
        e.preventDefault();
        const command = slashItems[slashActive];
        const unavailable = commandUnavailableReason(command, slashContext);
        if (unavailable) setError(`/${command.name} ${unavailable}`);
        else pickSlash(command);
        return;
      }
      if (e.key === "Enter" && !e.shiftKey) {
        const query = slashSearchQuery(text);
        const command = slashItems[slashActive];
        const exact = query === command.name || command.aliases?.includes(query ?? "");
        if (!exact) {
          e.preventDefault();
          const unavailable = commandUnavailableReason(command, slashContext);
          if (unavailable) setError(`/${command.name} ${unavailable}`);
          else pickSlash(command);
          return;
        }
      }
    }
    if (at && !at.error && at.items.length > 0) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const d = e.key === "ArrowDown" ? 1 : -1;
        setAt({ ...at, active: (at.active + d + at.items.length) % at.items.length });
        return;
      }
      if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        pickAt(at.items[at.active]);
        return;
      }
      if (e.key === "Escape") {
        setAt(null);
        return;
      }
    }
    if ((e.key === "ArrowUp" || e.key === "ArrowDown") && !e.altKey && !e.ctrlKey && !e.metaKey) {
      const browsing = historyIndexRef.current != null;
      const selectionCollapsed = e.currentTarget.selectionStart === e.currentTarget.selectionEnd;
      const multiline = e.currentTarget.value.includes("\n");
      const atFirstCharacter = e.currentTarget.selectionStart === 0;
      const canStartBrowsing = e.key === "ArrowUp"
        && selectionCollapsed
        && (!multiline || atFirstCharacter);
      if (
        e.key === "ArrowUp"
        && canStartBrowsing
        && inputHistory.length === 0
        && historyLoadedTaskRef.current !== taskId
      ) {
        e.preventDefault();
        const draft = e.currentTarget.value;
        void loadInputHistory().then((history) => {
          if (history.length === 0) return;
          historyLoadedTaskRef.current = taskId;
          setInputHistory(history);
          const textarea = taRef.current;
          if (!textarea || textarea.value !== draft || historyIndexRef.current != null) return;
          historyDraftRef.current = draft;
          historyIndexRef.current = history.length - 1;
          showHistoryValue(history[history.length - 1]);
        }).catch(() => {
          // A failed enhancement read leaves the draft untouched and may be retried.
        });
        return;
      }
      if (inputHistory.length > 0 && (browsing || canStartBrowsing)) {
        e.preventDefault();
        if (!browsing) {
          historyDraftRef.current = text;
          historyIndexRef.current = inputHistory.length - 1;
        } else if (e.key === "ArrowUp") {
          historyIndexRef.current = Math.max(0, (historyIndexRef.current ?? 0) - 1);
        } else if ((historyIndexRef.current ?? 0) < inputHistory.length - 1) {
          historyIndexRef.current = (historyIndexRef.current ?? 0) + 1;
        } else {
          const draft = historyDraftRef.current;
          leaveInputHistory();
          showHistoryValue(draft);
          return;
        }
        showHistoryValue(inputHistory[historyIndexRef.current ?? 0]);
        return;
      }
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void send(running ? runningSendMode : "auto");
    }
  };

  return (
    <div className="composer">
      {error && (
        <StatusBar kind="error" compact onDismiss={() => setError(null)}>
          发送失败：{error}
        </StatusBar>
      )}
      {abort.error && (
        <StatusBar kind="error" compact onDismiss={abort.clearError}>
          {abort.error}
        </StatusBar>
      )}
      {notice && (
        <StatusBar kind="info" compact onDismiss={() => setNotice(null)}>
          {notice}
        </StatusBar>
      )}
      {slashOpen && (
        <SlashCommandMenu
          anchorRef={compBoxRef}
          value={text}
          context={slashContext}
          skills={workflowSkills}
          activeIndex={slashActive}
          onActiveIndexChange={setSlashActive}
          onPick={pickSlash}
          onDismiss={() => setSlashDismissed(true)}
        />
      )}
      {attachments.error && (
        <StatusBar kind="error" compact onDismiss={attachments.clearError}>{attachments.error}</StatusBar>
      )}
      {at && (
        <AnchoredSurface
          anchorRef={compBoxRef}
          className="at-menu popover popover--up"
          role="listbox"
          label="引用文件"
          placement="up"
          align="left"
          matchAnchorWidth
          onDismiss={() => setAt(null)}
        >
          {at.error ? (
            <div className="popover-empty">文件搜索失败：{at.error}</div>
          ) : at.items.length === 0 ? (
            <div className="popover-empty">无匹配文件</div>
          ) : (
            at.items.map((p, i) => (
              <button
                key={p}
                type="button"
                role="option"
                aria-selected={i === at.active}
                className={"at-item ring-inset" + (i === at.active ? " on" : "")}
                onMouseDown={(e) => {
                  e.preventDefault(); // 保持 textarea 焦点
                  pickAt(p);
                }}
                onMouseEnter={() => setAt({ ...at, active: i })}
              >
                {p}
              </button>
            ))
          )}
        </AnchoredSurface>
      )}
      <div className="comp-box" ref={compBoxRef}>
        <textarea
          ref={taRef}
          rows={2}
          value={text}
          aria-label="给 Agent 的消息"
          aria-controls={slashOpen ? "slash-command-menu" : undefined}
          aria-activedescendant={slashOpen ? `slash-command-option-${slashActive}` : undefined}
          placeholder={
            running
              ? "正在处理，可继续补充要求…"
              : "回复、提问或补充上下文…（输入 @ 引用文件）"
          }
          onChange={(e) => {
            leaveInputHistory();
            setText(e.target.value);
            setSlashActive(0);
            setSlashDismissed(false);
            if (e.target.value.startsWith("/")) setAt(null);
            else detectAt(e.target.value, e.target.selectionStart ?? e.target.value.length);
          }}
          onPaste={attachments.onPaste}
          onKeyDown={onKeyDown}
        />

        <AttachmentTray
          attachments={attachments.attachments}
          capabilityFor={capabilityForAttachment}
          blockedReason={runBlockedReason}
          onRemove={attachments.remove}
        />

        {/* 输入区脚下的控件：与新对话页同构的「模型」「权限」入口 */}
        <div className="comp-meta">
          <div className="comp-meta-context">
            <AttachmentButton onFiles={attachments.addFiles} disabled={sending || commandBusy} />
            <AgentEngineSwitcher
              taskId={taskId}
              value={agentEngine}
              workspaceAttached={workspaceAttached}
              running={running}
              onChanged={onProviderChanged}
            />
            {agentEngine === "r_code" ? (
              <ModelSwitcher
                taskId={taskId}
                providerName={providerName}
                model={model}
                inference={inference}
                choices={providerChoices}
                fallback={providerFallback}
                running={running}
                onChanged={onProviderChanged}
                variant="pill"
                openRequest={modelMenuRequest}
              />
            ) : (
              <CodexModelConfiguration
                running={running}
                openRequest={modelMenuRequest}
                preload
                onPreferencesChange={setCodexPreferences}
              />
            )}
            {workspaceAttached && (
              <ProjectAccessSelector
                value={workspaceAccessMode}
                workspaceName={workspaceName ?? "当前工作区"}
                placement="up"
                disabled={scopeBusy || running}
                onChange={onAccessModeChange}
                openRequest={permissionMenuRequest}
              />
            )}
          </div>
          <span className="spacer" />
          {running ? (
            <div className="running-send-actions" aria-label="运行中消息操作">
              <div className={`run-send-mode-control mode-${runningSendMode}`}>
                <button
                  className="run-send-mode-label run-send-primary"
                  type="button"
                  disabled={sending || commandBusy}
                  onClick={() => setRunningSendMode(nextRunningSendMode(runningSendMode))}
                  aria-label={
                    `当前发送方式：${runningSendModeLabel(runningSendMode)}。` +
                    `点击切换为${runningSendModeLabel(nextRunningSendMode(runningSendMode))}`
                  }
                  title={
                    `${runningSendModeTitle(runningSendMode)}；` +
                    `点击切换为${runningSendModeLabel(nextRunningSendMode(runningSendMode))}`
                  }
                >
                  <span className="run-send-mode-dot" aria-hidden="true" />
                  <span>{runningSendModeLabel(runningSendMode)}</span>
                  <IconRefresh className="run-send-mode-cycle" width={11} height={11} aria-hidden="true" />
                  <kbd className="sr-only">Enter</kbd>
                </button>
                <Menu
                  className="run-send-mode-menu-root"
                  label="选择发送方式"
                  placement="up"
                  align="right"
                  menuClassName="comp-more-menu"
                  trigger={
                    <button
                      className="run-send-mode-trigger"
                      type="button"
                      disabled={sending || commandBusy}
                      aria-label={`选择发送方式，当前为${runningSendModeLabel(runningSendMode)}`}
                      title="直接选择发送方式"
                    >
                      <IconChevronDown width={11} height={11} />
                    </button>
                  }
                >
                  {({ close }) => (
                    <>
                      <MenuItem
                        close={close}
                        checked={runningSendMode === "queue"}
                        hint="当前运行结束后发送，不打断这一轮"
                        onSelect={() => setRunningSendMode("queue")}
                      >
                        排队发送
                      </MenuItem>
                      <MenuItem
                        close={close}
                        checked={runningSendMode === "steer"}
                        hint="补充到当前运行，原任务与上下文保持不变"
                        onSelect={() => setRunningSendMode("steer")}
                      >
                        引导当前运行
                      </MenuItem>
                      <MenuItem
                        close={close}
                        checked={runningSendMode === "send_now"}
                        className="is-destructive"
                        hint="中断当前运行，优先处理这条消息"
                        onSelect={() => setRunningSendMode("send_now")}
                      >
                        立即发送
                      </MenuItem>
                    </>
                  )}
                </Menu>
              </div>
              <button
                className={`send running-send-button mode-${runningSendMode}`}
                type="button"
                disabled={!text.trim() || sending || commandBusy || Boolean(attachmentBlockedReason)}
                onClick={() => void send(runningSendMode)}
                aria-label={`${runningSendModeLabel(runningSendMode)}消息`}
                title={`${runningSendModeTitle(runningSendMode)}（Enter）`}
              >
                <IconSend width={15} height={15} />
                <span>{sending ? "发送中" : "发送"}</span>
              </button>
            </div>
          ) : (
            <button
              className="send"
              type="button"
              disabled={
                (!text.trim() && sendableAttachments.length === 0)
                || Boolean(attachmentBlockedReason)
                || sending
                || commandBusy
              }
              onClick={() => void send("auto")}
              aria-label="发送消息"
              title={attachmentBlockedReason ?? "发送（Enter）"}
            >
              <IconSend width={15} height={15} />
              <span>{sending ? "发送中" : "发送"}</span>
            </button>
          )}
        </div>

        {running && (
          <div className="run-command-bar" aria-label="队列与运行控制">
            <Menu
              role="dialog"
              label="待发送队列"
              placement="up"
              align="left"
              menuClassName="queue-popover"
              trigger={
                <button className="run-queue-summary" type="button">
                  队列 <strong>{queuedMessages.length}</strong>
                </button>
              }
            >
              <div className="queue-popover-head">
                <span>待发送队列</span>
                <span>{queuedMessages.length} 条</span>
              </div>
              {queuedMessages.length === 0 ? (
                <p className="queue-empty">没有待发送的消息</p>
              ) : (
                <div className="queue-list">
                  {queuedMessages.map((item) => (
                    <div className="queue-item" key={item.id}>
                      <span title={item.message}>{item.message}</span>
                      <span className={"queue-state " + item.state}>{queueStateLabel(item.state)}</span>
                      <button
                        type="button"
                        className="iconbtn"
                        disabled={removeQueued.busy}
                        onClick={() => void removeQueued.run(item.id)}
                        aria-label={`移除队列消息：${item.message}`}
                      >
                        移除
                      </button>
                    </div>
                  ))}
                </div>
              )}
            </Menu>

            <span className="run-command-spacer" />

            <button
              className="run-command-stop is-destructive"
              type="button"
              disabled={abort.busy}
              onClick={() => void abort.run()}
              aria-label={abort.busy ? "正在中断当前运行" : "中断当前运行"}
              title="中断当前运行"
            >
              <IconStop width={11} height={11} />
              <span>{abort.busy ? "中断中" : "中断"}</span>
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

function queueStateLabel(state: QueuedMessage["state"]): string {
  switch (state) {
    case "queued":
      return "等待发送";
    case "dispatching":
      return "正在发送";
    case "failed":
      return "发送失败";
    case "sent":
      return "已发送";
    case "cancelled":
      return "已取消";
  }
}
