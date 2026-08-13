import { memo, useEffect, useMemo, useRef, useState } from "react";
import {
  planAnswer,
  planApprove,
  planCancel,
  planCreate,
  planRepairProjection,
  planRetryContinuation,
  planRetryImplementation,
} from "../../lib/ipc";
import { useSharedNow } from "../../lib/shared-clock";
import type {
  PlanItem,
  PlanQuestionAnswerInput,
  PlanQuestionSet,
  PlanState,
  Task,
} from "../../lib/types";
import {
  IconCheck,
  IconChevronDown,
  IconChevronRight,
  IconHelp,
  IconRefresh,
} from "../icons";
import type { ActivitySubagent, SubagentStatus } from "../room/activity";
import { Markdown } from "../room/Markdown";
import { SubagentAvatar } from "../room/SubagentIdentity";
import { StatusBar } from "../ui/StatusBar";
import { formatPlanDescriptionMarkdown } from "./plan-description";
import type { TaskPlanController } from "./useTaskPlan";

type AnswerDraft =
  | { kind: "option"; optionId: string }
  | { kind: "text"; text: string };

interface Props {
  task: Task;
  running: boolean;
  controller: TaskPlanController;
  subagents: readonly ActivitySubagent[];
  onInspectSubagent: (subagentId: string) => void;
  onTaskChanged?: () => Promise<void> | void;
}

const MAX_PARALLEL_SUBAGENTS = 3;
const ACTIVE_SUBAGENT_STATES = new Set<SubagentStatus>([
  "queued",
  "running",
  "waiting_permission",
]);

const PLAN_STATE_LABEL: Record<PlanState, string> = {
  draft: "草拟中",
  awaiting_input: "需要你确认",
  ready: "等待确认实施",
  approved: "已确认",
  executing: "实施中",
  completed: "已完成",
  cancelled: "已取消",
};

