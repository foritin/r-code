/**
 * Room 输入区 —— Enter 发送 / Shift+Enter 换行；运行中以引导为主动作。
 * `@` 触发 quickOpen 文件下拉,选中后插入 @path 文本。
 * 发送错误(如未配置 provider)显示在上方错误条,不静默。
 */
import { useCallback, useEffect, useRef, useState } from "react";
import {
  agentQueueRemove,
  agentSend,
  quickOpen,
  settingsGet,
} from "../../lib/ipc";
import type { AgentSendMode, QueuedMessage } from "../../lib/types";
import { useTasksStore } from "../../store/tasks";
import { IconChevronDown, IconSend, IconStop } from "../icons";

interface Props {
  taskId: string;
  workspacePath: string | null;
  workspaceAttached: boolean;
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
  const [rtChip, setRtChip] = useState<{ cls: string; text: string }>({ cls: "", text: "…" });
  const [moreOpen, setMoreOpen] = useState(false);
  const [queueOpen, setQueueOpen] = useState(false);
  const [sendNowArmed, setSendNowArmed] = useState(false);
  const [aborting, setAborting] = useState(false);
  const [abortError, setAbortError] = useState<string | null>(null);
  const taRef = useRef<HTMLTextAreaElement>(null);
  const commandBarRef = useRef<HTMLDivElement>(null);
  const debRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const sendNowTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const lastRun = useTasksStore((s) => {
    const runs = s.details[taskId]?.runs;
    return runs && runs.length > 0 ? runs[runs.length - 1] : undefined;
  });
  const boundProviderName = useTasksStore(
    (s) => s.details[taskId]?.task.provider_name ?? null
  );

  const disarmSendNow = useCallback(() => {
    if (sendNowTimerRef.current) {
      window.clearTimeout(sendNowTimerRef.current);
      sendNowTimerRef.current = null;
    }
    setSendNowArmed(false);
  }, []);

  useEffect(() => () => {
    if (debRef.current) window.clearTimeout(debRef.current);
    if (sendNowTimerRef.current) window.clearTimeout(sendNowTimerRef.current);
  }, []);

  useEffect(() => {
    disarmSendNow();
  }, [text, disarmSendNow]);

  useEffect(() => {
    if (running) return;
    setMoreOpen(false);
    setQueueOpen(false);
    setAbortError(null);
    disarmSendNow();
  }, [running, disarmSendNow]);

