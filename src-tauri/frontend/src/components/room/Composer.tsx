/**
 * Room 输入区 —— Enter 发送 / Shift+Enter 换行；运行中以引导为主动作。
 * `@` 触发 quickOpen 文件下拉，选中后插入 @path 文本。
 *
 * 输入区脚下现在镜像了「模型」与「权限」两个控件（与新对话页同构）。原先这里
 * 只有一个只读的模型状态芯片，想换模型或改权限必须回到会话顶栏找。
 */
import { useCallback, useEffect, useRef, useState } from "react";
import {
  agentDelegateCodex,
  agentDelegateCodexMcp,
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
} from "../../lib/ipc";
import type { AgentSendMode, ProjectAccessMode, QueuedMessage } from "../../lib/types";
import type { ProviderChoice } from "../../lib/provider";
import { useArmedAction, useAsyncAction } from "../../lib/hooks";
import { useTasksStore } from "../../store/tasks";
import { useAppStore } from "../../store/app";
import { Menu, MenuItem } from "../ui/Menu";
import { StatusBar } from "../ui/StatusBar";
import { ProjectAccessSelector, projectAccessModeLabel } from "../ProjectAccessSelector";
import { ModelSwitcher } from "./ModelSwitcher";
import { IconChevronDown, IconSend, IconStop } from "../icons";
import { useCodexCliGate } from "../codex/CodexCliGate";
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