const ITEM_STATE_LABEL: Record<PlanItem["state"], string> = {
  proposed: "待确认",
  pending: "等待依赖",
  in_progress: "进行中",
  blocked: "已阻塞",
  completed: "已完成",
  failed: "失败",
  cancelled: "已取消",
};

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function newIdempotencyKey(): string {
  return globalThis.crypto?.randomUUID?.()
    ?? `plan-answer-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

interface PlanProgress {
  completed: number;
  total: number;
  inProgress: number;
  pending: number;
  blocked: number;
  failed: number;
}

const PlanDescription = memo(function PlanDescription({
  description,
  taskId,
  workspacePath,
}: {
  description: string;
  taskId: string;
  workspacePath: string | null;
}) {
  const markdown = useMemo(
    () => formatPlanDescriptionMarkdown(description),
    [description],
  );
  return (
    <div className="plan-feature-description">
      <Markdown text={markdown} taskId={taskId} workspacePath={workspacePath} />
    </div>
  );
});

/**
 * Agent runs do not yet persist a Plan item id. The active feature's start time is therefore the
 * only honest association boundary: children delegated before it belong to earlier work and are
 * deliberately excluded. Active children remain visible if an older database lacks started_at.
 */
export function planSubagentsForItem(
  item: PlanItem,
  subagents: readonly ActivitySubagent[],
): ActivitySubagent[] {
  if (item.state !== "in_progress") return [];
  const itemStartedAt = item.started_at ? Date.parse(item.started_at) : Number.NaN;
  return subagents
    .filter((child) => (
      Number.isFinite(itemStartedAt)
        ? child.startedAt >= itemStartedAt
        : ACTIVE_SUBAGENT_STATES.has(child.status)
    ))
    .sort((left, right) => (
      planSubagentStatusOrder(left.status) - planSubagentStatusOrder(right.status)
      || right.startedAt - left.startedAt
      || left.id.localeCompare(right.id)
    ));
}

function PlanSubagentCluster({
  item,
  subagents,
  allSubagents,
  onInspect,
}: {
  item: PlanItem;
  subagents: readonly ActivitySubagent[];
  allSubagents: readonly ActivitySubagent[];
  onInspect: (subagentId: string) => void;
}) {
  const statusSignature = subagents.map((child) => `${child.id}:${child.status}`).join("|");
  const shouldAutoExpand = subagents.some((child) => child.status !== "completed");
  const [expanded, setExpanded] = useState(shouldAutoExpand);
  const previousAutoExpand = useRef(shouldAutoExpand);
  const liveCount = subagents.filter((child) => ACTIVE_SUBAGENT_STATES.has(child.status)).length;
  const completedCount = subagents.filter((child) => child.status === "completed").length;
  const hasLiveChildren = liveCount > 0;
  const now = useSharedNow(hasLiveChildren ? 1_000 : 60_000);
  const rowsId = `plan-item-${item.id.replace(/[^a-zA-Z0-9_-]/g, "-")}-subagents`;

  useEffect(() => {
    if (shouldAutoExpand && !previousAutoExpand.current) setExpanded(true);
    if (!shouldAutoExpand && previousAutoExpand.current) setExpanded(false);
    previousAutoExpand.current = shouldAutoExpand;
  }, [shouldAutoExpand, statusSignature]);

  if (subagents.length === 0) return null;

  const summary = liveCount > 0
    ? `并行执行 ${liveCount}/${MAX_PARALLEL_SUBAGENTS}`
    : completedCount === subagents.length
      ? `并行执行已完成 · ${completedCount}`
      : `并行执行需处理 · ${subagents.length - completedCount}`;

  return (
    <section
      className={`plan-subagent-cluster${expanded ? " is-expanded" : " is-collapsed"}`}
      aria-label={`功能「${item.title}」的并行子代理`}
      data-testid="plan-subagent-cluster"
    >
      <button
        type="button"
        className="plan-subagent-toggle"
        aria-expanded={expanded}
        aria-controls={rowsId}
        aria-label={`${expanded ? "收起" : "展开"}${summary}`}
        onClick={() => setExpanded((value) => !value)}
      >
        <span className={`plan-subagent-pulse${hasLiveChildren ? " is-live" : ""}`} aria-hidden="true" />
        <span className="plan-subagent-summary">
          <strong>{summary}</strong>
          <small>点击任务可打开对应的子代理会话</small>
        </span>
        <IconChevronDown width={13} height={13} aria-hidden="true" />
      </button>
      <div className="plan-subagent-rows" id={rowsId} hidden={!expanded}>
        {subagents.map((child) => {
          const childIndex = Math.max(0, allSubagents.findIndex((candidate) => candidate.id === child.id));
          return (
            <button
              type="button"
              className={`plan-subagent-row status-${child.status}`}
              key={child.id}
              data-subagent-id={child.id}
              aria-label={`${child.label}，${planSubagentStatusLabel(child.status)}，打开会话`}
              onClick={() => onInspect(child.id)}
            >
              <SubagentAvatar
                index={childIndex}
                identity={child.id}
                runtimeKind={child.runtimeKind}
                size="sm"
                className={`plan-subagent-avatar status-${child.status}`}
              />
              <span className="plan-subagent-copy">
                <span>
                  <strong title={child.label}>{child.label}</strong>
                  <em>{planSubagentStatusLabel(child.status)}</em>
                </span>
                <small title={planSubagentObservation(child)}>{planSubagentObservation(child)}</small>
              </span>
              <time title={planSubagentTimeTitle(child, now)}>
                {planSubagentTime(child, now)}
              </time>
              <IconChevronRight className="plan-subagent-open" width={13} height={13} aria-hidden="true" />
            </button>
          );
        })}
      </div>
    </section>
  );
}

function planSubagentStatusOrder(status: SubagentStatus): number {
  switch (status) {
    case "waiting_permission": return 0;
    case "running": return 1;
    case "queued": return 2;
    case "failed": return 3;
    case "cancelled": return 4;
    case "completed": return 5;
  }
}

function planSubagentStatusLabel(status: SubagentStatus): string {
  switch (status) {
    case "queued": return "等待中";
    case "running": return "进行中";
    case "waiting_permission": return "等待权限";
    case "completed": return "已完成";
    case "failed": return "失败";
    case "cancelled": return "已停止";
  }
}

function planSubagentObservation(child: ActivitySubagent): string {
  const event = child.events[child.events.length - 1];
  const fallback = event?.detail?.trim()
    ? `${event.label} · ${event.detail.trim()}`
    : event?.label;
  const value = (child.detail?.trim() || fallback || planSubagentStatusLabel(child.status))
    .replace(/\s+/g, " ");
  if (value.startsWith("{") || value.startsWith("[")) {
    return `${planSubagentStatusLabel(child.status)}，打开会话查看结构化结果`;
  }
  return value.length > 140 ? `${value.slice(0, 139).trimEnd()}…` : value;
}

function planSubagentTime(child: ActivitySubagent, now: number): string {
  const end = ACTIVE_SUBAGENT_STATES.has(child.status)
    ? now
    : (child.endedAt ?? child.lastEventAt);
  return elapsedCompact(child.startedAt, end);
}

function planSubagentTimeTitle(child: ActivitySubagent, now: number): string {
  const end = ACTIVE_SUBAGENT_STATES.has(child.status)
    ? now
    : (child.endedAt ?? child.lastEventAt);
  const duration = elapsedCompact(child.startedAt, end);
  return ACTIVE_SUBAGENT_STATES.has(child.status)
    ? `已运行 ${duration}`
    : `${planSubagentStatusLabel(child.status)} · 总耗时 ${duration}`;
}

function elapsedCompact(startedAt: number, endedAt: number): string {
  const seconds = Math.max(0, Math.floor((endedAt - startedAt) / 1_000));
  const minutes = Math.floor(seconds / 60);
  return minutes > 0 ? `${minutes}m ${String(seconds % 60).padStart(2, "0")}s` : `${seconds}s`;
}

function featureProgress(items: readonly PlanItem[]): PlanProgress {
  return {
    completed: items.filter((item) => item.state === "completed").length,
    total: items.length,
    inProgress: items.filter((item) => item.state === "in_progress").length,
    pending: items.filter((item) => ["proposed", "pending"].includes(item.state)).length,
    blocked: items.filter((item) => item.state === "blocked").length,
    failed: items.filter((item) => item.state === "failed").length,
  };
}

type OutlineNode =
  | { kind: "section"; key: string; title: string; children: OutlineNode[] }
  | { kind: "item"; key: string; item: PlanItem };

type OutlineEntry =
  | { kind: "section"; key: string; title: string; number: string; depth: number; ancestors: string[]; progress: PlanProgress }
  | { kind: "item"; key: string; item: PlanItem; number: string; depth: number; ancestors: string[] };

export function planOutline(items: readonly PlanItem[]): OutlineEntry[] {
  const root: OutlineNode[] = [];
  let sectionSequence = 0;
  for (const item of items) {
    let children = root;
    const path: string[] = Array.isArray(item.section_path)
      ? item.section_path.map((segment) => segment.trim()).filter(Boolean)
      : [];
    const keyPath: string[] = [];
    for (const segment of path) {
      keyPath.push(segment);
      const last = children[children.length - 1];
      let section = last?.kind === "section" && last.title === segment ? last : undefined;
      if (!section) {
        sectionSequence += 1;
        section = { kind: "section", key: `section:${sectionSequence}:${JSON.stringify(keyPath)}`, title: segment, children: [] };
        children.push(section);
      }
      children = section.children;
    }
    children.push({ kind: "item", key: `item:${item.id}`, item });
  }

  const descendantItems = (nodes: readonly OutlineNode[]): PlanItem[] => nodes.flatMap((node) => (
    node.kind === "item" ? [node.item] : descendantItems(node.children)
  ));
  const flattened: OutlineEntry[] = [];
  const visit = (nodes: readonly OutlineNode[], prefix: number[], depth: number, ancestors: string[]) => {
    nodes.forEach((node, index) => {
      const numberPath = [...prefix, index + 1];
      const number = numberPath.join(".");
      if (node.kind === "section") {
        flattened.push({
          kind: "section",
          key: node.key,
          title: node.title,
          number,
          depth,
          ancestors,
          progress: featureProgress(descendantItems(node.children)),
        });
        visit(node.children, numberPath, depth + 1, [...ancestors, node.key]);
      } else {
        flattened.push({ kind: "item", key: node.key, item: node.item, number, depth, ancestors });
      }
    });
  };
  visit(root, [], 0, []);
  return flattened;
}

export function PlanPanel({
  task,
  running,
  controller,
  subagents,
  onInspectSubagent,
  onTaskChanged,
}: Props) {
  const {
    view,
    loaded,
    loadError,
    setView,
    refresh,
    clearLoadError,
  } = controller;
  const [busy, setBusy] = useState<
    "create" | "answer" | "skip" | "retry" | "approve" | "repair" | "retryImplementation" | "cancel" | null
  >(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [cancelArmedRevision, setCancelArmedRevision] = useState<number | null>(null);
  const [answers, setAnswers] = useState<Record<string, AnswerDraft>>({});
  const [retryQuestionSetId, setRetryQuestionSetId] = useState<string | null>(null);
  const [planCollapsed, setPlanCollapsed] = useState(false);
  const [collapsedSections, setCollapsedSections] = useState<Set<string>>(() => new Set());
  const [expandedItems, setExpandedItems] = useState<Set<string>>(() => new Set());
  const currentQuestionSetId = view?.pending_question_set?.id ?? null;
  const answerKeys = useRef(new Map<string, string>());
  useEffect(() => {
    answerKeys.current.clear();
    setAnswers({});
    setRetryQuestionSetId(null);
    setCancelArmedRevision(null);
    setPlanCollapsed(false);
    setCollapsedSections(new Set());
    setExpandedItems(new Set());
    setNotice(null);
    setError(null);
  }, [task.id]);

  useEffect(() => {
    if (!currentQuestionSetId) return;
    setAnswers({});
    setNotice(null);
  }, [currentQuestionSetId]);

  const progress = useMemo(() => featureProgress(view?.items ?? []), [view?.items]);
  const outline = useMemo(() => planOutline(view?.items ?? []), [view?.items]);
  const visibleOutline = useMemo(
    () => outline.filter((entry) => entry.ancestors.every((key) => !collapsedSections.has(key))),
    [collapsedSections, outline],
  );
  const itemTitles = useMemo(
    () => new Map((view?.items ?? []).map((item) => [item.id, item.title])),
    [view?.items],
  );
  const cancelArmed = view != null && cancelArmedRevision === view.plan.revision;
  const panelBodyId = `plan-panel-${task.id.replace(/[^a-zA-Z0-9_-]/g, "-")}-body`;

  const toggleSection = (key: string) => {
    setCollapsedSections((current) => {
      const next = new Set(current);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const toggleItem = (itemId: string) => {
    setExpandedItems((current) => {
      const next = new Set(current);
      if (next.has(itemId)) next.delete(itemId);
      else next.add(itemId);
      return next;
    });
  };

  const initialize = async () => {
    if (busy) return;
    setBusy("create");
    setError(null);
    try {
      const next = await planCreate(task.id);
      setView(next);
    } catch (cause) {
      setError(`初始化计划失败：${errorText(cause)}`);
    } finally {
      setBusy(null);
    }
  };

  const answerSet = async (questionSet: PlanQuestionSet, skipAll: boolean) => {
    if (busy || !view) return;
    let payload: PlanQuestionAnswerInput[] = [];
    if (!skipAll) {
      const missing = questionSet.questions.filter((question) => {
        const draft = answers[question.id];
        return !draft || (draft.kind === "text" && !draft.text.trim());
      });
      if (missing.length > 0) {
        setError(`请逐项回答：${missing.map((question) => question.header).join("、")}；也可以跳过整组。`);
        return;
      }
      payload = questionSet.questions.map((question) => {
        const draft = answers[question.id]!;
        return draft.kind === "option"
          ? { kind: "option", question_id: question.id, option_id: draft.optionId }
          : { kind: "text", question_id: question.id, text: draft.text.trim() };
      });
    }

    const operation = skipAll ? "skip" : "answer";
    setBusy(operation);
    setError(null);
    setNotice(null);
    const key = answerKeys.current.get(questionSet.id) ?? newIdempotencyKey();
    answerKeys.current.set(questionSet.id, key);
    try {
      const next = await planAnswer(task.id, {
        question_set_id: questionSet.id,
        expected_revision: questionSet.revision,
        idempotency_key: key,
        skip_all: skipAll,
        answers: payload,
      });
      setView(next);
      setRetryQuestionSetId(null);
      setNotice(null);
      await onTaskChanged?.();
    } catch (cause) {
      const message = errorText(cause);
      setError(`提交计划回答失败：${message}`);
      setNotice(null);
      if (/续接|continuation|dispatch/i.test(message)) setRetryQuestionSetId(questionSet.id);
    } finally {
      setBusy(null);
    }
  };

  const retryContinuation = async (questionSetId: string) => {
    if (busy) return;
    setBusy("retry");
    setError(null);
    try {
      setView(await planRetryContinuation(task.id, questionSetId));
      setRetryQuestionSetId(null);
      setNotice("已重新请求续接计划。");
    } catch (cause) {
      setError(`重试计划续接失败：${errorText(cause)}`);
    } finally {
      setBusy(null);
    }
  };

  const approve = async () => {
    if (!view || busy) return;
    setBusy("approve");
    setError(null);
    try {
      const next = await planApprove(task.id, view.plan.id, view.plan.revision);
      setView(next);
      setNotice(null);
      await onTaskChanged?.();
    } catch (cause) {
      setError(`确认实施失败：${errorText(cause)}`);
      await refresh().catch(() => undefined);
    } finally {
      setBusy(null);
    }
  };

  const repairProjection = async () => {
    if (!view || busy) return;
    setBusy("repair");
    setError(null);
    try {
      setView(await planRepairProjection(task.id, view.plan.id));
      setNotice("计划文档已重新同步。");
    } catch (cause) {
      setError(`修复计划文档失败：${errorText(cause)}`);
    } finally {
      setBusy(null);
    }
  };

  const retryImplementation = async () => {
    if (!view || busy) return;
    setBusy("retryImplementation");
    setError(null);
    try {
      const next = await planRetryImplementation(task.id, view.plan.id);
      setView(next);
      setNotice("实施任务已重新加入可靠队列。即使应用重启，也会从这里继续。");
      await onTaskChanged?.();
    } catch (cause) {
      setError(`重试实施派发失败：${errorText(cause)}`);
      await refresh().catch(() => undefined);
    } finally {
      setBusy(null);
    }
  };

  const prepareCancel = async () => {
    if (!view || busy || running || cancelArmed) return;
    setBusy("cancel");
    setError(null);
    setNotice(null);
    try {
      // Refresh before arming the destructive action. Plan item/review activity may have
      // advanced the revision since the last poll; confirming against that stale snapshot
      // would otherwise force the user through an avoidable conflict round-trip.
      const latest = await refresh();
      if (!latest) throw new Error("当前计划已不存在");
      setCancelArmedRevision(latest.plan.revision);
    } catch {
      setError("暂时无法取消计划，请稍后再试。");
    } finally {
      setBusy(null);
    }
  };

  const cancel = async () => {
    if (!view || busy || running || !cancelArmed) return;
    setBusy("cancel");
    setError(null);
    try {
      const next = await planCancel(task.id, view.plan.id, view.plan.revision);
      setView(next);
      setCancelArmedRevision(null);
      setNotice(null);
      await onTaskChanged?.();
    } catch (cause) {
      setError(`取消计划失败：${errorText(cause)}`);
      await refresh().catch(() => undefined);
      setCancelArmedRevision(null);
    } finally {
      setBusy(null);
    }
  };

  if (!loaded && !view) {
    return <div className="plan-panel plan-panel-loading" role="status">正在读取当前对话的计划…</div>;
  }

  if (!view) {
    return (
      <section className="plan-panel plan-panel-empty" aria-label="当前计划" data-task-id={task.id}>
        <span className="plan-empty-icon"><IconHelp width={18} height={18} /></span>
        <div>
          <strong>当前对话没有计划</strong>
          <p>进入计划模式后，步骤和需要你确认的问题会显示在这里。</p>
        </div>
        {task.mode === "plan" && (
          <button className="btn" type="button" disabled={busy === "create"} onClick={() => void initialize()}>
            {busy === "create" ? "初始化中…" : "初始化计划"}
          </button>
        )}
        {loadError && <StatusBar kind="error" compact onDismiss={clearLoadError}>{loadError}</StatusBar>}
        {error && <StatusBar kind="error" compact onDismiss={() => setError(null)}>{error}</StatusBar>}
      </section>
    );
  }

  const questionSet = view.pending_question_set;
  const continuationSet = view.continuation_question_set;
  const implementationReady = view.plan.state === "ready" && view.items.length > 0;
  const cancellable = !["completed", "cancelled"].includes(view.plan.state);
  const progressPercent = progress.total > 0 ? Math.round(progress.completed / progress.total * 100) : 0;

  return (
    <section className={`plan-panel state-${view.plan.state}`} aria-label="当前计划" data-task-id={task.id}>
      <header className={`plan-panel-summary${planCollapsed ? " is-collapsed" : ""}`}>
        <button
          type="button"
          className="plan-summary-toggle"
          aria-expanded={!planCollapsed}
          aria-controls={panelBodyId}
          aria-label={`${planCollapsed ? "展开" : "收起"}计划`}
          onClick={() => setPlanCollapsed((collapsed) => !collapsed)}
        >
          <span className="plan-summary-chevron" aria-hidden="true">
            {planCollapsed ? <IconChevronRight width={14} height={14} /> : <IconChevronDown width={14} height={14} />}
          </span>
          <span className="plan-state-diamond" aria-hidden="true" />
          <span className="plan-summary-copy">
            <strong>计划</strong>
            <small>{PLAN_STATE_LABEL[view.plan.state]} · 修订 {view.plan.revision}</small>
          </span>
          {progress.total > 0 && (
            <span className="plan-summary-progress">{progress.completed}/{progress.total} · {progressPercent}%</span>
          )}
        </button>
        {running && <span className="plan-runtime-state">Agent 运行中</span>}
        <button type="button" className="iconbtn" aria-label="刷新计划" title="刷新计划" onClick={() => void refresh()}>
          <IconRefresh width={13} height={13} />
        </button>
      </header>

      <div className="plan-panel-body" id={panelBodyId} hidden={planCollapsed}>
          <div className="plan-metadata">
            <span title={view.plan.projection_path ?? "计划文档尚未生成"}>
              文档 · {view.plan.projection_path ?? "准备中"}
            </span>
            <span>同步修订 · {view.plan.projection_revision ?? "—"}</span>
          </div>

          {view.goal.goal && (
            <p className="plan-goal"><span>目标</span>{view.goal.goal}</p>
          )}

          {view.plan.projection_error && (
            <StatusBar
              kind="warn"
              compact
              action={{ label: busy === "repair" ? "同步中…" : "重新同步", onClick: () => void repairProjection(), disabled: busy === "repair" }}
            >
              计划正文同步失败：{view.plan.projection_error}
            </StatusBar>
          )}
          {loadError && <StatusBar kind="error" compact onDismiss={clearLoadError}>{loadError}</StatusBar>}
          {error && <StatusBar kind="error" compact onDismiss={() => setError(null)}>{error}</StatusBar>}
          {notice && <StatusBar kind="info" compact onDismiss={() => setNotice(null)}>{notice}</StatusBar>}
          {(task.state === "interrupted" || task.state === "review_ready") && progress.inProgress > 0 && !running && (
            <StatusBar
              kind="warn"
              compact
              action={{
                label: busy === "retryImplementation" ? "续接中…" : "继续当前功能",
                onClick: () => void retryImplementation(),
                disabled: busy != null,
              }}
            >
              上一轮已停止或留下部分成果，但当前功能仍标记为进行中。续接会沿用同一计划和当前进度，不会重新建立 0/{progress.total} 的任务列表。
            </StatusBar>
          )}

          {questionSet && (
            <div className="plan-hitl" role="group" aria-label="计划需要你的回答">
              <header>
                <IconHelp width={16} height={16} />
                <div>
                  <strong>需要你确认 {questionSet.questions.length} 个问题</strong>
                  <p>每个问题单独作答；选项和自定义回答不会串到其他问题。</p>
                </div>
              </header>
              <div className="plan-question-list">
                {questionSet.questions.map((question, questionIndex) => {
                  const draft = answers[question.id];
                  return (
                    <fieldset className="plan-question" key={question.id}>
                      <legend>
                        <span>{questionIndex + 1}</span>
                        <span><strong>{question.header}</strong><small>{question.question}</small></span>
                      </legend>
                      <div className="plan-question-options">
                        {question.options.map((option) => {
                          const checked = draft?.kind === "option" && draft.optionId === option.id;
                          return (
                            <label className={checked ? "is-selected" : ""} key={option.id}>
                              <input
                                type="radio"
                                name={`plan-question-${question.id}`}
                                checked={checked}
                                onChange={() => setAnswers((current) => ({
                                  ...current,
                                  [question.id]: { kind: "option", optionId: option.id },
                                }))}
                              />
                              <span><strong>{option.label}</strong><small>{option.description}</small></span>
                            </label>
                          );
                        })}
                        <label className={`plan-question-custom${draft?.kind === "text" ? " is-selected" : ""}`}>
                          <input
                            type="radio"
                            name={`plan-question-${question.id}`}
                            checked={draft?.kind === "text"}
                            onChange={() => setAnswers((current) => {
                              const existing = current[question.id];
                              return {
                                ...current,
                                [question.id]: { kind: "text", text: existing?.kind === "text" ? existing.text : "" },
                              };
                            })}
                          />
                          <input
                            type="text"
                            aria-label={`${question.header}的自定义回答`}
                            value={draft?.kind === "text" ? draft.text : ""}
                            placeholder="自定义回答…"
                            onFocus={() => setAnswers((current) => {
                              const existing = current[question.id];
                              return {
                                ...current,
                                [question.id]: { kind: "text", text: existing?.kind === "text" ? existing.text : "" },
                              };
                            })}
                            onChange={(event) => setAnswers((current) => ({
                              ...current,
                              [question.id]: { kind: "text", text: event.target.value },
                            }))}
                          />
                        </label>
                      </div>
                    </fieldset>
                  );
                })}
              </div>
              <footer>
                <button type="button" className="quiet-link" disabled={busy != null} onClick={() => void answerSet(questionSet, true)}>
                  {busy === "skip" ? "跳过中…" : "跳过整组"}
                </button>
                <button type="button" className="btn accent" disabled={busy != null} onClick={() => void answerSet(questionSet, false)}>
                  {busy === "answer" ? "提交中…" : "提交回答"}
                </button>
              </footer>
            </div>
          )}

          {continuationSet && ["pending", "dispatching"].includes(continuationSet.continuation_state) && (
            <StatusBar kind="info" compact>
              已收到你的回答，正在续接同一份计划；可以继续查看对话，无需重复提交。
            </StatusBar>
          )}

          {continuationSet?.continuation_state === "failed" && (
            <StatusBar
              kind="error"
              compact
              action={{
                label: busy === "retry" ? "重试中…" : "重试续接",
                onClick: () => void retryContinuation(continuationSet.id),
                disabled: busy != null,
              }}
            >
              计划续接失败：{continuationSet.continuation_error ?? "运行未能继续，但你的回答已经安全保存。"}
            </StatusBar>
          )}

          {["pending", "dispatching"].includes(view.plan.implementation_dispatch_state) && (
            <StatusBar kind="info" compact>
              已确认计划，正在把实施事项写入可靠队列。完成前不会重复启动同一份计划。
            </StatusBar>
          )}

          {view.plan.implementation_dispatch_state === "failed" && (
            <StatusBar
              kind="error"
              compact
              action={{
                label: busy === "retryImplementation" ? "重试中…" : "重试实施",
                onClick: () => void retryImplementation(),
                disabled: busy != null || running,
              }}
            >
              实施任务尚未启动：{view.plan.implementation_dispatch_error ?? "可靠队列派发失败，计划内容仍已保存。"}
            </StatusBar>
          )}

          {retryQuestionSetId && continuationSet?.continuation_state !== "failed" && (
            <button className="plan-retry" type="button" disabled={busy != null} onClick={() => void retryContinuation(retryQuestionSetId)}>
              <IconRefresh width={13} height={13} />
              {busy === "retry" ? "正在重试续接…" : "重试计划续接"}
            </button>
          )}

          {view.items.length > 0 && (
            <div className="plan-feature-section">
              <div className="plan-feature-head">
                <div>
                  <strong>功能事项</strong>
                  <small>按可独立验收的功能拆分，不按文件拆分</small>
                </div>
                <span>{progress.completed}/{progress.total}</span>
              </div>
              <div className="plan-progress" role="progressbar" aria-valuemin={0} aria-valuemax={progress.total} aria-valuenow={progress.completed}>
                <span style={{ width: `${progressPercent}%` }} />
              </div>
              <div className="plan-progress-breakdown" aria-label="计划进度明细">
                <span className="is-completed">完成 {progress.completed}</span>
                <span className="is-active">进行中 {progress.inProgress}</span>
                <span>待处理 {progress.pending}</span>
                {progress.blocked > 0 && <span className="is-blocked">阻塞 {progress.blocked}</span>}
                {progress.failed > 0 && <span className="is-failed">失败 {progress.failed}</span>}
              </div>
              <ol className="plan-feature-list">
                {visibleOutline.map((entry) => {
                  if (entry.kind === "section") {
                    const collapsed = collapsedSections.has(entry.key);
                    return (
                      <li
                        className={`plan-outline-section${collapsed ? " is-collapsed" : ""}`}
                        key={entry.key}
                        style={{ paddingInlineStart: `${entry.depth * 14}px` }}
                      >
                        <button
                          type="button"
                          className="plan-outline-toggle"
                          aria-expanded={!collapsed}
                          aria-label={`${collapsed ? "展开" : "收起"}阶段 ${entry.number}：${entry.title}`}
                          onClick={() => toggleSection(entry.key)}
                        >
                          <span className="plan-row-chevron" aria-hidden="true">
                            {collapsed ? <IconChevronRight width={13} height={13} /> : <IconChevronDown width={13} height={13} />}
                          </span>
                          <span className="plan-feature-index" aria-label={`阶段 ${entry.number}`}>{entry.number}</span>
                          <span className="plan-feature-copy">
                            <span><strong>{entry.title}</strong><em>{entry.progress.completed}/{entry.progress.total}</em></span>
                          </span>
                        </button>
                      </li>
                    );
                  }

                  const expanded = expandedItems.has(entry.item.id);
                  const itemSubagents = planSubagentsForItem(entry.item, subagents);
                  const detailsId = `plan-item-${entry.item.id.replace(/[^a-zA-Z0-9_-]/g, "-")}-details`;
                  return (
                    <li
                      className={`plan-feature-item state-${entry.item.state}${expanded ? " is-expanded" : ""}`}
                      key={entry.key}
                      style={{ paddingInlineStart: `${entry.depth * 14}px` }}
                    >
                      <button
                        type="button"
                        className="plan-feature-toggle"
                        aria-expanded={expanded}
                        aria-controls={detailsId}
                        aria-label={`${expanded ? "收起" : "展开"}功能 ${entry.number}：${entry.item.title}`}
                        onClick={() => toggleItem(entry.item.id)}
                      >
                        <span className="plan-row-chevron" aria-hidden="true">
                          {expanded ? <IconChevronDown width={13} height={13} /> : <IconChevronRight width={13} height={13} />}
                        </span>
                        <span className="plan-feature-index" aria-label={`第 ${entry.number} 步`}>{entry.number}</span>
                        <span className="plan-feature-copy">
                          <span><strong>{entry.item.title}</strong><em>{ITEM_STATE_LABEL[entry.item.state]}</em></span>
                        </span>
                        {entry.item.state === "completed" && <IconCheck width={14} height={14} aria-label="已完成" />}
                      </button>
                      {itemSubagents.length > 0 && (
                        <PlanSubagentCluster
                          item={entry.item}
                          subagents={itemSubagents}
                          allSubagents={subagents}
                          onInspect={onInspectSubagent}
                        />
                      )}
                      <div className="plan-feature-details" id={detailsId} hidden={!expanded}>
                        {entry.item.depends_on.length > 0 && (
                          <small className="plan-feature-dependencies">
                            依赖：{entry.item.depends_on.map((id) => itemTitles.get(id) ?? id).join("、")}
                          </small>
                        )}
                        {entry.item.description && (
                          <PlanDescription
                            description={entry.item.description}
                            taskId={task.id}
                            workspacePath={task.workspace_path}
                          />
                        )}
                      </div>
                    </li>
                  );
                })}
              </ol>
            </div>
          )}

          {(implementationReady || cancellable) && (
            <div className={`plan-decision-bar${cancelArmed ? " is-canceling" : ""}`}>
              {cancelArmed && <span className="plan-decision-prompt" role="status">取消当前计划？</span>}
              <span className="plan-decision-actions">
                {cancellable && (
                  <button
                    className="btn plan-cancel-action"
                    type="button"
                    disabled={busy != null || running}
                    aria-busy={busy === "cancel" && !cancelArmed}
                    title={running
                      ? "请先停止或等待当前运行结束"
                      : cancelArmed
                        ? "返回计划"
                        : "取消当前计划"}
                    onClick={() => {
                      if (cancelArmed) setCancelArmedRevision(null);
                      else void prepareCancel();
                    }}
                  >
                    {busy === "cancel" && !cancelArmed && <span className="plan-action-spinner" aria-hidden="true" />}
                    取消
                  </button>
                )}
                {(implementationReady || cancelArmed) && (
                  <button
                    className={`btn ${cancelArmed ? "danger" : "accent"}`}
                    type="button"
                    disabled={busy != null || running}
                    aria-busy={busy === "approve" || (busy === "cancel" && cancelArmed)}
                    title={running
                      ? "请等待当前 Plan 运行结束"
                      : cancelArmed
                        ? "确认取消当前计划"
                        : "确认并开始实施"}
                    onClick={() => void (cancelArmed ? cancel() : approve())}
                  >
                    {(busy === "approve" || (busy === "cancel" && cancelArmed)) && (
                      <span className="plan-action-spinner" aria-hidden="true" />
                    )}
                    确认
                  </button>
                )}
              </span>
            </div>
          )}
      </div>
    </section>
  );
}

interface PlanShortcutProps {
  taskMode: Task["mode"];
  controller: TaskPlanController;
  onOpen: () => void;
}

/** 对话区只保留轻量入口，完整内容统一进入任务级右侧工作台。 */
export function PlanShortcut({ taskMode, controller, onOpen }: PlanShortcutProps) {
  const { view, loaded } = controller;
  if (taskMode !== "plan" && !view) return null;
  const progress = featureProgress(view?.items ?? []);
  const label = view ? PLAN_STATE_LABEL[view.plan.state] : loaded ? "尚未建立" : "读取中";

  return (
    <button
      type="button"
      className={`room-plan-shortcut state-${view?.plan.state ?? "draft"}`}
      onClick={onOpen}
      aria-label={`打开计划，${label}`}
    >
      <span className="plan-state-diamond" aria-hidden="true" />
      <strong>计划</strong>
      <small>{label}{progress.total > 0 ? ` · ${progress.completed}/${progress.total}` : ""}</small>
    </button>
  );
}
