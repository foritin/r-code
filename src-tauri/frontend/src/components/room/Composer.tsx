/**
 * Room 输入区 —— Enter 按当前选择发送 / Shift+Enter 换行；运行中可选择排队、引导或立即发送。
 * `@` 触发 quickOpen 文件下拉，选中后插入 @path 文本。
 *
 * 输入区脚下现在镜像了「模型」与「权限」两个控件（与新对话页同构）。原先这里
 * 只有一个只读的模型状态芯片，想换模型或改权限必须回到会话顶栏找。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { flushSync } from "react-dom";
import {
  agentQueueRemove,
  agentQueueReorder,
  agentQueueSteer,
  agentQueueUpdate,
  agentSend,
  quickOpen,
  runVerification,
  sessionMessages,
  taskCompactContext,
  taskCreate,
  taskForkContext,
  taskRename,
  taskSetModel,
  taskUpdateGoal,
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
import { AnchoredSurface } from "../ui/AnchoredSurface";
import { StatusBar } from "../ui/StatusBar";
import { ProjectAccessSelector, projectAccessModeLabel } from "../ProjectAccessSelector";
import { ModelSwitcher } from "./ModelSwitcher";
import { AgentEngineSwitcher } from "./AgentEngineSwitcher";
import { CodexModelConfiguration } from "./CodexModelConfiguration";
import {
  IconDragHandle,
  IconEdit,
  IconMore,
  IconSend,
  IconSteer,
  IconStop,
  IconTrash,
} from "../icons";
import { Menu, MenuItem } from "../ui/Menu";
import {
  AttachmentTray,
  firstBlockedAttachmentReason,
  sendableAttachmentInputs,
  useAttachments,
  type DraftAttachment,
} from "../Attachments";
import { ActiveGoalBar, GoalModeChip, TaskAddMenu } from "../TaskAddMenu";
import {
  AgentSendModeControl,
  agentSendModeLabel,
  agentSendModeTitle,
  effectiveAgentSendMode,
  useAgentSendModePreference,
} from "../AgentSendModeControl";
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

type QueueDropEdge = "before" | "after";

function moveQueueItem(
  messages: QueuedMessage[],
  sourceId: string,
  targetId: string,
  edge: QueueDropEdge,
): QueuedMessage[] {
  if (sourceId === targetId) return messages;
  const source = messages.find((item) => item.id === sourceId);
  if (!source) return messages;
  const next = messages.filter((item) => item.id !== sourceId);
  const targetIndex = next.findIndex((item) => item.id === targetId);
  if (targetIndex < 0) return messages;
  next.splice(targetIndex + (edge === "after" ? 1 : 0), 0, source);
  return next;
}

function sameQueueOrder(left: QueuedMessage[], right: QueuedMessage[]): boolean {
  return left.length === right.length && left.every((item, index) => item.id === right[index]?.id);
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
  const [goalSaveError, setGoalSaveError] = useState<string | null>(null);
  const [goalMode, setGoalMode] = useState(false);
  const [goalSaving, setGoalSaving] = useState(false);
  const [goalDeleting, setGoalDeleting] = useState(false);
  const [sending, setSending] = useState(false);
  const [at, setAt] = useState<AtState | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [commandBusy, setCommandBusy] = useState(false);
  const [slashActive, setSlashActive] = useState(0);
  const [slashDismissed, setSlashDismissed] = useState(false);
  const [modelMenuRequest, setModelMenuRequest] = useState(0);
  const [permissionMenuRequest, setPermissionMenuRequest] = useState(0);
  const [codexPreferences, setCodexPreferences] = useState<CodexCliPreferences | null>(null);
  const [sendMode, setSendMode] = useAgentSendModePreference();
  const [inputHistory, setInputHistory] = useState<string[]>([]);
  const [workflowSkills, setWorkflowSkills] = useState<WorkflowSkill[]>([]);
  const [queueView, setQueueView] = useState<QueuedMessage[]>(queuedMessages);
  const [draggedQueueId, setDraggedQueueId] = useState<string | null>(null);
  const [queueDropTarget, setQueueDropTarget] = useState<{ id: string; edge: QueueDropEdge } | null>(null);
  const [queueAnnouncement, setQueueAnnouncement] = useState("");
  const [queueActionError, setQueueActionError] = useState<string | null>(null);
  const [queueActionBusyId, setQueueActionBusyId] = useState<string | null>(null);
  const [editingQueueId, setEditingQueueId] = useState<string | null>(null);
  const [queueEditText, setQueueEditText] = useState("");
  const taRef = useRef<HTMLTextAreaElement>(null);
  const compBoxRef = useRef<HTMLDivElement>(null);
  const debRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const historyIndexRef = useRef<number | null>(null);
  const historyDraftRef = useRef("");
  const historyLoadedTaskRef = useRef<string | null>(null);
  const historyRequestRef = useRef<{ taskId: string; promise: Promise<string[]> } | null>(null);
  const initializedTaskRef = useRef<string | null>(null);
  const consumedFileReferencesRef = useRef(new Set<string>());
  const messageDraftBeforeGoalRef = useRef("");

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
  const runBlockedReason = running && !goalMode && attachments.attachments.length > 0
    ? "当前运行结束后才能把附件作为新一轮消息发送。"
    : null;
  const attachmentBlockedReason = runBlockedReason ?? capabilityBlockedReason;

  const slashContext = {
    location: "room" as const,
    workspaceAttached,
    running,
  };
  const slashItems = goalMode || slashDismissed ? [] : matchingSlashCommands(text, slashContext, workflowSkills);
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
    setGoalSaveError(null);
    setGoalMode(false);
    setGoalSaving(false);
    setGoalDeleting(false);
    setNotice(null);
    setQueueActionError(null);
    setQueueActionBusyId(null);
    setEditingQueueId(null);
    setQueueEditText("");
    setAt(null);
    setSlashActive(0);
    setSlashDismissed(false);
    attachments.clear();
  }, [taskId, attachments.clear]);

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

  const enterGoalMode = useCallback(() => {
    if (goalMode) return;
    messageDraftBeforeGoalRef.current = text;
    setText(task?.goal_active ? task.goal : "");
    setGoalMode(true);
    setGoalSaveError(null);
    setAt(null);
    setSlashDismissed(true);
    leaveInputHistory();
    requestAnimationFrame(() => taRef.current?.focus());
  }, [goalMode, leaveInputHistory, task?.goal, task?.goal_active, text]);

  const exitGoalMode = useCallback(() => {
    setText(messageDraftBeforeGoalRef.current);
    setGoalMode(false);
    setGoalSaveError(null);
    setAt(null);
    setSlashDismissed(false);
    requestAnimationFrame(() => taRef.current?.focus());
  }, []);

  const saveTaskGoal = useCallback(async () => {
    const draft = text;
    const normalized = text.trim();
    const updatingExistingGoal = task?.goal_active === true;
    if (goalSaving || sending || commandBusy || !normalized) return;
    if (capabilityBlockedReason) {
      setGoalSaveError(capabilityBlockedReason);
      return;
    }

    setGoalSaving(true);
    setGoalSaveError(null);
    setNotice(null);
    setText("");
    setAt(null);
    setGoalMode(false);
    leaveInputHistory();
    try {
      await taskUpdateGoal(taskId, normalized);
      // Editing an active Goal replaces its objective. A normal steer is explicitly additive in
      // the runtime, so use send_now to stop the old run and start the updated Goal immediately.
      const sent = await transmit(
        normalized,
        running ? "send_now" : "auto",
        sendableAttachments,
      );
      if (!sent) {
        setGoalSaveError("目标已经更新，但还没有成功交给 Agent；请重试执行。");
        setText(normalized);
        setGoalMode(true);
        return;
      }
      if (attachments.attachments.length > 0) attachments.clear();
      setText(messageDraftBeforeGoalRef.current);
      await refreshDetail(taskId);
      setNotice(updatingExistingGoal ? "目标已更新，Agent 正按新目标执行。" : "目标已设置，Agent 已开始执行。");
    } catch (cause) {
      setGoalSaveError(String(cause).replace(/^Error:\s*/i, ""));
      setText((current) => current.length > 0 ? current : draft);
      setGoalMode(true);
    } finally {
      setGoalSaving(false);
      requestAnimationFrame(() => taRef.current?.focus());
    }
  }, [
    attachments.attachments.length,
    attachments.clear,
    capabilityBlockedReason,
    commandBusy,
    goalSaving,
    leaveInputHistory,
    refreshDetail,
    running,
    sendableAttachments,
    sending,
    task?.goal,
    task?.goal_active,
    taskId,
    text,
    transmit,
  ]);

  const deleteTaskGoal = useCallback(async () => {
    if (goalDeleting || goalSaving || sending || !task?.goal_active || !task.goal.trim()) return;
    setGoalDeleting(true);
    setGoalSaveError(null);
    setNotice(null);
    try {
      if (running) await onAbort();
      await taskUpdateGoal(taskId, "");
      await refreshDetail(taskId);
      if (goalMode) setText(messageDraftBeforeGoalRef.current);
      setGoalMode(false);
    } catch (cause) {
      setGoalSaveError(String(cause).replace(/^Error:\s*/i, ""));
    } finally {
      setGoalDeleting(false);
    }
  }, [goalDeleting, goalMode, goalSaving, onAbort, refreshDetail, running, sending, task?.goal, task?.goal_active, taskId]);

  const resumeTaskGoal = useCallback(async () => {
    const currentGoal = task?.goal_active ? task.goal.trim() : "";
    if (!currentGoal || sending || commandBusy) return;
    const sent = await transmit(currentGoal, "auto");
    if (sent) setNotice("已继续执行目标。");
  }, [commandBusy, sending, task?.goal, task?.goal_active, transmit]);

  const send = useCallback(async (mode: AgentSendMode = "auto") => {
    if (goalMode) {
      await saveTaskGoal();
      return;
    }
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
    goalMode,
    leaveInputHistory,
    saveTaskGoal,
    sendableAttachments,
    sending,
    text,
    transmit,
    workflowSkills,
  ]);

  const removeQueued = useAsyncAction(async (queueId: string) => {
    await agentQueueRemove(taskId, queueId);
    setQueueView((current) => current.filter((item) => item.id !== queueId));
    try {
      await refreshDetail(taskId);
    } catch {
      // The durable mutation succeeded; polling will reconcile a transient refresh failure.
    }
    setQueueAnnouncement("已删除一条排队消息");
  }, {
    label: "移除队列消息",
    onError: () => setQueueActionError("暂时无法删除这条消息，请稍后重试。"),
  });

  const reorderQueued = useAsyncAction(async (queueIds: string[]) => {
    await agentQueueReorder(taskId, queueIds);
    try {
      await refreshDetail(taskId);
    } catch {
      // Keep the already persisted optimistic order until the next detail poll.
    }
  }, {
    label: "调整队列顺序",
    onError: () => {
      setQueueView(queuedMessages);
      setQueueActionError("队列刚刚发生了变化，已恢复最新顺序，请再试一次。");
    },
  });

  useEffect(() => {
    if (draggedQueueId || reorderQueued.busy) return;
    setQueueView(queuedMessages);
  }, [draggedQueueId, queuedMessages, reorderQueued.busy, taskId]);

  useEffect(() => {
    if (!editingQueueId) return;
    if (!queuedMessages.some((item) => item.id === editingQueueId)) {
      setEditingQueueId(null);
      setQueueEditText("");
    }
  }, [editingQueueId, queuedMessages]);

  const startQueueEdit = (item: QueuedMessage) => {
    if (item.state !== "queued" && item.state !== "failed") return;
    setQueueActionError(null);
    setEditingQueueId(item.id);
    setQueueEditText(item.message);
  };

  const cancelQueueEdit = () => {
    setEditingQueueId(null);
    setQueueEditText("");
  };

  const saveQueueEdit = async (item: QueuedMessage) => {
    const message = queueEditText.trim();
    if (!message || queueActionBusyId) return;
    setQueueActionError(null);
    setQueueActionBusyId(item.id);
    try {
      await agentQueueUpdate(taskId, item.id, message);
      setQueueView((current) => current.map((queued) => queued.id === item.id
        ? { ...queued, message, state: "queued" }
        : queued));
      try {
        await refreshDetail(taskId);
      } catch {
        // The edit is durable; the next poll can refresh timestamps and state.
      }
      setEditingQueueId(null);
      setQueueEditText("");
      setQueueAnnouncement("队列消息已更新");
    } catch {
      setQueueActionError("暂时无法保存这条消息，请稍后重试。");
    } finally {
      setQueueActionBusyId(null);
    }
  };

  const steerQueued = async (item: QueuedMessage) => {
    if (!running || item.state !== "queued" || queueActionBusyId) return;
    setQueueActionError(null);
    setQueueActionBusyId(item.id);
    try {
      const outcome = await agentQueueSteer(taskId, item.id);
      setQueueView((current) => {
        if (outcome === "steered" || outcome === "started") {
          return current.filter((queued) => queued.id !== item.id);
        }
        return [
          { ...item, state: "queued" },
          ...current.filter((queued) => queued.id !== item.id),
        ];
      });
      try {
        await refreshDetail(taskId);
      } catch {
        // The selected action already succeeded; polling will reconcile the projection.
      }
      if (outcome === "steered") {
        setQueueAnnouncement("消息已引导进当前运行");
      } else if (outcome === "started") {
        setQueueAnnouncement("当前运行已经结束，消息已开始执行");
      } else {
        setQueueAnnouncement("当前运行暂不可介入，消息已移到队首");
      }
    } catch {
      setQueueActionError("暂时无法引导这条消息；它仍保留在队列中。");
      try {
        await refreshDetail(taskId);
      } catch {
        // Preserve the last visible queue when even the refresh path is unavailable.
      }
    } finally {
      setQueueActionBusyId(null);
    }
  };

  const toggleQueueing = () => {
    if (sendMode === "queue") {
      setSendMode("steer");
      setQueueAnnouncement("已关闭后续排队；现有队列保持不变");
    } else {
      setSendMode("queue");
      setQueueAnnouncement("后续消息将加入队列");
    }
  };

  const persistQueueOrder = (next: QueuedMessage[]) => {
    if (sameQueueOrder(queueView, next)) return;
    setQueueView(next);
    const pending = next.filter((item) => item.state === "queued");
    setQueueAnnouncement("队列顺序已调整；越靠上越先执行");
    void reorderQueued.run(pending.map((item) => item.id));
  };

  const dropQueueItem = (
    event: React.DragEvent<HTMLLIElement>,
    targetId: string,
  ) => {
    event.preventDefault();
    const sourceId = draggedQueueId || event.dataTransfer.getData("text/plain");
    const target = queueView.find((item) => item.id === targetId);
    if (!sourceId || target?.state !== "queued" || reorderQueued.busy) return;
    const bounds = event.currentTarget.getBoundingClientRect();
    const edge: QueueDropEdge = event.clientY < bounds.top + bounds.height / 2 ? "before" : "after";
    const next = moveQueueItem(queueView, sourceId, targetId, edge);
    setDraggedQueueId(null);
    setQueueDropTarget(null);
    persistQueueOrder(next);
  };

  const moveQueueItemFromKeyboard = (queueId: string, key: string) => {
    const pending = queueView.filter((item) => item.state === "queued");
    const index = pending.findIndex((item) => item.id === queueId);
    if (index < 0) return;
    let targetIndex = index;
    let edge: QueueDropEdge = "before";
    if (key === "ArrowUp") targetIndex = Math.max(0, index - 1);
    else if (key === "ArrowDown") {
      targetIndex = Math.min(pending.length - 1, index + 1);
      edge = "after";
    } else if (key === "Home") targetIndex = 0;
    else if (key === "End") {
      targetIndex = pending.length - 1;
      edge = "after";
    } else return;
    if (targetIndex === index) return;
    persistQueueOrder(moveQueueItem(queueView, queueId, pending[targetIndex].id, edge));
  };

  const abort = useAsyncAction(onAbort, { label: "停止" });

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
    if (goalMode) {
      if (e.key === "Escape") {
        e.preventDefault();
        exitGoalMode();
        return;
      }
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        void saveTaskGoal();
      }
      return;
    }
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
      void send(effectiveAgentSendMode(sendMode, running));
    }
  };

  const queuedPositionById = useMemo(() => {
    const positions = new Map<string, number>();
    let position = 0;
    for (const item of queueView) {
      if (item.state !== "queued") continue;
      position += 1;
      positions.set(item.id, position);
    }
    return positions;
  }, [queueView]);
  const reorderableQueueCount = queuedPositionById.size;
  const queueBusy = removeQueued.busy || reorderQueued.busy || queueActionBusyId != null;

  return (
    <div className="composer">
      {error && (
        <StatusBar kind="error" compact onDismiss={() => setError(null)}>
          发送失败：{error}
        </StatusBar>
      )}
      {goalSaveError && (
        <StatusBar kind="error" compact onDismiss={() => setGoalSaveError(null)}>
          目标操作失败：{goalSaveError}
        </StatusBar>
      )}
      {abort.error && (
        <StatusBar kind="error" compact onDismiss={abort.clearError}>
          {abort.error}
        </StatusBar>
      )}
      {queueActionError && (
        <StatusBar kind="error" compact onDismiss={() => setQueueActionError(null)}>
          {queueActionError}
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
      {queueView.length > 0 && (
        <section className="composer-queue-stack" aria-label="待发送队列，越靠上越先执行">
          <p id={`queue-order-help-${taskId}`} className="sr-only">
            拖动排序手柄调整执行顺序；也可以聚焦手柄后使用上下方向键，越靠上越先执行。
          </p>
          <ol className="composer-queue-list">
            {queueView.map((item) => {
              const pendingPosition = queuedPositionById.get(item.id) ?? 0;
              const editing = editingQueueId === item.id;
              const itemBusy = queueActionBusyId === item.id;
              const canEdit = item.state === "queued" || item.state === "failed";
              const canSteer = running && item.state === "queued";
              const canReorder = item.state === "queued"
                && reorderableQueueCount > 1
                && !editing;
              const dropClass = queueDropTarget?.id === item.id
                ? ` is-drop-${queueDropTarget.edge}`
                : "";
              const steerTitle = running
                ? "在模型的下一个可介入点引导当前运行"
                : "当前没有可引导的运行";
              return (
                <li
                  className={`composer-queue-row${editing ? " is-editing" : ""}${draggedQueueId === item.id ? " is-dragging" : ""}${dropClass}`}
                  data-queue-id={item.id}
                  key={item.id}
                  onDragOver={(event) => {
                    if (!canReorder || !draggedQueueId || draggedQueueId === item.id || queueBusy) return;
                    event.preventDefault();
                    event.dataTransfer.dropEffect = "move";
                    const bounds = event.currentTarget.getBoundingClientRect();
                    const edge: QueueDropEdge = event.clientY < bounds.top + bounds.height / 2 ? "before" : "after";
                    setQueueDropTarget((current) => current?.id === item.id && current.edge === edge
                      ? current
                      : { id: item.id, edge });
                  }}
                  onDrop={(event) => dropQueueItem(event, item.id)}
                >
                  <span className="queue-row-leading">
                    <span className="queue-kind-icon" aria-hidden="true">
                      <IconSteer width={15} height={15} />
                    </span>
                    {canReorder && (
                      <button
                        type="button"
                        className="queue-reorder-handle"
                        draggable={!queueBusy}
                        disabled={queueBusy}
                        aria-label={`调整队列顺序：${item.message}，当前第 ${pendingPosition} 条，共 ${reorderableQueueCount} 条`}
                        aria-describedby={`queue-order-help-${taskId}`}
                        title="拖动排序；也可使用 ↑ ↓ Home End"
                        onDragStart={(event) => {
                          if (queueBusy) {
                            event.preventDefault();
                            return;
                          }
                          event.dataTransfer.effectAllowed = "move";
                          event.dataTransfer.setData("text/plain", item.id);
                          setDraggedQueueId(item.id);
                          setQueueDropTarget(null);
                        }}
                        onDragEnd={() => {
                          setDraggedQueueId(null);
                          setQueueDropTarget(null);
                        }}
                        onKeyDown={(event) => {
                          if (!["ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) return;
                          event.preventDefault();
                          if (!queueBusy) {
                            moveQueueItemFromKeyboard(item.id, event.key);
                          }
                        }}
                      >
                        <IconDragHandle width={16} height={16} />
                      </button>
                    )}
                  </span>
                  {editing ? (
                    <textarea
                      className="queue-edit-input"
                      value={queueEditText}
                      rows={1}
                      autoFocus
                      disabled={itemBusy}
                      aria-label="编辑队列消息"
                      onChange={(event) => setQueueEditText(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key === "Escape") {
                          event.preventDefault();
                          cancelQueueEdit();
                        } else if (event.key === "Enter" && !event.shiftKey) {
                          event.preventDefault();
                          void saveQueueEdit(item);
                        }
                      }}
                    />
                  ) : (
                    <span className="queue-message" title={item.message}>{item.message}</span>
                  )}
                  {!editing && (
                    <span className={`queue-state ${item.state}`}>
                      {item.state === "queued" ? "" : queueStateLabel(item.state)}
                    </span>
                  )}
                  <div className="queue-row-actions">
                    {editing ? (
                      <>
                        <button
                          type="button"
                          className="queue-edit-action"
                          disabled={itemBusy}
                          onClick={cancelQueueEdit}
                        >
                          取消
                        </button>
                        <button
                          type="button"
                          className="queue-edit-action primary"
                          disabled={itemBusy || !queueEditText.trim()}
                          onClick={() => void saveQueueEdit(item)}
                        >
                          {itemBusy ? "保存中…" : "保存"}
                        </button>
                      </>
                    ) : (
                      <>
                        <button
                          type="button"
                          className={`queue-steer-button${itemBusy ? " is-loading" : ""}`}
                          disabled={!canSteer || queueBusy}
                          onClick={() => void steerQueued(item)}
                          aria-label={`引导当前运行：${item.message}`}
                          title={steerTitle}
                        >
                          <IconSteer width={15} height={15} />
                          <span>引导</span>
                        </button>
                        <button
                          type="button"
                          className="queue-remove-button"
                          disabled={queueBusy || item.state === "dispatching"}
                          onClick={() => void removeQueued.run(item.id)}
                          aria-label={`删除队列消息：${item.message}`}
                          title="删除这条队列消息"
                        >
                          <IconTrash width={15} height={15} />
                        </button>
                        <Menu
                          className="queue-actions-menu-root"
                          label="队列消息操作"
                          placement="up"
                          align="right"
                          gap={6}
                          disabled={queueBusy}
                          menuClassName="queue-actions-popover"
                          trigger={
                            <button
                              type="button"
                              className="queue-more-button"
                              aria-label={`更多队列操作：${item.message}`}
                              title="更多操作"
                            >
                              <IconMore width={16} height={16} />
                            </button>
                          }
                        >
                          {({ close }) => (
                            <>
                              <MenuItem
                                close={close}
                                disabled={!canEdit}
                                onSelect={() => startQueueEdit(item)}
                              >
                                <IconEdit width={15} height={15} />
                                编辑消息
                              </MenuItem>
                              <MenuItem close={close} onSelect={toggleQueueing}>
                                <IconSteer width={15} height={15} />
                                {sendMode === "queue" ? "关闭排队" : "启用排队"}
                              </MenuItem>
                            </>
                          )}
                        </Menu>
                      </>
                    )}
                  </div>
                </li>
              );
            })}
          </ol>
          <span className="sr-only" aria-live="polite">{queueAnnouncement}</span>
        </section>
      )}
      <div className="comp-box" ref={compBoxRef}>
        {task?.goal_active && task.goal.trim() && (
          <ActiveGoalBar
            goal={task.goal.trim()}
            running={running}
            stopped={task.state === "interrupted"}
            busy={goalSaving || goalDeleting || sending || abort.busy}
            onEdit={enterGoalMode}
            onStop={() => void abort.run()}
            onResume={() => void resumeTaskGoal()}
            onDelete={() => void deleteTaskGoal()}
          />
        )}
        <textarea
          ref={taRef}
          rows={2}
          value={text}
          aria-label={goalMode ? "任务目标" : "给 Agent 的消息"}
          aria-controls={slashOpen ? "slash-command-menu" : undefined}
          aria-activedescendant={slashOpen ? `slash-command-option-${slashActive}` : undefined}
          placeholder={
            goalMode
              ? task?.goal_active
                ? "修改目标；发送后 Agent 会立即按新目标执行…"
                : "描述目标；发送后 Agent 会立即开始执行…"
              : running
                ? "正在处理，可继续补充要求…"
                : "回复、提问或补充上下文…（输入 @ 引用文件）"
          }
          onChange={(e) => {
            leaveInputHistory();
            setText(e.target.value);
            setSlashActive(0);
            setGoalSaveError(null);
            setSlashDismissed(goalMode);
            if (goalMode) {
              setAt(null);
              return;
            }
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
            <TaskAddMenu
              onFiles={attachments.addFiles}
              disabled={sending || commandBusy || goalSaving}
              running={running}
              task={task}
              agentEngine={agentEngine}
              goalMode={goalMode}
              onGoalModeChange={(active) => {
                if (active) enterGoalMode();
                else exitGoalMode();
              }}
              onTaskChanged={() => refreshDetail(taskId)}
              onError={setError}
            />
            {goalMode && (
              <GoalModeChip
                disabled={goalSaving}
                onExit={exitGoalMode}
              />
            )}
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
            <ProjectAccessSelector
              value={workspaceAccessMode}
              workspaceName={workspaceName ?? "未附加工作区"}
              placement="up"
              disabled={scopeBusy}
              unavailableReason={workspaceAttached
                ? undefined
                : "先通过“+”附加文件夹，才能设置 Agent 的本地工具权限。"}
              changeNotice={running
                ? "当前运行继续使用启动时的权限；新设置从下一轮开始生效。"
                : undefined}
              onChange={onAccessModeChange}
              openRequest={permissionMenuRequest}
            />
          </div>
          <span className="spacer" />
          {goalMode ? (
            <button
              className={`send composer-primary-button${goalSaving || sending ? " is-loading" : ""}`}
              type="button"
              disabled={goalSaving || sending || commandBusy || !text.trim()}
              onClick={() => void saveTaskGoal()}
              aria-label={task?.goal_active ? "更新并执行目标" : "执行目标"}
              aria-busy={goalSaving || sending || undefined}
              title={task?.goal_active ? "更新并执行目标（Enter）" : "执行目标（Enter）"}
            >
              {goalSaving || sending
                ? <span className="send-loading-spinner" aria-hidden="true" />
                : <IconSend width={15} height={15} />}
              <span className="sr-only">{goalSaving || sending ? "执行中" : task?.goal_active ? "更新并执行目标" : "执行目标"}</span>
            </button>
          ) : (
            <div className="running-send-actions" aria-label={running ? "运行中消息操作" : "消息发送操作"}>
              <AgentSendModeControl
                mode={sendMode}
                running={running}
                disabled={sending || commandBusy}
                onChange={setSendMode}
              />
              {sending ? (
                <button
                  className="send composer-primary-button running-send-button is-loading"
                  type="button"
                  disabled
                  aria-label="正在发送消息"
                  aria-busy="true"
                  title="正在发送消息"
                >
                  <span className="send-loading-spinner" aria-hidden="true" />
                  <span className="sr-only">发送中</span>
                </button>
              ) : running && !text.trim() && sendableAttachments.length === 0 ? (
                <button
                  className={`send composer-primary-button composer-stop-button${abort.busy ? " is-loading" : ""}`}
                  type="button"
                  disabled={abort.busy || commandBusy}
                  onClick={() => void abort.run()}
                  aria-label={abort.busy ? "正在停止当前运行" : "停止当前运行"}
                  aria-busy={abort.busy || undefined}
                  title={abort.busy ? "正在停止当前运行" : "停止当前运行"}
                >
                  {abort.busy
                    ? <span className="send-loading-spinner" aria-hidden="true" />
                    : <IconStop width={22} height={22} />}
                  <span className="sr-only">{abort.busy ? "停止中" : "停止"}</span>
                </button>
              ) : (
                <button
                  className={`send composer-primary-button running-send-button mode-${sendMode}`}
                  type="button"
                  disabled={
                    (!text.trim() && sendableAttachments.length === 0)
                    || Boolean(attachmentBlockedReason)
                    || commandBusy
                  }
                  onClick={() => void send(effectiveAgentSendMode(sendMode, running))}
                  aria-label={running ? `${agentSendModeLabel(sendMode)}消息` : "发送消息"}
                  title={attachmentBlockedReason ?? `${agentSendModeTitle(sendMode, running)}（Enter）`}
                >
                  <IconSend width={15} height={15} />
                  <span className="sr-only">发送</span>
                </button>
              )}
            </div>
          )}
        </div>

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
