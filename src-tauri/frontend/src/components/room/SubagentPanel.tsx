/**
 * 子代理概览。
 *
 * 主对话只保留一条可展开的运行树和扁平列表；完整审计在右侧详情中查看，避免
 * “面板 → 分组 → 卡片”三层边框。这里不渲染模型私有推理。
 */
import { useEffect, useMemo, useState, type CSSProperties } from "react";
import { useSharedNow } from "../../lib/shared-clock";
import type { ActivitySubagent, ActivityTraceState } from "./activity";
import { IconCheck, IconChevronDown, IconChevronRight, IconStop, IconSubagent } from "../icons";

interface Props {
  state: ActivityTraceState;
  selectedSubagentId?: string | null;
  onInspectSubagent?: (subagentId: string) => void;
  onAbortSubagent?: (subagentId: string) => Promise<void>;
  openRequest?: number;
}

export function SubagentPanel({
  state,
  selectedSubagentId,
  onInspectSubagent,
  onAbortSubagent,
  openRequest,
}: Props) {
  const [open, setOpen] = useState(true);
  const [stoppingId, setStoppingId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const active = useMemo(
    () => state.subagents.filter((child) => isActive(child.status)),
    [state.subagents]
  );
  const done = useMemo(
    () => [...state.subagents.filter((child) => !isActive(child.status))].reverse(),
    [state.subagents]
  );
  const now = useSharedNow(active.length > 0 ? 1000 : null);

  useEffect(() => {
    if (active.length > 0) setOpen(true);
  }, [active.length]);

  useEffect(() => {
    if (openRequest) setOpen(true);
  }, [openRequest]);

  if (state.subagents.length === 0) return null;

  const stop = async (subagentId: string) => {
    if (!onAbortSubagent || stoppingId) return;
    setStoppingId(subagentId);
    setError(null);
    try {
      await onAbortSubagent(subagentId);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setStoppingId(null);
    }
  };

  return (
    <section className="subagent-panel" aria-label="子代理">
      <button
        className="subagent-panel-toggle"
        type="button"
        onClick={() => setOpen((value) => !value)}
        aria-expanded={open}
      >
        <span className="subagent-panel-title">
          <i className={active.length > 0 ? "is-active" : ""} aria-hidden="true" />
          子代理
        </span>
        <span className="subagent-panel-count">
          {active.length > 0 ? `${active.length} 正在运行` : "无运行中"}
          {done.length > 0 ? ` · ${done.length} 已完成` : ""}
        </span>
        {open ? <IconChevronDown width={13} height={13} /> : <IconChevronRight width={13} height={13} />}
      </button>

      {open && (
        <div className="subagent-list opt-subagent-lines in-panel">
          {active.length > 0 && <div className="subagent-list-label">正在运行</div>}
          {active.map((child) => (
            <AgentRow
              key={child.id}
              child={child}
              now={now}
              selected={selectedSubagentId === child.id}
              stopping={stoppingId === child.id}
              onInspect={onInspectSubagent ? () => onInspectSubagent(child.id) : undefined}
              onStop={onAbortSubagent ? () => void stop(child.id) : undefined}
            />
          ))}
          {done.length > 0 && <div className="subagent-list-label done">最近完成</div>}
          {done.map((child) => (
            <AgentRow
              key={child.id}
              child={child}
              now={now}
              selected={selectedSubagentId === child.id}
              onInspect={onInspectSubagent ? () => onInspectSubagent(child.id) : undefined}
            />
          ))}
        </div>
      )}

      {error && <div className="subagent-panel-error">停止失败：{error}</div>}
    </section>
  );
}

function AgentRow({
  child,
  now,
  selected,
  stopping = false,
  onInspect,
  onStop,
}: {
  child: ActivitySubagent;
  now: number;
  selected: boolean;
  stopping?: boolean;
  onInspect?: () => void;
  onStop?: () => void;
}) {
  const observation = childObservation(child);
  const running = isActive(child.status);
  // sheen 交错延迟与 opt-room.css 的 --agent-delay 消费点对齐（面板内行）。
  const delayStyle = { "--agent-delay": "0s" } as CSSProperties;
  return (
    <div
      className={`opt-subagent-line${selected ? " selected" : ""}`}
      style={delayStyle}
      role={onInspect ? "button" : undefined}
      tabIndex={onInspect ? 0 : undefined}
      aria-pressed={onInspect ? selected : undefined}
      title={[statusLabel(child.status), observation, elapsedLabel(child.startedAt, child.endedAt ?? now)]
        .filter(Boolean)
        .join(" · ")}
      onClick={onInspect}
      onKeyDown={
        onInspect
          ? (event) => {
              if (event.key !== "Enter" && event.key !== " ") return;
              event.preventDefault();
              onInspect();
            }
          : undefined
      }
    >
      <span className="opt-icon" aria-hidden="true">
        {running ? <IconSubagent width={16} height={16} /> : <IconCheck width={16} height={16} />}
      </span>
      <span className="opt-agent-kind">子智能体</span>
      <span
        className={`opt-agent-name ${running ? "is-running" : "is-complete"}`}
        data-name={child.label}
      >
        {child.label}
      </span>
      <span className="opt-agent-separator" aria-hidden="true">·</span>
      <span className="opt-agent-description">
        {statusLabel(child.status)} · {elapsedLabel(child.startedAt, child.endedAt ?? now)} · {observation}
      </span>
      {onStop && (
        <button
          className="opt-agent-stop"
          type="button"
          disabled={stopping}
          onClick={(event) => {
            event.stopPropagation();
            onStop();
          }}
          aria-label={`停止 ${child.label}`}
          title="停止子代理"
        >
          <IconStop width={11} height={11} />
          <span>{stopping ? "停止中" : "停止"}</span>
        </button>
      )}
    </div>
  );
}

function isActive(status: ActivitySubagent["status"]): boolean {
  return status === "queued" || status === "running" || status === "waiting_permission";
}

function statusLabel(status: ActivitySubagent["status"]): string {
  switch (status) {
    case "queued": return "等待执行";
    case "running": return "工作中";
    case "waiting_permission": return "等待权限";
    case "completed": return "已完成";
    case "failed": return "失败";
    case "cancelled": return "已停止";
  }
}

function childObservation(child: ActivitySubagent): string {
  if (child.detail) return child.detail;
  switch (child.status) {
    case "queued": return "等待调度";
    case "running": return "等待第一条进度";
    case "waiting_permission": return "等待权限批准";
    case "completed": return "已完成，暂无摘要";
    case "failed": return "运行未完成";
    case "cancelled": return "已停止";
  }
}

function elapsedLabel(startedAt: number, now: number): string {
  const seconds = Math.max(0, Math.floor((now - startedAt) / 1000));
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  return minutes > 0 ? `${minutes}分${String(rest).padStart(2, "0")}秒` : `${rest}秒`;
}
