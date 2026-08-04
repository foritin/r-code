import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  planReviewAcceptFeature,
  planReviewAcceptFile,
  planReviewRejectFeature,
  planReviewRejectFile,
  planReviewStatus,
  REVIEW_STATUS_CHANGED_EVENT,
} from "../../lib/ipc";
import { usePoll } from "../../lib/poll";
import { IconChevronDown, IconChevronRight } from "../icons";
import type {
  EnhancedReviewGroupView,
  EnhancedReviewTarget,
  EnhancedReviewView,
  PlanItemState,
  PlanReviewDecisionKind,
} from "../../lib/types";

interface Props {
  taskId: string;
  running: boolean;
  onVisibleCountChange: (count: number) => void;
}

const TERMINAL_STATES = new Set<PlanItemState>(["completed", "failed", "cancelled"]);

interface ActionFailure {
  key: string;
  message: string;
  detail: string;
}

const STATE_LABEL: Record<PlanItemState, string> = {
  proposed: "待批准",
  pending: "等待中",
  in_progress: "实施中",
  blocked: "受阻",
  completed: "已完成",
  failed: "失败",
  cancelled: "已取消",
};

function stableTone(itemId: string): number {
  let hash = 0;
  for (let index = 0; index < itemId.length; index += 1) hash = (hash * 31 + itemId.charCodeAt(index)) | 0;
  return Math.abs(hash) % 6;
}

function operationKey(itemId: string, path: string | null): string {
  return path == null ? `feature:${itemId}` : `file:${itemId}:${path}`;
}

function targetOf(view: EnhancedReviewView, itemId: string, path: string | null): EnhancedReviewTarget {
  return {
    task_id: view.task_id,
    plan_id: view.plan_id,
    plan_revision: view.plan_revision,
    item_id: itemId,
    path,
  };
}

function decisionLabel(decision: PlanReviewDecisionKind): string {
  return decision === "accepted" ? "已接受" : "已拒绝";
}

function isGroupResolved(group: EnhancedReviewGroupView): boolean {
  return group.decision != null
    || (group.files.length > 0 && group.files.every((file) => file.decision != null));
}

function actionFailure(
  key: string,
  cause: unknown,
  path: string | null,
  decision: PlanReviewDecisionKind,
): ActionFailure {
  const detail = String(cause).replace(/^Error:\s*/i, "");
  const rollbackConflict = decision === "rejected"
    && /rejection conflicts|changed (?:while|externally).*rejection|external change prevents rollback/i.test(detail);
  if (rollbackConflict) {
    return {
      key,
      detail,
      message: `${path ? `文件 ${path} 未拒绝` : "未拒绝整组"}：检测到捕获后的重叠修改。为避免覆盖后续内容，R-Code 已安全停止回滚；请先审阅或保存后续修改，再重试。`,
    };
  }
  return {
    key,
    detail,
    message: `${path ? `文件 ${path}` : "整组"}${decision === "accepted" ? "接受" : "拒绝"}失败：${detail}`,
  };
}

function patchLineKind(line: string): string {
  if (line.startsWith("@@")) return "hunk";
  if (line.startsWith("+") && !line.startsWith("+++")) return "add";
  if (line.startsWith("-") && !line.startsWith("---")) return "del";
  if (line.startsWith("diff ") || line.startsWith("index ") || line.startsWith("---") || line.startsWith("+++")) return "meta";
  return "ctx";
}

function optimisticDecision(
  current: EnhancedReviewView | null,
  itemId: string,
  path: string | null,
  decision: PlanReviewDecisionKind,
): EnhancedReviewView | null {
  if (!current) return current;
  return {
    ...current,
    groups: current.groups.map((group) => {
      if (group.item_id !== itemId) return group;
      if (path == null) {
        return {
          ...group,
          decision,
          files: group.files.map((file) => ({ ...file, decision })),
        };
      }
      return {
        ...group,
        files: group.files.map((file) => file.path === path ? { ...file, decision } : file),
      };
    }),
  };
}