interface Props {
  taskId: string;
  workspacePath: string | null;
  workspaceAttached: boolean;
  workspaceName: string | null;
  workspaceAccessMode: ProjectAccessMode;
  onAccessModeChange: (mode: ProjectAccessMode) => Promise<void> | void;
  scopeBusy: boolean;
  providerName: string | null;
  model: string | null;
  providerChoices: ProviderChoice[];
  providerFallback: string;
  onProviderChanged: () => void;
  running: boolean;
  queuedMessages: QueuedMessage[];
  onAbort: () => Promise<void>;
  onSent: (text: string, mode: AgentSendMode) => void;
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

export function Composer({
  taskId,
  workspacePath,
  workspaceAttached,
  workspaceName,
  workspaceAccessMode,
  onAccessModeChange,
  scopeBusy,
  providerName,
  model,
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
  const taRef = useRef<HTMLTextAreaElement>(null);
  const debRef = useRef<ReturnType<typeof setTimeout> | null>(null);

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
  const { runWithCodexCli } = useCodexCliGate();

  const slashContext = {
    location: "room" as const,
    workspaceAttached,
    running,
  };
  const slashItems = slashDismissed ? [] : matchingSlashCommands(text, slashContext);
  const slashOpen = slashItems.length > 0;

  useEffect(() => () => {
    if (debRef.current) window.clearTimeout(debRef.current);
  }, []);

  useEffect(() => {
    setText("");
    setError(null);
    setNotice(null);
    setAt(null);
    setSlashActive(0);
    setSlashDismissed(false);
  }, [taskId]);

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

  const transmit = useCallback(async (message: string, mode: AgentSendMode) => {
    if (!message.trim() || sending) return false;
    setSending(true);
    setError(null);
    setNotice(null);
    try {
      // 不等待 IPC 往返：运行中的引导、排队和立即发送都应立即可见。
      onSent(message, mode);
      await agentSend(taskId, message, mode);
      // IPC 成功后才把引导标为"已接纳"，失败时由下方 catch 回滚时间线。
      onActivitySent(mode);
      setText("");
      setAt(null);
      setSlashDismissed(false);
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
  }, [sending, taskId, onSent, onSendFailed, onActivitySent, refreshDetail]);

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
          `模型 ${providerName ?? "默认服务"} / ${model ?? "服务默认"}；` +
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
        setScene("projects");
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
      case "codex":
        await runWithCodexCli({ feature: "Codex 快速子代理", requireAuth: true }, async () => {
          await agentDelegateCodex(taskId, parsed.args, "只读调查");
          await refreshDetail(taskId);
          onShowSubagents();
        });
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
    runWithCodexCli,
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
    const msg = text.trim();
    if (!msg || sending || commandBusy) return;
    const parsed = parseSlashCommand(msg);
    if (!parsed) {
      await transmit(msg, mode);
      return;
    }

    setCommandBusy(true);
    setError(null);
    setNotice(null);
    try {
      await executeSlash(parsed, mode);
      setText("");
      setAt(null);
      setSlashDismissed(false);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setCommandBusy(false);
    }
  }, [commandBusy, executeSlash, sending, text, transmit]);

  // 立即发送会打断当前运行，保留二次确认（原先是本地手写的 4s 计时器）
  const sendNow = useArmedAction(() => void send("send_now"));
  const disarmSendNow = sendNow.disarm;
  useEffect(() => {
    if (!running) disarmSendNow();
  }, [running, disarmSendNow]);

  const removeQueued = useAsyncAction(async (queueId: string) => {
    await agentQueueRemove(taskId, queueId);
    await refreshDetail(taskId);
  }, { label: "移除队列消息" });

  // 复用当前草稿作为子任务，而不是把它同时送进主 Agent。这样用户可以在运行中
  // 临时请 Codex 做独立只读调查，主会话历史不会被一条“请另一个 Agent 查一下”污染。
  const delegateCodex = useAsyncAction(async () => {
    const goal = text.trim();
    if (!goal) return;
    await runWithCodexCli({ feature: "Codex 快速子代理", requireAuth: true }, async () => {
      await agentDelegateCodex(taskId, goal, "只读调查");
      setText("");
      setAt(null);
      await refreshDetail(taskId);
    });
  }, { label: "委派 Codex 子代理" });

  // MCP 会话会保留 Codex thread ID，适合需要在同一外部上下文中继续追问的调查。
  // 它和快速 exec 委派都使用当前草稿，但只会创建一项独立只读子任务。
  const delegateCodexMcp = useAsyncAction(async () => {
    const goal = text.trim();
    if (!goal) return;
    await runWithCodexCli({ feature: "Codex MCP 子代理", requireAuth: true }, async () => {
      await agentDelegateCodexMcp(taskId, goal, "会话调查");
      setText("");
      setAt(null);
      await refreshDetail(taskId);
    });
  }, { label: "以 MCP 会话委派 Codex 子代理" });

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
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      if (running) {
        if (e.ctrlKey || e.metaKey) sendNow.trigger();
        else if (e.altKey) void send("queue");
        else void send("steer");
      } else {
        void send();
      }
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
      {delegateCodex.error && (
        <StatusBar kind="error" compact onDismiss={delegateCodex.clearError}>
          {delegateCodex.error}
        </StatusBar>
      )}
      {delegateCodexMcp.error && (
        <StatusBar kind="error" compact onDismiss={delegateCodexMcp.clearError}>
          {delegateCodexMcp.error}
        </StatusBar>
      )}
      {notice && (
        <StatusBar kind="info" compact onDismiss={() => setNotice(null)}>
          {notice}
        </StatusBar>
      )}
      {slashOpen && (
        <SlashCommandMenu
          value={text}
          context={slashContext}
          activeIndex={slashActive}
          onActiveIndexChange={setSlashActive}
          onPick={pickSlash}
        />
      )}
      {at && (
        <div className="at-menu popover popover--up" role="listbox" aria-label="引用文件">
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
        </div>
      )}
      <div className="comp-box">
        <textarea
          ref={taRef}
          rows={2}
          value={text}
          aria-label="给 Agent 的消息"
          placeholder={
            running
              ? "正在处理，可继续补充要求…"
              : "回复、提问或补充上下文…（输入 @ 引用文件）"
          }
          onChange={(e) => {
            setText(e.target.value);
            setSlashActive(0);
            setSlashDismissed(false);
            if (e.target.value.startsWith("/")) setAt(null);
            else detectAt(e.target.value, e.target.selectionStart ?? e.target.value.length);
          }}
          onKeyDown={onKeyDown}
        />

        {/* 输入区脚下的控件：与新对话页同构的「模型」「权限」入口 */}
        <div className="comp-meta">
          <ModelSwitcher
            taskId={taskId}
            providerName={providerName}
            model={model}
            choices={providerChoices}
            fallback={providerFallback}
            running={running}
            onChanged={onProviderChanged}
            variant="pill"
            openRequest={modelMenuRequest}
          />
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
          <span className="spacer" />
          {!running && (
            <button
              className="send"
              disabled={!text.trim() || sending || commandBusy}
              onClick={() => void send("auto")}
              aria-label="发送消息"
              title="发送（Enter）"
            >
              <IconSend width={12} height={12} />
            </button>
          )}
        </div>

        {running && (
          <div className="run-command-bar" aria-label="运行中消息操作">
            <button
              className="run-command-action primary"
              type="button"
              disabled={!text.trim() || sending || commandBusy}
              onClick={() => void send("steer")}
              title="作为引导注入当前运行（Enter）"
            >
              <IconSend width={11} height={11} />
              引导 <kbd>Enter</kbd>
            </button>
            <button
              className="run-command-action"
              type="button"
              disabled={!text.trim() || sending || commandBusy}
              onClick={() => void send("queue")}
              title="当前消息将在本轮结束后发送（Alt+Enter）"
            >
              排队 <kbd>Alt+Enter</kbd>
            </button>

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

            <Menu
              label="更多发送方式"
              placement="up"
              align="right"
              menuClassName="comp-more-menu"
              trigger={
                <button
                  className="run-command-more"
                  type="button"
                  disabled={!text.trim() || sending || commandBusy}
                  aria-label="更多运行中发送操作"
                  title="更多操作"
                >
                  <IconChevronDown width={12} height={12} />
                </button>
              }
            >
              {({ close }) => (
                <>
                  <MenuItem
                    close={close}
                    closeOnSelect={false}
                    className={sendNow.armed ? "confirm is-destructive" : "is-destructive"}
                    shortcut="Ctrl+Enter"
                    onSelect={() => {
                      sendNow.trigger();
                      if (sendNow.armed) close();
                    }}
                  >
                    {sendNow.armed ? "确认立即发送" : "立即发送"}
                  </MenuItem>
                  {sendNow.armed && (
                    <p className="comp-send-now-note" role="status">
                      将停止当前运行；再次点击或按 Ctrl+Enter 确认
                    </p>
                  )}
                  <MenuItem
                    close={close}
                    disabled={!workspaceAttached || delegateCodex.busy}
                    hint={workspaceAttached ? "以只读模式独立检查当前工作区" : "先附加本地工作区后才能使用"}
                    onSelect={() => void delegateCodex.run()}
                  >
                    {delegateCodex.busy ? "正在委派 Codex…" : "委派给 Codex（快速）"}
                  </MenuItem>
                  <MenuItem
                    close={close}
                    disabled={!workspaceAttached || delegateCodexMcp.busy}
                    hint={workspaceAttached ? "保留 Codex 外部会话，可用于后续续接" : "先附加本地工作区后才能使用"}
                    onSelect={() => void delegateCodexMcp.run()}
                  >
                    {delegateCodexMcp.busy ? "正在建立 Codex MCP 会话…" : "委派给 Codex（MCP 会话）"}
                  </MenuItem>
                </>
              )}
            </Menu>

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
