/**
 * 子代理概览。
 *
 * 主对话只保留一条可展开的运行树和扁平列表；完整审计在右侧详情中查看，避免
 * “面板 → 分组 → 卡片”三层边框。这里不渲染模型私有推理。
 */
import { useEffect, useMemo, useState } from "react";
import type { ActivitySubagent, ActivityTraceState } from "./activity";
import { IconChevronDown, IconChevronRight, IconStop } from "../icons";

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
  const [now, setNow] = useState(() => Date.now());
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

  useEffect(() => {
    if (active.length > 0) setOpen(true);
  }, [active.length]);

  useEffect(() => {
    if (openRequest) setOpen(true);
  }, [openRequest]);

  useEffect(() => {
    if (active.length === 0) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [active.length]);

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
        <div className="subagent-list">
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
  return (
    <div className={`subagent-row status-${child.status}${selected ? " selected" : ""}`}>
      <button className="subagent-row-main" type="button" onClick={onInspect} disabled={!onInspect}>
        <span className="subagent-row-lamp" aria-hidden="true" />
        <span className="subagent-row-copy">
          <span className="subagent-row-topline">
            <strong title={child.label}>{child.label}</strong>
            <span>{statusLabel(child.status)}</span>
          </span>
          <span className="subagent-row-observation" title={observation}>{observation}</span>
        </span>
        <span className="subagent-row-time">
          {elapsedLabel(child.startedAt, child.endedAt ?? now)}
        </span>
        {onInspect && <IconChevronRight className="subagent-row-arrow" width={13} height={13} />}
      </button>
      {onStop && (
        <button
          className="subagent-row-stop"
          type="button"
          disabled={stopping}
          onClick={onStop}
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
