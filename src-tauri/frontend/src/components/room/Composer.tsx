/**
 * Room 输入区 —— Enter 发送 / Shift+Enter 换行；运行中以引导为主动作。
 * `@` 触发 quickOpen 文件下拉，选中后插入 @path 文本。
 *
 * 输入区脚下现在镜像了「模型」与「权限」两个控件（与新对话页同构）。原先这里
 * 只有一个只读的模型状态芯片，想换模型或改权限必须回到会话顶栏找。
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { agentQueueRemove, agentSend, quickOpen } from "../../lib/ipc";
import type { AgentSendMode, ProjectAccessMode, QueuedMessage } from "../../lib/types";
import type { ProviderChoice } from "../../lib/provider";
import { useArmedAction, useAsyncAction } from "../../lib/hooks";
import { useTasksStore } from "../../store/tasks";
import { Menu, MenuItem } from "../ui/Menu";
import { StatusBar } from "../ui/StatusBar";
import { ProjectAccessSelector } from "../ProjectAccessSelector";
import { ModelSwitcher } from "./ModelSwitcher";
import { IconChevronDown, IconSend, IconStop } from "../icons";

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
}: Props) {
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const [at, setAt] = useState<AtState | null>(null);
  const taRef = useRef<HTMLTextAreaElement>(null);
  const debRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const refreshDetail = useTasksStore((s) => s.refreshDetail);

  useEffect(() => () => {
    if (debRef.current) window.clearTimeout(debRef.current);
  }, []);

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

  const send = useCallback(async (mode: AgentSendMode = "auto") => {
    const msg = text.trim();
    if (!msg || sending) return;
    setSending(true);
    setError(null);
    try {
      // 不等待 IPC 往返：运行中的引导、排队和立即发送都应立即可见。
      onSent(msg, mode);
      await agentSend(taskId, msg, mode);
      // IPC 成功后才把引导标为"已接纳"，失败时由下方 catch 回滚时间线。
      onActivitySent(mode);
      setText("");
      setAt(null);
      await refreshDetail(taskId);
    } catch (e) {
      // 后端在无 provider 配置等情况下返回错误字符串 —— 必须可见
      setError(String(e));
      onSendFailed();
    } finally {
      setSending(false);
    }
  }, [text, sending, taskId, onSent, onSendFailed, onActivitySent, refreshDetail]);

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

  const abort = useAsyncAction(onAbort, { label: "中断" });

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
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
            detectAt(e.target.value, e.target.selectionStart ?? e.target.value.length);
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
          />
          {workspaceAttached && (
            <ProjectAccessSelector
              value={workspaceAccessMode}
              workspaceName={workspaceName ?? "当前工作区"}
              placement="up"
              disabled={scopeBusy || running}
              onChange={onAccessModeChange}
            />
          )}
          <span className="spacer" />
          {!running && (
            <button
              className="send"
              disabled={!text.trim() || sending}
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
              disabled={!text.trim() || sending}
              onClick={() => void send("steer")}
              title="作为引导注入当前运行（Enter）"
            >
              <IconSend width={11} height={11} />
              引导 <kbd>Enter</kbd>
            </button>
            <button
              className="run-command-action"
              type="button"
              disabled={!text.trim() || sending}
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
                  disabled={!text.trim() || sending}
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
