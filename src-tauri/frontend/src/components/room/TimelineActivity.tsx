import { memo, useId, useState } from "react";
import {
  IconActivity,
  IconChevronDown,
  IconChevronRight,
  IconFile,
  IconSearch,
  IconTerminal,
} from "../icons";
import { ToolCard, ToolPayloadDetails } from "./ToolCard";
import { SubagentAvatar } from "./SubagentIdentity";
import type {
  TimelineSubagentEntry,
  TimelineSubagentGroupItem,
  TimelineToolGroupItem,
  TimelineToolGroupKind,
  TimelineToolItem,
} from "./timeline-presentation";

interface ActivityGroupProps {
  item: TimelineToolGroupItem;
  dim?: string;
}

export const TimelineToolGroup = memo(function TimelineToolGroup({ item, dim = "" }: ActivityGroupProps) {
  const [open, setOpen] = useState(false);
  const generatedId = useId().replace(/:/g, "");
  const detailId = `timeline-activity-${generatedId}`;
  const hasDetails = item.tools.some((tool) => Boolean(tool.inputJson?.trim() || tool.outputJson?.trim()));
  const title = toolGroupTitle(item.groupKind, item.tools);
  const state = groupState(item.tools);
  const single = item.tools.length === 1 ? item.tools[0] : null;

  return (
    <div
      className={`timeline-activity-event kind-${item.groupKind} state-${state}${open ? " open" : ""}${dim}`}
      data-t={item.t}
    >
      <button
        type="button"
        className="timeline-activity-toggle ring-inset"
        aria-expanded={hasDetails ? open : undefined}
        aria-controls={hasDetails ? detailId : undefined}
        disabled={!hasDetails}
        title={hasDetails ? (open ? "收起活动详情" : "展开活动详情") : title}
        onClick={() => setOpen((value) => !value)}
      >
        <span className="timeline-activity-icon" aria-hidden="true">
          <ActivityIcon kind={item.groupKind} />
        </span>
        <span className="timeline-activity-title" title={title}>{title}</span>
        <span className={`timeline-activity-state state-${state}`}>{groupStateLabel(state, item.tools.length)}</span>
        {hasDetails && (
          <span className="timeline-activity-chevron" aria-hidden="true">
            {open ? <IconChevronDown width={13} height={13} /> : <IconChevronRight width={13} height={13} />}
          </span>
        )}
      </button>

      {open && (
        <div className="timeline-activity-details" id={detailId}>
          {single ? (
            <div className="timeline-activity-single-detail">
              <ToolPayloadDetails
                inputJson={single.inputJson}
                outputJson={single.outputJson}
                state={single.state}
              />
            </div>
          ) : (
            <div className="timeline-command-list" aria-label="命令详情">
              {item.tools.map((tool) => (
                <ToolCard
                  key={tool.id}
                  t={tool.t}
                  name={tool.name}
                  target={tool.target}
                  state={tool.state}
                  summary={tool.summary}
                  inputJson={tool.inputJson}
                  outputJson={tool.outputJson}
                />
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
});

export function TimelineContextEvent({
  t,
  label,
  detail,
  dim = "",
}: {
  t: number;
  label: string;
  detail: string | null;
  dim?: string;
}) {
  return (
    <div className={`timeline-context-event${dim}`} data-t={t} title={detail ?? undefined}>
      <span className="timeline-activity-icon" aria-hidden="true"><IconActivity width={14} height={14} /></span>
      <span>{label}</span>
      {detail && <small>{detail}</small>}
    </div>
  );
}

export function TimelineSubagentGroup({
  item,
  selectedSubagentId,
  onInspectSubagent,
  dim = "",
}: {
  item: TimelineSubagentGroupItem;
  selectedSubagentId?: string | null;
  onInspectSubagent?: (runId: string) => void;
  dim?: string;
}) {
  const status = subagentGroupStatus(item.agents);
  return (
    <div className={`timeline-subagent-event${dim}`} data-t={item.t} aria-label="子代理运行">
      <div className="timeline-subagent-chips">
        {item.agents.map((agent, index) => {
          const inspectable = Boolean(agent.runId && onInspectSubagent);
          const selected = Boolean(agent.runId && selectedSubagentId === agent.runId);
          return (
            <button
              key={agent.id}
              type="button"
              className={`timeline-subagent-chip status-${agent.status}${selected ? " selected" : ""}`}
              disabled={!inspectable}
              aria-pressed={inspectable ? selected : undefined}
              aria-label={`${agent.label}，${subagentStatusLabel(agent.status)}`}
              title={[agent.summary, agent.model].filter(Boolean).join(" · ") || undefined}
              onClick={() => agent.runId && onInspectSubagent?.(agent.runId)}
            >
              <SubagentAvatar index={index} size="xs" className="timeline-subagent-avatar" />
              <span>{agent.label}</span>
            </button>
          );
        })}
        <span className="timeline-subagent-summary">{status}</span>
      </div>
    </div>
  );
}

function ActivityIcon({ kind }: { kind: TimelineToolGroupKind }) {
  if (kind === "command") return <IconTerminal width={14} height={14} />;
  if (kind === "file") return <IconFile width={14} height={14} />;
  if (kind === "lookup") return <IconSearch width={14} height={14} />;
  return <IconActivity width={14} height={14} />;
}

function toolGroupTitle(kind: TimelineToolGroupKind, tools: readonly TimelineToolItem[]): string {
  const active = tools.some((tool) => tool.state === "active");
  const failed = tools.some((tool) => tool.state === "fail");
  const target = compactTarget(tools[0]?.target || tools[0]?.name || "");

  if (kind === "command") {
    if (tools.length > 1) return active ? "正在运行多个命令" : failed ? "多个命令中有执行失败" : "运行了多个命令";
    if (active) return target ? `正在运行 ${target}` : "正在运行命令";
    if (failed) return target ? `命令执行失败：${target}` : "命令执行失败";
    return target ? `已运行 ${target}` : "已运行命令";
  }
  if (kind === "file") {
    if (active) return "正在编辑文件";
    return failed ? "文件编辑未完成" : "已编辑的文件";
  }
  if (kind === "lookup") {
    if (tools.length > 1) return active ? "正在检查多个文件" : "检查了多个文件";
    if (active) return target ? `正在检查 ${target}` : "正在检查文件";
    return target ? `已检查 ${target}` : "已检查文件";
  }
  if (tools.length > 1) return active ? "正在使用多个工具" : "使用了多个工具";
  return active ? `正在使用 ${target || "工具"}` : `已使用 ${target || "工具"}`;
}

function compactTarget(value: string): string {
  const normalized = value.trim().replace(/\s+/g, " ");
  return normalized.length > 72 ? `${normalized.slice(0, 71)}…` : normalized;
}

function groupState(tools: readonly TimelineToolItem[]): "active" | "ok" | "fail" {
  if (tools.some((tool) => tool.state === "active")) return "active";
  if (tools.some((tool) => tool.state === "fail")) return "fail";
  return "ok";
}

function groupStateLabel(state: "active" | "ok" | "fail", count: number): string {
  if (state === "active") return "运行中";
  if (state === "fail") return "失败";
  return count > 1 ? `${count} 项完成` : "完成";
}

function subagentGroupStatus(agents: readonly TimelineSubagentEntry[]): string {
  const active = agents.filter((agent) =>
    agent.status === "queued" || agent.status === "running" || agent.status === "waiting_permission"
  ).length;
  const completed = agents.filter((agent) => agent.status === "completed").length;
  const failed = agents.length - active - completed;
  const parts = [
    completed > 0 ? `${completed} 已完成` : null,
    active > 0 ? `${active} 进行中` : null,
    failed > 0 ? `${failed} 未完成` : null,
  ].filter(Boolean);
  return parts.join(" · ") || "等待状态";
}

function subagentStatusLabel(status: TimelineSubagentEntry["status"]): string {
  switch (status) {
    case "queued": return "等待执行";
    case "running": return "运行中";
    case "waiting_permission": return "等待权限";
    case "completed": return "已完成";
    case "failed": return "失败";
    case "cancelled": return "已停止";
  }
}
