import { memo, useEffect, useId, useMemo, useState } from "react";
import {
  IconActivity,
  IconChevronDown,
  IconChevronRight,
  IconFile,
  IconSearch,
  IconSubagent,
  IconTerminal,
} from "../icons";
import {
  hasMcpConfirmationPayload,
  hasMcpSettingsActionPayload,
  ToolCard,
  ToolPayloadDetails,
} from "./ToolCard";
import { FileTypeIcon } from "./FileTypeIcon";
import { SubagentAvatar } from "./SubagentIdentity";
import type {
  TimelineSubagentEntry,
  TimelineSubagentGroupItem,
  TimelineToolGroupItem,
  TimelineToolGroupKind,
  TimelineToolItem,
} from "./timeline-presentation";
import { toolActivityProgress, toolActivityTitle, toolDiffStat } from "./tool-activity";
import { toolVerb } from "../../lib/format";

interface ActivityGroupProps {
  item: TimelineToolGroupItem;
  dim?: string;
}

/** 文件活动超过该数量时折叠尾部，避免长会话被几十个文件行淹没。 */
const FILE_ROW_LIMIT = 6;

export const TimelineToolGroup = memo(function TimelineToolGroup(props: ActivityGroupProps) {
  if (props.item.groupKind === "file") return <TimelineFileRows {...props} />;
  return <TimelineToolGroupBase {...props} />;
});

const TimelineToolGroupBase = memo(function TimelineToolGroupBase({ item, dim = "" }: ActivityGroupProps) {
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
  const isReasoning = label === "思考过程" || label === "Codex 思考摘要";

  if (collapsible && hasDetails) {
    const preview = detail?.replace(/\s+/g, " ").trim() ?? "";
    return (
      <div
        className={`timeline-context-event is-collapsible${isReasoning ? " is-reasoning" : ""}${open ? " open" : ""}${dim}`}
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
  const [open, setOpen] = useState(true);
  const generatedId = useId().replace(/:/g, "");
  const detailId = `timeline-subagent-${generatedId}`;
  const status = subagentGroupStatus(item.agents);
  const primary = item.agents[0] ?? null;
  const primaryName = primary
    ? primary.label + (item.agents.length > 1 ? ` 等 ${item.agents.length} 个` : "")
    : "";
  const primaryGoal = item.agents.length === 1 ? primary?.goal ?? null : null;
  return (
    <div className={`timeline-subagent-event${open ? " open" : ""}${dim}`} data-t={item.t} aria-label="子代理运行">
      <button
        type="button"
        className="timeline-subagent-toggle ring-inset"
        aria-expanded={open}
        aria-controls={detailId}
        title={primaryGoal ?? undefined}
        onClick={() => setOpen((value) => !value)}
      >
        <span className="timeline-activity-icon" aria-hidden="true">
          <IconSubagent width={14} height={14} />
        </span>
        <span className="timeline-subagent-title">子智能体</span>
        {primaryName && <span className="timeline-subagent-name">{primaryName}</span>}
        {primaryGoal && <span className="timeline-subagent-goal">· {primaryGoal}</span>}
        <span className="timeline-subagent-state">{status}</span>
        <span className="timeline-activity-chevron" aria-hidden="true">
          {open ? <IconChevronDown width={13} height={13} /> : <IconChevronRight width={13} height={13} />}
        </span>
      </button>
      {open && (
        <div className="timeline-subagent-details" id={detailId}>
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
                  title={[agent.goal, agent.summary, agent.model].filter(Boolean).join(" · ") || undefined}
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
          </div>
        </div>
      )}
    </div>
  );
}

/**
 * 文件活动：每个文件独立成行（类型图标 + 文件名 + 行内 +N −N），
 * 不再折叠成「已编辑 N 个文件」——行内差异即时可见是这个界面的核心承诺。
 * 超过 FILE_ROW_LIMIT 个文件时折叠尾部，按需展开。
 */
function TimelineFileRows({ item, dim = "" }: ActivityGroupProps) {
  const [showAll, setShowAll] = useState(false);
  const tools = item.tools;
  const visible = showAll ? tools : tools.slice(0, FILE_ROW_LIMIT);
  const hidden = tools.length - visible.length;
  const state = groupState(tools);
  return (
    <div className={`timeline-file-event state-${state}${dim}`} data-t={item.t}>
      {visible.map((tool) => (
        <TimelineFileRow key={tool.id} tool={tool} />
      ))}
      {hidden > 0 && (
        <button
          type="button"
          className="timeline-file-more"
          aria-expanded={showAll}
          onClick={() => setShowAll(true)}
        >
          还有 {hidden} 个文件，显示全部
        </button>
      )}
    </div>
  );
}

const TimelineFileRow = memo(function TimelineFileRow({ tool }: { tool: TimelineToolItem }) {
  const [open, setOpen] = useState(false);
  const hasPayload = Boolean(tool.inputJson?.trim() || tool.outputJson?.trim());
  const verb = toolVerb(tool.name);
  const diff = useMemo(() => toolDiffStat(tool.inputJson, verb), [tool.inputJson, verb]);
  const target = tool.target || tool.name;
  const summaryVisible = tool.summary && tool.summary !== "done" ? tool.summary : "";
  return (
    <>
      <button
        type="button"
        className="timeline-file-row ring-inset"
        aria-expanded={hasPayload ? open : undefined}
        disabled={!hasPayload}
        title={target}
        onClick={() => setOpen((value) => !value)}
      >
        <span className="timeline-file-verb">{fileVerbLabel(tool.name, verb)}</span>
        <span className="timeline-file-icon" aria-hidden="true">
          <FileTypeIcon path={target} />
        </span>
        <span className="timeline-file-name">{fileNameOf(target)}</span>
        {tool.state === "active" ? (
          <span className="timeline-file-stat"><span className="spin" aria-hidden="true" /></span>
        ) : tool.state === "fail" ? (
          <span className="timeline-file-stat state-fail">✗ {summaryVisible || "失败"}</span>
        ) : diff ? (
          <span className="timeline-file-stat">
            <span className="plus">+{diff.plus}</span> <span className="minus">−{diff.minus}</span>
          </span>
        ) : (
          <span className="timeline-file-stat">{summaryVisible}</span>
        )}
        {hasPayload && (
          <span className="timeline-activity-chevron" aria-hidden="true">
            {open ? <IconChevronDown width={13} height={13} /> : <IconChevronRight width={13} height={13} />}
          </span>
        )}
      </button>
      {open && (
        <div className="timeline-activity-details">
          <div className="timeline-activity-single-detail">
            <ToolPayloadDetails
              toolName={tool.name}
              inputJson={tool.inputJson}
              outputJson={tool.outputJson}
              state={tool.state}
            />
          </div>
        </div>
      )}
    </>
  );
});

function fileNameOf(target: string): string {
  const parts = target.split(/[\\/]/);
  return parts[parts.length - 1] || target;
}

/** 文件行动词：读取/查看/编辑/写入（原型 C 的文件行语言）。 */
function fileVerbLabel(name: string, verb: string): string {
  if (verb === "write") return "写入";
  if (verb === "edit") return "编辑";
  if (verb === "read") return name.toLowerCase().includes("view") ? "查看" : "读取";
  return "处理";
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
