/**
 * 子代理监督面板。
 * 只展示运行树、已分类工具动作、权限等待与最终摘要，不渲染模型增量或私有推理。
 */
import { useEffect, useMemo, useState, type ReactNode } from "react";
import type { ActivitySubagent, ActivityTraceState } from "./activity";
import { IconStop } from "../icons";

interface Props {
  state: ActivityTraceState;
  onAbortSubagent?: (subagentId: string) => Promise<void>;
}

export function SubagentPanel({ state, onAbortSubagent }: Props) {
  const [now, setNow] = useState(() => Date.now());
  const [activeOpen, setActiveOpen] = useState(true);
  const [doneOpen, setDoneOpen] = useState(false);
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
    if (active.length > 0) setActiveOpen(true);
  }, [active.length]);

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
    <section className="subagent-panel" aria-label="子代理监督">
      <div className="subagent-panel-head">
        <span className="subagent-panel-title">子代理监督</span>
        <span className="subagent-panel-count">
          Active {active.length} · Done {done.length}
        </span>
      </div>

      {active.length > 0 && (
        <AgentGroup
          title="Active"
          count={active.length}
          open={activeOpen}
          onToggle={() => setActiveOpen((value) => !value)}
        >
          {active.map((child) => (
            <AgentCard
              key={child.id}
              child={child}
              now={now}
              stopping={stoppingId === child.id}
              onStop={onAbortSubagent ? () => void stop(child.id) : undefined}
            />
          ))}
        </AgentGroup>
      )}

      {done.length > 0 && (
        <AgentGroup
          title="Done"
          count={done.length}
          open={doneOpen}
          onToggle={() => setDoneOpen((value) => !value)}
        >
          {done.map((child) => (
            <AgentCard key={child.id} child={child} now={now} />
          ))}
        </AgentGroup>
      )}

      {error && <div className="subagent-panel-error">停止子代理失败：{error}</div>}
    </section>
  );
}

function AgentGroup({
  title,
  count,
  open,
  onToggle,
  children,
}: {
  title: "Active" | "Done";
  count: number;
  open: boolean;
  onToggle: () => void;
  children: ReactNode;
}) {
  return (
    <div className={`subagent-group group-${title.toLowerCase()}`}>
      <button
        className="subagent-group-toggle"
        type="button"
        onClick={onToggle}
        aria-expanded={open}
      >
        <span>{title}</span>
        <span className="subagent-group-count">{count}</span>
        <span className="subagent-group-arrow">{open ? "收起" : "展开"}</span>
      </button>
      {open && <div className="subagent-group-list">{children}</div>}
    </div>
  );
}

function AgentCard({
  child,
  now,
  stopping = false,
  onStop,
}: {
  child: ActivitySubagent;
  now: number;
  stopping?: boolean;
  onStop?: () => void;
}) {
  const terminal = !isActive(child.status);
  const observation = childObservation(child);
  return (
    <article className={`subagent-card status-${child.status}`}>
      <span className="subagent-card-lamp" aria-hidden="true" />
      <div className="subagent-card-main">
        <div className="subagent-card-topline">
          <span className="subagent-card-label" title={child.label}>
            {child.label}
          </span>
          <span className="subagent-card-status">{statusLabel(child.status)}</span>
          <span className="subagent-card-elapsed">
            {elapsedLabel(child.startedAt, child.endedAt ?? now)}
          </span>
        </div>
        <div className={`subagent-card-observation${terminal ? " summary" : ""}`} title={observation}>
          {terminal ? "摘要：" : "动作："}{observation}
        </div>
      </div>
      {onStop && (
        <button className="subagent-card-stop" type="button" disabled={stopping} onClick={onStop}>
          <IconStop width={11} height={11} /> {stopping ? "停止中…" : "停止"}
        </button>
      )}
    </article>
  );
}

function isActive(status: ActivitySubagent["status"]): boolean {
  return status === "queued" || status === "running" || status === "waiting_permission";
}

function statusLabel(status: ActivitySubagent["status"]): string {
  switch (status) {
    case "queued":
      return "等待执行";
    case "running":
      return "工作中";
    case "waiting_permission":
      return "等待权限";
    case "completed":
      return "已完成";
    case "failed":
      return "失败";
    case "cancelled":
      return "已停止";
  }
}

function childObservation(child: ActivitySubagent): string {
  if (child.detail) return child.detail;
  switch (child.status) {
    case "queued":
      return "等待调度";
    case "running":
      return "等待可观察动作";
    case "waiting_permission":
      return "等待权限批准";
    case "completed":
      return "已完成，暂无摘要";
    case "failed":
      return "运行未完成";
    case "cancelled":
      return "已停止";
  }
}

function elapsedLabel(startedAt: number, now: number): string {
  const seconds = Math.max(0, Math.floor((now - startedAt) / 1000));
  const minutes = Math.floor(seconds / 60);
  const rest = seconds % 60;
  return minutes > 0 ? `${minutes}分${String(rest).padStart(2, "0")}秒` : `${rest}秒`;
}
