import { memo, useEffect, useId, useState } from "react";
import {
  IconActivity,
  IconChevronDown,
  IconChevronRight,
  IconFile,
  IconSearch,
  IconTerminal,
} from "../icons";
import {
  hasMcpConfirmationPayload,
  hasMcpSettingsActionPayload,
  ToolCard,
  ToolPayloadDetails,
} from "./ToolCard";
import { SubagentAvatar } from "./SubagentIdentity";
import type {
  TimelineSubagentEntry,
  TimelineSubagentGroupItem,
  TimelineToolGroupItem,
  TimelineToolGroupKind,
  TimelineToolItem,
} from "./timeline-presentation";
import { toolActivityProgress, toolActivityTitle } from "./tool-activity";

interface ActivityGroupProps {
  item: TimelineToolGroupItem;
  dim?: string;
}

export const TimelineToolGroup = memo(function TimelineToolGroup({ item, dim = "" }: ActivityGroupProps) {
  const hasMcpAction = item.tools.some((tool) => (
    hasMcpConfirmationPayload(tool.name, tool.outputJson)
      || hasMcpSettingsActionPayload(tool.name, tool.outputJson)
  ));
  const [open, setOpen] = useState(hasMcpAction);
  const generatedId = useId().replace(/:/g, "");
  const detailId = `timeline-activity-${generatedId}`;
  const hasDetails = item.tools.some((tool) => Boolean(tool.inputJson?.trim() || tool.outputJson?.trim()));
  const state = groupState(item.tools);
  const title = toolActivityTitle(
    item.groupKind,
    item.tools.length,
    state,
    item.tools[0]?.target || item.tools[0]?.name || "",
  );
  const single = item.tools.length === 1 ? item.tools[0] : null;

  useEffect(() => {
    if (hasMcpAction) setOpen(true);
  }, [hasMcpAction]);

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
        <span className={`timeline-activity-state state-${state}`}>
          {toolActivityProgress(item.tools.map((tool) => tool.state))}
        </span>
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
                toolName={single.name}
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
  collapsible = false,
  dim = "",
}: {
  t: number;
  label: string;
  detail: string | null;
  collapsible?: boolean;
  dim?: string;
}) {
  const [open, setOpen] = useState(false);
  const generatedId = useId().replace(/:/g, "");
  const detailId = `timeline-context-${generatedId}`;
  const hasDetails = Boolean(detail?.trim());

  if (collapsible && hasDetails) {
    const preview = detail?.replace(/\s+/g, " ").trim() ?? "";
    return (
      <div
        className={`timeline-context-event is-collapsible${open ? " open" : ""}${dim}`}
        data-t={t}
      >
        <button
          type="button"
          className="timeline-context-toggle ring-inset"
          aria-expanded={open}
          aria-controls={detailId}
          title={open ? "收起思考内容" : "展开思考内容"}
          onClick={() => setOpen((value) => !value)}
        >
          <span className="timeline-activity-icon" aria-hidden="true">
            <IconActivity width={14} height={14} />
          </span>
          <span className="timeline-context-label">{label}</span>
          <small>{preview}</small>
          <span className="timeline-activity-chevron" aria-hidden="true">
            {open ? <IconChevronDown width={13} height={13} /> : <IconChevronRight width={13} height={13} />}
          </span>
        </button>
        {open && (
          <div className="timeline-context-detail" id={detailId}>
            {detail}
          </div>
        )}
      </div>
    );
  }

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
              <SubagentAvatar
                index={index}
                identity={agent.id}
                runtimeKind={agent.runtimeKind}
                size="xs"
                className="timeline-subagent-avatar"
              />
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

function groupState(tools: readonly TimelineToolItem[]): "active" | "ok" | "fail" {
  if (tools.some((tool) => tool.state === "active")) return "active";
  if (tools.some((tool) => tool.state === "fail")) return "fail";
  return "ok";
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