  useEffect(() => {
    if (!queueOpen && !moreOpen) return;

    const dismissFromOutside = (event: PointerEvent) => {
      if (commandBarRef.current?.contains(event.target as Node)) return;
      setQueueOpen(false);
      setMoreOpen(false);
      disarmSendNow();
    };
    const dismissFromKeyboard = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      setQueueOpen(false);
      setMoreOpen(false);
      disarmSendNow();
      taRef.current?.focus();
    };

    document.addEventListener("pointerdown", dismissFromOutside);
    document.addEventListener("keydown", dismissFromKeyboard);
    return () => {
      document.removeEventListener("pointerdown", dismissFromOutside);
      document.removeEventListener("keydown", dismissFromKeyboard);
    };
  }, [queueOpen, moreOpen, disarmSendNow]);

  // 优先展示最近一次运行模型；避免把 runtime 内部术语暴露给用户。
  useEffect(() => {
    let dead = false;
    void (async () => {
      let provider = "";
      let cfgModel = "";
      try {
        const res = await settingsGet();
        provider = boundProviderName ?? res.config.default_provider ?? "";
        cfgModel = (provider && res.config.providers?.[provider]?.model) || "";
      } catch {
        /* 设置读取失败不阻塞输入区 */
      }
      if (dead) return;
      const runModel = lastRun?.model ?? "";
      if (runModel.toLowerCase().includes("mock")) {
        setRtChip({ cls: "", text: "演示模型" });
      } else if (runModel) {
        setRtChip({ cls: "ok", text: `模型：${runModel}` });
      } else if (provider) {
        setRtChip({ cls: "ok", text: `模型：${cfgModel || provider}` });
      } else {
        setRtChip({ cls: "warn", text: "未连接模型服务" });
      }
    })();
    return () => {
      dead = true;
    };
  }, [lastRun?.model, boundProviderName]);

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
      setAt({
        start,
        query,
        items: [],
        active: 0,
        error: "附加一个文件夹后才能引用本地文件",
      });
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
      // IPC 成功后才把引导标为“已接纳”，失败时会由下方 catch 回滚时间线。
      onActivitySent(mode);
      setMoreOpen(false);
      setQueueOpen(mode === "queue");
      disarmSendNow();
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
  }, [
    text,
    sending,
    taskId,
    onSent,
    onSendFailed,
    onActivitySent,
    refreshDetail,
    disarmSendNow,
  ]);

  const requestSendNow = useCallback(() => {
    if (!text.trim() || sending) return;
    if (sendNowArmed) {
      disarmSendNow();
      void send("send_now");
      return;
    }
    setMoreOpen(true);
    setSendNowArmed(true);
    if (sendNowTimerRef.current) window.clearTimeout(sendNowTimerRef.current);
    sendNowTimerRef.current = window.setTimeout(() => {
      sendNowTimerRef.current = null;
      setSendNowArmed(false);
    }, 4000);
  }, [text, sending, sendNowArmed, disarmSendNow, send]);

  const removeQueued = useCallback(async (queueId: string) => {
    try {
      await agentQueueRemove(taskId, queueId);
      await refreshDetail(taskId);
    } catch (e) {
      setError(String(e));
    }
  }, [taskId, refreshDetail]);

  const abort = useCallback(async () => {
    if (aborting) return;
    setAborting(true);
    setAbortError(null);
    try {
      await onAbort();
    } catch (cause) {
      setAbortError(String(cause));
    } finally {
      setAborting(false);
    }
  }, [aborting, onAbort]);

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
        if (e.ctrlKey || e.metaKey) {
          requestSendNow();
        } else if (e.altKey) {
          void send("queue");
        } else {
          void send("steer");
        }
      } else {
        void send();
      }
    }
  };

  return (
    <div className="composer">
      {error && (
        <div className="comp-error" role="alert">
          发送失败：{error}
        </div>
      )}
      {abortError && <div className="comp-error" role="alert">中断失败：{abortError}</div>}
      {at && (
        <div className="at-menu">
          {at.error ? (
            <div className="at-item dim">文件搜索失败:{at.error}</div>
          ) : at.items.length === 0 ? (
            <div className="at-item dim">无匹配文件</div>
          ) : (
            at.items.map((p, i) => (
              <button
                key={p}
                className={"at-item" + (i === at.active ? " on" : "")}
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
        <div className="comp-meta">
          <span className={"chip rt-chip " + rtChip.cls}>{rtChip.text}</span>
          <span className="spacer" />
          {!running && (
            <button
              className="send"
              disabled={!text.trim() || sending}
              onClick={() => void send("auto")}
              title="发送（Enter）"
            >
              <IconSend width={12} height={12} />
            </button>
          )}
        </div>
        {running && (
          <div className="run-command-bar" ref={commandBarRef} aria-label="运行中消息操作">
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
            <div className="run-queue">
              <button
                className="run-queue-summary"
                type="button"
                onClick={() => {
                  setQueueOpen((value) => !value);
                  setMoreOpen(false);
                  disarmSendNow();
                }}
                aria-expanded={queueOpen}
                aria-controls={`queue-popover-${taskId}`}
                aria-haspopup="dialog"
              >
                队列 <strong>{queuedMessages.length}</strong>
              </button>
              {queueOpen && (
                <div className="queue-popover" id={`queue-popover-${taskId}`} role="dialog" aria-label="待发送队列">
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
                          <button type="button" onClick={() => void removeQueued(item.id)} aria-label={`移除队列消息：${item.message}`}>
                            移除
                          </button>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              )}
            </div>
            <span className="run-command-spacer" />
            <div className="comp-more">
              <button
                className="run-command-more"
                type="button"
                disabled={!text.trim() || sending}
                onClick={() => {
                  setMoreOpen((value) => !value);
                  setQueueOpen(false);
                }}
                aria-label="更多运行中发送操作"
                aria-expanded={moreOpen}
                aria-haspopup="menu"
                title="更多操作"
              >
                <IconChevronDown width={12} height={12} />
              </button>
              {moreOpen && (
                <div className="comp-more-menu" role="menu">
                  <button
                    type="button"
                    role="menuitem"
                    className={sendNowArmed ? "confirm" : ""}
                    onClick={requestSendNow}
                  >
                    {sendNowArmed ? "确认立即发送" : "立即发送"}
                    <span>Ctrl+Enter</span>
                  </button>
                  {sendNowArmed && (
                    <p className="comp-send-now-note" role="status">
                      将停止当前运行；再次点击或按 Ctrl+Enter 确认
                    </p>
                  )}
                </div>
              )}
            </div>
            <button
              className="run-command-stop"
              type="button"
              disabled={aborting}
              onClick={() => void abort()}
              aria-label={aborting ? "正在中断当前运行" : "中断当前运行"}
              title="中断当前运行"
            >
              <IconStop width={11} height={11} />
              <span>{aborting ? "中断中" : "中断"}</span>
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