export function EnhancedReviewPanel({ taskId, running, onVisibleCountChange }: Props) {
  const [view, setView] = useState<EnhancedReviewView | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshError, setRefreshError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<ActionFailure | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [pending, setPending] = useState<Set<string>>(new Set());
  const [confirmRejects, setConfirmRejects] = useState<Set<string>>(new Set());
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(new Set());
  const pendingRef = useRef<Set<string>>(new Set());
  const confirmTimersRef = useRef<Map<string, ReturnType<typeof setTimeout>>>(new Map());
  const refreshSequenceRef = useRef(0);
  const expansionPlanRef = useRef<string | null>(null);

  const refresh = useCallback(async () => {
    const sequence = ++refreshSequenceRef.current;
    try {
      const next = await planReviewStatus(taskId);
      if (sequence !== refreshSequenceRef.current) return;
      setView(next);
      setRefreshError(null);
    } catch (cause) {
      if (sequence === refreshSequenceRef.current) setRefreshError(`无法刷新增强审核：${String(cause)}`);
    } finally {
      if (sequence === refreshSequenceRef.current) setLoading(false);
    }
  }, [taskId]);

  usePoll(refresh, running ? 900 : 2000, true, "增强审核");

  useEffect(() => {
    const onReviewChanged = (event: Event) => {
      const detail = (event as CustomEvent<{ taskId?: string }>).detail;
      if (!detail?.taskId || detail.taskId === taskId) void refresh();
    };
    window.addEventListener(REVIEW_STATUS_CHANGED_EVENT, onReviewChanged);
    return () => window.removeEventListener(REVIEW_STATUS_CHANGED_EVENT, onReviewChanged);
  }, [refresh, taskId]);

  useEffect(() => () => {
    for (const timer of confirmTimersRef.current.values()) clearTimeout(timer);
    confirmTimersRef.current.clear();
  }, []);

  const visibleGroups = useMemo(
    () => view?.groups.filter((group) => !isGroupResolved(group)) ?? [],
    [view],
  );

  const unresolvedCount = useMemo(() => visibleGroups.reduce((total, group) => {
    return total + group.files.filter((file) => !file.decision).length;
  }, 0), [visibleGroups]);

  useEffect(() => {
    if (!view) {
      expansionPlanRef.current = null;
      setExpandedGroups(new Set());
      return;
    }
    const identity = `${view.plan_id}:${view.plan_revision}`;
    if (expansionPlanRef.current === identity) return;
    expansionPlanRef.current = identity;
    setExpandedGroups(visibleGroups[0] ? new Set([visibleGroups[0].item_id]) : new Set());
  }, [view, visibleGroups]);

  useEffect(() => onVisibleCountChange(unresolvedCount), [onVisibleCountChange, unresolvedCount]);

  const beginOperation = (key: string): boolean => {
    if (pendingRef.current.has(key)) return false;
    pendingRef.current.add(key);
    setPending(new Set(pendingRef.current));
    return true;
  };

  const finishOperation = (key: string) => {
    pendingRef.current.delete(key);
    setPending(new Set(pendingRef.current));
  };

  const clearConfirmation = (key: string) => {
    const timer = confirmTimersRef.current.get(key);
    if (timer) clearTimeout(timer);
    confirmTimersRef.current.delete(key);
    setConfirmRejects((current) => {
      const next = new Set(current);
      next.delete(key);
      return next;
    });
  };

  const armReject = (key: string): boolean => {
    if (confirmRejects.has(key)) {
      clearConfirmation(key);
      return true;
    }
    setConfirmRejects((current) => new Set(current).add(key));
    const previous = confirmTimersRef.current.get(key);
    if (previous) clearTimeout(previous);
    confirmTimersRef.current.set(key, setTimeout(() => clearConfirmation(key), 3500));
    return false;
  };

  const perform = async (
    group: EnhancedReviewGroupView,
    path: string | null,
    decision: PlanReviewDecisionKind,
  ) => {
    if (!view) return;
    const key = operationKey(group.item_id, path);
    if (decision === "rejected" && !armReject(key)) return;
    if (!beginOperation(key)) return;
    const snapshot = view;
    setActionError(null);
    setNotice(null);
    // Accept is a ledger-only decision and can update immediately. Rejection may need a
    // three-way filesystem rollback, so it stays visible until that operation really commits.
    if (decision === "accepted") {
      setView((current) => optimisticDecision(current, group.item_id, path, decision));
    }
    try {
      const target = targetOf(snapshot, group.item_id, path);
      if (decision === "accepted") {
        if (path == null) await planReviewAcceptFeature(target);
        else await planReviewAcceptFile(target);
      } else if (path == null) {
        await planReviewRejectFeature(target);
      } else {
        await planReviewRejectFile(target);
      }
      if (decision === "rejected") {
        setView((current) => optimisticDecision(current, group.item_id, path, decision));
      }
      setNotice(`${path ?? group.title} ${decision === "accepted" ? "已接受" : "已拒绝并恢复"}。`);
      void refresh();
    } catch (cause) {
      if (decision === "accepted") setView(snapshot);
      setActionError(actionFailure(key, cause, path, decision));
      void refresh();
    } finally {
      finishOperation(key);
    }
  };

  const groupBusy = (itemId: string) => [...pending].some((key) => key === operationKey(itemId, null) || key.startsWith(`file:${itemId}:`));
  const fileBusy = (itemId: string, path: string) => pending.has(operationKey(itemId, null)) || pending.has(operationKey(itemId, path));

  const toggleGroup = (itemId: string) => {
    setExpandedGroups((current) => {
      const next = new Set(current);
      if (next.has(itemId)) next.delete(itemId);
      else next.add(itemId);
      return next;
    });
  };

  if (loading && !view) return <div className="enhanced-review-empty">加载当前 Plan 的功能变更…</div>;

  return (
    <div className="enhanced-review" data-testid="enhanced-review">
      <header className="enhanced-review-intro">
        <span>
          <strong>功能点审核</strong>
          <small>仅显示当前 Plan 捕获的事件补丁；同一路径在不同功能点中独立决策。</small>
        </span>
        {view && <em>Plan r{view.plan_revision} · {unresolvedCount} 个文件待处理</em>}
      </header>
      {refreshError && <div className="panel-error" role="alert">{refreshError}</div>}
      {notice && <div className="panel-note" role="status">{notice}</div>}
      {!view || visibleGroups.length === 0 ? (
        <div className="enhanced-review-empty">
          <strong>{view?.groups.length ? "当前 Plan 的变更已全部处理" : "当前没有可增强审核的 Plan 变更"}</strong>
          <span>{view?.groups.length
            ? "已接受或拒绝的决定仍保存在审核账本中。"
            : "先在计划模式批准并实施功能点；普通模式仍可查看 Git 工作区变更。"}</span>
        </div>
      ) : (
        <div className="enhanced-review-groups">
          {visibleGroups.map((group) => {
            const terminal = TERMINAL_STATES.has(group.state);
            const featureKey = operationKey(group.item_id, null);
            const featureBusy = pending.has(featureKey);
            const busy = groupBusy(group.item_id);
            const expanded = expandedGroups.has(group.item_id);
            const bodyId = `enhanced-feature-${group.item_id.replace(/[^a-zA-Z0-9_-]/g, "-")}-body`;
            const hasFileDecision = group.files.some((file) => file.decision != null);
            const featureActionTitle = !terminal
              ? group.state === "blocked"
                ? "功能暂时受阻，恢复并完成或终止后才能审核"
                : "功能仍在实施"
              : hasFileDecision
                ? "已有文件级决定，不能再整组处理"
                : undefined;
            return (
              <section className={`enhanced-feature tone-${stableTone(group.item_id)}${expanded ? " expanded" : " collapsed"}`} key={group.item_id} data-item-id={group.item_id}>
                <header className="enhanced-feature-head">
                  <button
                    type="button"
                    className="enhanced-feature-toggle"
                    aria-expanded={expanded}
                    aria-controls={bodyId}
                    aria-label={`${expanded ? "收起" : "展开"}功能 ${group.ordinal}：${group.title}`}
                    onClick={() => toggleGroup(group.item_id)}
                  >
                    <i className="feature-diamond" aria-hidden="true" />
                    <span className="enhanced-feature-copy">
                      <span><b>功能 {group.ordinal}</b><strong>{group.title}</strong></span>
                      <small>{expanded
                        ? group.description
                        : `${group.state === "in_progress" ? "功能仍在实施 · " : group.state === "blocked" ? "功能暂时受阻 · " : ""}${group.files.length} 个文件${group.files.some((file) => file.events.some((event) => event.binary)) ? " · 含二进制" : ""}`}</small>
                    </span>
                    <span className="enhanced-feature-chevron" aria-hidden="true">
                      {expanded ? <IconChevronDown width={13} height={13} /> : <IconChevronRight width={13} height={13} />}
                    </span>
                  </button>
                  <span className={`enhanced-feature-state state-${group.state}`}>
                    {group.state === "in_progress" && <i aria-hidden="true" />}
                    {STATE_LABEL[group.state]}
                  </span>
                  {group.decision ? (
                    <span className={`enhanced-decision ${group.decision}`}>{decisionLabel(group.decision)}</span>
                  ) : (
                    <span className="enhanced-actions">
                      <button
                        type="button"
                        className="btn sm"
                        disabled={!terminal || busy || hasFileDecision}
                        title={featureActionTitle ?? "接受这个功能点的全部文件"}
                        onClick={() => void perform(group, null, "accepted")}
                      >{featureBusy ? "处理中…" : "接受整组"}</button>
                      <button
                        type="button"
                        className={`btn sm enhanced-reject${confirmRejects.has(featureKey) ? " confirm" : ""}`}
                        disabled={!terminal || busy || hasFileDecision}
                        title={featureActionTitle ?? "拒绝并恢复这个功能点的全部事件补丁"}
                        onClick={() => void perform(group, null, "rejected")}
                      >{confirmRejects.has(featureKey) ? "再次确认拒绝" : "拒绝整组"}</button>
                    </span>
                  )}
                </header>
                {actionError?.key === featureKey && (
                  <div className="enhanced-action-error" role="alert" title={actionError.detail}>{actionError.message}</div>
                )}
                {expanded && <div className="enhanced-feature-body" id={bodyId}>
                  {!terminal && (
                    <div className="enhanced-running-note">
                      {group.state === "blocked"
                        ? "功能暂时受阻 · 仍可恢复实施，完成或终止前不能接受或拒绝"
                        : "功能仍在实施 · diff 会持续刷新，结束前不能接受或拒绝"}
                    </div>
                  )}
                  <div className="enhanced-files">
                  {group.files.map((file) => {
                    const key = operationKey(group.item_id, file.path);
                    const isBusy = fileBusy(group.item_id, file.path);
                    const isBinary = file.events.some((event) => event.binary);
                    return (
                      <article className="enhanced-file" key={file.path} data-path={file.path}>
                        <header className="enhanced-file-head">
                          <i className="feature-diamond small" aria-hidden="true" />
                          <strong title={file.path}>{file.path}</strong>
                          <span className="enhanced-file-meta">事件 {file.events.length}</span>
                          {isBinary && <span className="enhanced-binary">二进制</span>}
                          {file.decision ? (
                            <span className={`enhanced-decision ${file.decision}`}>{decisionLabel(file.decision)}</span>
                          ) : (
                            <span className="enhanced-actions">
                              <button
                                type="button"
                                disabled={!terminal || isBusy}
                                title={!terminal
                                  ? group.state === "blocked" ? "功能暂时受阻，尚可恢复实施" : "功能仍在实施"
                                  : `接受 ${file.path}`}
                                onClick={() => void perform(group, file.path, "accepted")}
                              >{pending.has(key) ? "…" : "接受"}</button>
                              <button
                                type="button"
                                className={confirmRejects.has(key) ? "confirm" : ""}
                                disabled={!terminal || isBusy}
                                title={!terminal
                                  ? group.state === "blocked" ? "功能暂时受阻，尚可恢复实施" : "功能仍在实施"
                                  : `拒绝并恢复 ${file.path}`}
                                onClick={() => void perform(group, file.path, "rejected")}
                              >{confirmRejects.has(key) ? "确认拒绝" : "拒绝"}</button>
                            </span>
                          )}
                        </header>
                        {actionError?.key === key && (
                          <div className="enhanced-action-error file" role="alert" title={actionError.detail}>{actionError.message}</div>
                        )}
                        <div className="enhanced-events">
                          {file.events.map((event) => event.binary ? (
                            <div className="enhanced-binary-preview" key={event.event_id}>
                              <strong>二进制变更</strong>
                              <span>不提供行级预览 · {event.before_exists ? "已有文件" : "新文件"} → {event.after_exists ? "保留" : "删除"}</span>
                            </div>
                          ) : (
                            <pre className="enhanced-patch" key={event.event_id} aria-label={`${file.path} 的事件补丁 ${event.sequence}`}>
                              {(event.patch ?? "").split("\n").map((line, index) => (
                                <code className={`patch-${patchLineKind(line)}`} key={`${event.event_id}:${index}`}>
                                  <span>{index + 1}</span>{line || " "}
                                </code>
                              ))}
                            </pre>
                          ))}
                        </div>
                      </article>
                    );
                  })}
                  {group.files.length === 0 && <div className="enhanced-no-files">该功能点还没有捕获到文件事件。</div>}
                  </div>
                </div>}
              </section>
            );
          })}
        </div>
      )}
    </div>
  );
}
