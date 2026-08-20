import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  acceptTask,
  changeRequest,
  permissionApprove,
  reviewAcceptAll,
  reviewAcceptFile,
  reviewGitStatus,
  REVIEW_STATUS_CHANGED_EVENT,
  rollbackTask,
} from "../../lib/ipc";
import { elapsedSince, permissionAttribution, permissionRiskLabel } from "../../lib/format";
import { taskTitle, workspaceName } from "../../lib/presentation";
import { usePoll } from "../../lib/poll";
import { selectNeedsYou, useTasksStore, type NeedsYouItem } from "../../store/tasks";
import { useAppStore } from "../../store/app";
import type { PermissionDecision, ReviewGitStatus } from "../../lib/types";
import {
  IconArrowRight,
  IconChevronLeft,
  IconCheck,
  IconClose,
  IconFile,
  IconFolderOpen,
  IconRefresh,
  IconShield,
} from "../icons";

type InspectorKind = "permission" | "review_ready" | "plan_entry_offer";

interface ReviewStatusEntry {
  status: ReviewGitStatus | null;
  error: string | null;
}

interface InboxProjectGroup {
  key: string;
  name: string;
  path: string | null;
  items: NeedsYouItem[];
  permissionCount: number;
  reviewCount: number;
  pendingFileCount: number;
}

const itemKey = (item: NeedsYouItem) => item.kind === "permission" ? `permission:${item.permission!.id}` : item.kind === "plan_entry_offer" ? `plan-offer:${item.task.id}` : `review:${item.task.id}`;

function reviewStatusSignature(entry: ReviewStatusEntry | undefined): string {
  if (!entry) return "missing";
  const status = entry.status;
  if (!status) return `error:${entry.error ?? "loading"}`;
  return [
    status.git_repository,
    status.accepted_count,
    status.rejected_count,
    status.remaining_count,
    status.conflict_count,
    status.can_accept_all,
    entry.error ?? "",
    ...status.paths.map((path) => `${path.path}:${path.accepted}:${path.rejected}:${path.remaining}:${path.conflict}:${path.safe_to_accept}:${path.blocker ?? ""}`),
  ].join("|");
}

function sameReviewStatuses(left: Record<string, ReviewStatusEntry>, right: Record<string, ReviewStatusEntry>): boolean {
  const leftKeys = Object.keys(left);
  const rightKeys = Object.keys(right);
  return leftKeys.length === rightKeys.length
    && rightKeys.every((key) => reviewStatusSignature(left[key]) === reviewStatusSignature(right[key]));
}

/**
 * 跨项目待处理页：任务状态决定“是否需要决策”，应用审核账本决定“哪些文件尚待处理”。
 * Git 交付是后续独立步骤，不参与这里的实时同步。
 */
export function InboxScene() {
  const items = useTasksStore(selectNeedsYou);
  const details = useTasksStore((s) => s.details);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const refreshDetails = useTasksStore((s) => s.refreshDetails);
  const workspaces = useTasksStore((s) => s.workspaces);
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [inspectorCollapsed, setInspectorCollapsed] = useState(false);
  const [reviewStatuses, setReviewStatuses] = useState<Record<string, ReviewStatusEntry>>({});
  const [hydrated, setHydrated] = useState(false);
  const [manualRefreshing, setManualRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const refreshFlight = useRef<Promise<void> | null>(null);

  const syncReviewStatuses = useCallback(async (taskIds: string[]) => {
    const results = await Promise.all(taskIds.map(async (taskId) => {
      try {
        return { taskId, status: await reviewGitStatus(taskId), error: null };
      } catch (cause) {
        return { taskId, status: null, error: String(cause) };
      }
    }));
    setReviewStatuses((current) => {
      const next: Record<string, ReviewStatusEntry> = {};
      for (const result of results) {
        next[result.taskId] = result.status
          ? { status: result.status, error: null }
          : { status: current[result.taskId]?.status ?? null, error: result.error };
      }
      return sameReviewStatuses(current, next) ? current : next;
    });
  }, []);

  const refreshInbox = useCallback((): Promise<void> => {
    if (refreshFlight.current) return refreshFlight.current;
    const operation = (async () => {
      await refreshTasks();
      const state = useTasksStore.getState();
      const detailIds = state.tasks
        .filter((task) => task.state !== "idle" && task.state !== "archived")
        .map((task) => task.id);
      if (detailIds.length) await refreshDetails(detailIds);
      const reviewIds = useTasksStore.getState().tasks
        .filter((task) => task.state === "review_ready")
        .map((task) => task.id);
      await syncReviewStatuses(reviewIds);
    })();
    refreshFlight.current = operation;
    const finish = () => {
      if (refreshFlight.current === operation) refreshFlight.current = null;
      setHydrated(true);
    };
    void operation.then(finish, finish);
    return operation;
  }, [refreshDetails, refreshTasks, syncReviewStatuses]);

  const refreshReviewStatus = useCallback(async (taskId: string) => {
    try {
      const status = await reviewGitStatus(taskId);
      setReviewStatuses((current) => {
        const next = { ...current, [taskId]: { status, error: null } };
        return sameReviewStatuses(current, next) ? current : next;
      });
    } catch (cause) {
      setReviewStatuses((current) => ({
        ...current,
        [taskId]: { status: current[taskId]?.status ?? null, error: String(cause) },
      }));
      throw cause;
    }
  }, []);

  usePoll(refreshInbox, 2000);

  useEffect(() => {
    const onReviewStatusChanged = (event: Event) => {
      const taskId = (event as CustomEvent<{ taskId?: string }>).detail?.taskId;
      if (taskId && useTasksStore.getState().tasks.some((task) => task.id === taskId && task.state === "review_ready")) {
        void refreshReviewStatus(taskId).catch(() => undefined);
      }
    };
    window.addEventListener(REVIEW_STATUS_CHANGED_EVENT, onReviewStatusChanged);
    return () => window.removeEventListener(REVIEW_STATUS_CHANGED_EVENT, onReviewStatusChanged);
  }, [refreshReviewStatus]);

  useEffect(() => {
    if (items.length === 0) {
      setSelectedKey(null);
      setInspectorCollapsed(false);
      return;
    }
    if (!selectedKey || !items.some((item) => itemKey(item) === selectedKey)) setSelectedKey(itemKey(items[0]));
  }, [items, selectedKey]);

  const selected = useMemo(() => items.find((item) => itemKey(item) === selectedKey) ?? null, [items, selectedKey]);
  const groups = useMemo<InboxProjectGroup[]>(() => {
    const byPath = new Map<string, InboxProjectGroup>();
    for (const item of items) {
      const path = item.task.workspace_path;
      const key = path ?? "__unassigned__";
      let group = byPath.get(key);
      if (!group) {
        group = {
          key,
          name: workspaceName(path, workspaces),
          path,
          items: [],
          permissionCount: 0,
          reviewCount: 0,
          pendingFileCount: 0,
        };
        byPath.set(key, group);
      }
      group.items.push(item);
      if (item.kind === "permission") {
        group.permissionCount += 1;
      } else {
        group.reviewCount += 1;
        group.pendingFileCount += reviewStatuses[item.task.id]?.status?.remaining_count
          ?? details[item.task.id]?.changes.length
          ?? 0;
      }
    }
    const workspaceOrder = new Map(workspaces.map((workspace, index) => [workspace.canonical_path, index]));
    return [...byPath.values()].sort((left, right) => {
      const leftOrder = left.path ? workspaceOrder.get(left.path) ?? Number.MAX_SAFE_INTEGER : Number.MAX_SAFE_INTEGER;
      const rightOrder = right.path ? workspaceOrder.get(right.path) ?? Number.MAX_SAFE_INTEGER : Number.MAX_SAFE_INTEGER;
      return leftOrder - rightOrder || left.name.localeCompare(right.name);
    });
  }, [details, items, reviewStatuses, workspaces]);
  const kind: InspectorKind = selected?.kind ?? "permission";
  const permissionCount = items.filter((item) => item.kind === "permission").length;
  const reviewCount = items.length - permissionCount;
  const pendingFileCount = groups.reduce((count, group) => count + group.pendingFileCount, 0);

  const choose = (key: string) => {
    setSelectedKey(key);
    setInspectorCollapsed(false);
  };
  const manualRefresh = async () => {
    if (manualRefreshing) return;
    setManualRefreshing(true);
    setError(null);
    try {
      await refreshInbox();
    } catch (cause) {
      setError(`刷新待处理失败：${String(cause)}`);
    } finally {
      setManualRefreshing(false);
    }
  };

  return (
    <div className={`scene scene-inbox${selected ? " has-inspector" : ""}${inspectorCollapsed ? " inspector-collapsed" : ""}`}>
      <div className="inbox-main">
        <div className="inbox-scroll">
          <header className="inbox-header">
            <div>
              <p className="page-kicker">NEEDS YOU</p>
              <h1>待处理</h1>
              <p>跨项目同步权限请求与审核变更，处理结果会实时回流。</p>
            </div>
            <div className="inbox-header-actions">
              <span className="inbox-live"><i />实时同步</span>
              <span className="inbox-count">{items.length} 项</span>
              <button className={`inbox-refresh${manualRefreshing ? " refreshing" : ""}`} onClick={() => void manualRefresh()} disabled={manualRefreshing} aria-label="刷新待处理" title="立即刷新">
                <IconRefresh width={15} height={15} />
              </button>
            </div>
          </header>

          {items.length > 0 && (
            <div className="inbox-overview" aria-label="待处理概览">
              <span><strong>{groups.length}</strong> 个项目</span>
              <span><strong>{permissionCount}</strong> 项授权</span>
              <span><strong>{reviewCount}</strong> 项审核</span>
              <span><strong>{pendingFileCount}</strong> 个文件待处理</span>
            </div>
          )}
          {error && <div className="inbox-error" role="alert">{error}</div>}

          {!hydrated && items.length === 0 ? (
            <div className="inbox-empty inbox-loading"><IconRefresh width={24} height={24} /><h2>正在同步待处理事项</h2><p>正在读取各项目的权限与审核状态。</p></div>
          ) : items.length === 0 ? (
            <div className="inbox-empty"><IconCheck width={24} height={24} /><h2>暂时没有待处理事项</h2><p>权限请求和待审核变更会在出现时显示在这里。</p></div>
          ) : (
            <div className="inbox-projects" aria-label="按项目分组的待处理事项">
              {groups.map((group, index) => (
                <section className="inbox-project-group" key={group.key} data-project-path={group.path ?? ""} aria-labelledby={`inbox-project-${index}`}>
                  <header className="inbox-project-head">
                    <span className="inbox-project-mark"><IconFolderOpen width={17} height={17} /></span>
                    <div>
                      <h2 id={`inbox-project-${index}`}>{group.name}</h2>
                      <p>{group.path ?? "未归属本地项目"}</p>
                    </div>
                    <span className="inbox-project-summary">
                      {group.permissionCount > 0 && <b>{group.permissionCount} 授权</b>}
                      {group.reviewCount > 0 && <b>{group.reviewCount} 审核</b>}
                      {group.pendingFileCount > 0 && <b>{group.pendingFileCount} 文件</b>}
                    </span>
                  </header>
                  <div className="inbox-list">
                    {group.items.map((item) => (
                      <InboxRow
                        key={itemKey(item)}
                        item={item}
                        selected={itemKey(item) === selectedKey}
                        detailChanges={details[item.task.id]?.changes.length ?? 0}
                        reviewEntry={reviewStatuses[item.task.id]}
                        onSelect={() => choose(itemKey(item))}
                      />
                    ))}
                  </div>
                </section>
              ))}
            </div>
          )}
        </div>
      </div>

      {selected && (
        <aside className="inbox-inspector" aria-label={kind === "permission" ? "权限详情" : "审核摘要"}>
          {inspectorCollapsed ? (
            <button className="inspector-rail-button" onClick={() => setInspectorCollapsed(false)} title={`展开${kind === "permission" ? "权限详情" : "审核摘要"}`}>
              <span>{kind === "permission" ? "权限详情" : "审核摘要"}</span><IconChevronLeft width={16} height={16} />
            </button>
          ) : kind === "permission" ? (
            <PermissionInspector item={selected} onError={setError} onCollapse={() => setInspectorCollapsed(true)} />
          ) : (
            <ReviewInspector
              item={selected}
              reviewEntry={reviewStatuses[selected.task.id]}
              onRefreshStatus={refreshReviewStatus}
              onError={setError}
              onCollapse={() => setInspectorCollapsed(true)}
            />
          )}
        </aside>
      )}
    </div>
  );
}

function InboxRow({
  item,
  selected,
  detailChanges,
  reviewEntry,
  onSelect,
}: {
  item: NeedsYouItem;
  selected: boolean;
  detailChanges: number;
  reviewEntry?: ReviewStatusEntry;
  onSelect: () => void;
}) {
  const label = item.kind === "permission" ? "权限请求" : "等待审核";
  let description = item.kind === "permission" ? item.permission!.tool_name : `${detailChanges} 个文件变更`;
  if (item.kind === "review_ready") {
    if (reviewEntry?.status) {
      description = reviewEntry.status.remaining_count > 0
        ? `${reviewEntry.status.remaining_count} 个文件待处理`
        : "审核项已全部处理 · 待确认完成";
    } else if (reviewEntry?.error) {
      description = "审核状态同步失败 · 可打开详情重试";
    } else if (!reviewEntry) {
      description = "正在同步审核状态";
    }
  }
  return (
    <button className={`inbox-row${selected ? " selected" : ""}`} data-task-id={item.task.id} onClick={onSelect}>
      <span className={`inbox-row-icon ${item.kind}`}>{item.kind === "permission" ? <IconShield width={17} height={17} /> : <IconFile width={17} height={17} />}</span>
      <span className="inbox-row-copy"><small>{label}</small><strong>{taskTitle(item.task)}</strong><em>{description}</em></span>
      <span className={`inbox-row-state ${item.kind}`}>{item.kind === "permission" ? "待授权" : reviewEntry?.status?.remaining_count === 0 ? "待完成" : "待审核"}</span>
      <time>等待 {elapsedSince(item.since)}</time>
      <IconArrowRight className="inbox-row-arrow" width={16} height={16} />
    </button>
  );
}

function InspectorHead({ title, subtitle, onCollapse }: { title: string; subtitle: string; onCollapse: () => void }) {
  return (
    <header className="inspector-head"><div><p className="section-kicker">DECISION DETAIL</p><h2>{title}</h2><span>{subtitle}</span></div><button className="inspector-close" onClick={onCollapse} aria-label={`收起${title}`} title={`收起${title}`}><IconClose width={13} height={13} /></button></header>
  );
}

function PermissionInspector({ item, onError, onCollapse }: { item: NeedsYouItem; onError: (text: string | null) => void; onCollapse: () => void }) {
  const permission = item.permission!;
  const detail = useTasksStore((s) => s.details[item.task.id]);
  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const openRoom = useAppStore((s) => s.openRoom);
  const [busy, setBusy] = useState(false);
  const attribution = permissionAttribution(permission, detail?.runs ?? []);
  const decide = async (decision: Exclude<PermissionDecision, "pending">) => {
    if (busy) return;
    setBusy(true);
    onError(null);
    try {
      await permissionApprove(permission.id, decision);
      await refreshDetail(item.task.id);
    } catch (cause) {
      onError(`权限裁决失败：${String(cause)}`);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="inspector-card">
      <InspectorHead title="权限详情" subtitle={taskTitle(item.task)} onCollapse={onCollapse} />
      <div className="inspector-body">
        <div className="inspector-callout permission"><IconShield width={19} height={19} /><div><strong>{permission.tool_name}</strong><span>{permission.risk_level} · {permissionRiskLabel(permission.risk_level)}</span></div></div>
        <DetailLine label="发起者" value={attribution.label} />
        <DetailLine label="等待时间" value={elapsedSince(item.since)} />
        <div className="inspector-summary"><small>请求说明</small><p>{permission.input_summary || "没有补充说明。"}</p></div>
      </div>
      <footer className="inspector-actions">
        <button className="rc-button rc-button-primary" disabled={busy} onClick={() => void decide("allow")}>允许一次</button>
        <button className="rc-button" disabled={busy} onClick={() => void decide("deny")}>拒绝</button>
        <button className="rc-button rc-button-quiet" disabled={busy} onClick={() => void decide("allow_always")}>始终允许</button>
        <button className="text-link inspector-open-task" onClick={() => openRoom(item.task.id)}>打开任务 <IconArrowRight width={14} height={14} /></button>
      </footer>
    </div>
  );
}

function ReviewInspector({
  item,
  reviewEntry,
  onRefreshStatus,
  onError,
  onCollapse,
}: {
  item: NeedsYouItem;
  reviewEntry?: ReviewStatusEntry;
  onRefreshStatus: (taskId: string) => Promise<void>;
  onError: (text: string | null) => void;
  onCollapse: () => void;
}) {
  const detail = useTasksStore((s) => s.details[item.task.id]);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const openRoom = useAppStore((s) => s.openRoom);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [pendingFiles, setPendingFiles] = useState<Set<string>>(new Set());
  const [requestingChanges, setRequestingChanges] = useState(false);
  const [feedback, setFeedback] = useState("");
  const busy = busyAction !== null;
  const changes = detail?.changes ?? [];
  const changeByPath = useMemo(() => new Map(changes.map((change) => [change.path, change])), [changes]);
  const verify = detail?.verifications.slice(-1)[0];
  const status = reviewEntry?.status ?? null;
  const pendingPaths = status ? status.paths.filter((path) => path.remaining) : [];
  const open = (tab: "review" | "changes") => openRoom(item.task.id, tab);

  const finishReview = async () => {
    if (busy) return;
    setBusyAction("finish");
    onError(null);
    try {
      await acceptTask(item.task.id);
      await refreshTasks();
    } catch (cause) {
      onError(`完成审核失败：${String(cause)}`);
    } finally {
      setBusyAction(null);
    }
  };
  const rollback = async () => {
    if (busy) return;
    setBusyAction("rollback");
    onError(null);
    try {
      await rollbackTask(item.task.id);
      await refreshTasks();
    } catch (cause) {
      onError(`回滚失败：${String(cause)}`);
    } finally {
      setBusyAction(null);
    }
  };
  const acceptFile = async (path: string) => {
    if (busy || pendingFiles.has(path)) return;
    setPendingFiles((current) => new Set(current).add(path));
    onError(null);
    try {
      await reviewAcceptFile(item.task.id, path);
      await onRefreshStatus(item.task.id);
    } catch (cause) {
      onError(`接受文件失败：${String(cause)}`);
    } finally {
      setPendingFiles((current) => {
        const next = new Set(current);
        next.delete(path);
        return next;
      });
    }
  };
  const acceptAllFiles = async () => {
    if (busy) return;
    setBusyAction("all-files");
    onError(null);
    try {
      await reviewAcceptAll(item.task.id);
      await onRefreshStatus(item.task.id);
    } catch (cause) {
      onError(`接受全部文件失败：${String(cause)}`);
    } finally {
      setBusyAction(null);
    }
  };
  const requestChanges = async () => {
    const message = feedback.trim();
    if (!message) {
      onError("请先说明希望修改的内容。");
      return;
    }
    if (busy) return;
    setBusyAction("request-changes");
    onError(null);
    try {
      await changeRequest(item.task.id, message);
      await Promise.all([refreshTasks(), refreshDetail(item.task.id)]);
      setFeedback("");
      setRequestingChanges(false);
    } catch (cause) {
      onError(`请求修改失败：${String(cause)}`);
    } finally {
      setBusyAction(null);
    }
  };

  const fileSummary = !reviewEntry
    ? "正在同步文件状态"
    : reviewEntry.error && !status
      ? "文件状态暂不可用"
      : status
        ? status.remaining_count === 0 ? "审核项已全部处理" : `${status.remaining_count} 个文件待处理`
        : `${changes.length} 个文件变更`;

  return (
    <div className="inspector-card">
      <InspectorHead title="审核摘要" subtitle={taskTitle(item.task)} onCollapse={onCollapse} />
      <div className="inspector-body">
        <div className="inspector-callout review"><IconFile width={19} height={19} /><div><strong>{fileSummary}</strong><span>{verify ? `${verify.command} · ${verify.status === "passed" ? "验证通过" : verify.status}` : "尚未记录验证"}</span></div></div>
        {reviewEntry?.error && <div className="inspector-sync-warning" role="status">状态同步暂时失败，仍显示最近一次结果。<button className="text-link" disabled={busy} onClick={() => void onRefreshStatus(item.task.id).catch(() => undefined)}>重试</button></div>}
        <div className="inspector-file-list" aria-label="待处理文件">
          {!reviewEntry ? (
            <p>正在读取应用审核状态…</p>
          ) : status && pendingPaths.length === 0 ? (
            <div className="review-files-complete"><IconCheck width={16} height={16} /><span><strong>审核项已全部处理</strong><small>请确认验证结果后完成审核。</small></span></div>
          ) : status ? (
            pendingPaths.map((pathStatus) => {
              const change = changeByPath.get(pathStatus.path);
              return (
                <div className={`inspector-review-file${pathStatus.conflict ? " conflict" : ""}`} key={pathStatus.path}>
                  <IconFile width={14} height={14} />
                  <span><strong>{pathStatus.path}</strong><small>{pathStatus.conflict ? pathStatus.blocker ?? "存在冲突" : change?.change_type ?? "变更"}</small></span>
                  <button className="rc-button rc-button-quiet" disabled={busy || pendingFiles.has(pathStatus.path) || !pathStatus.safe_to_accept} onClick={() => void acceptFile(pathStatus.path)} aria-label={`接受文件 ${pathStatus.path}`}>{pendingFiles.has(pathStatus.path) ? "接受中…" : "接受"}</button>
                </div>
              );
            })
          ) : changes.length === 0 ? (
            <p>变更明细读取中，或当前没有可展示的文件。</p>
          ) : (
            changes.map((change) => <div className="inspector-review-file readonly" key={change.id}><IconFile width={14} height={14} /><span><strong>{change.path}</strong><small>{change.change_type}</small></span></div>)
          )}
        </div>
        {status && (status.accepted_count > 0 || status.rejected_count > 0) && <p className="review-accepted-note">已接受 {status.accepted_count} 个文件，已拒绝 {status.rejected_count} 个文件；列表仅显示仍待处理的文件。</p>}
        {requestingChanges && (
          <div className="review-request-form">
            <label htmlFor={`change-request-${item.task.id}`}>修改说明</label>
            <textarea id={`change-request-${item.task.id}`} value={feedback} onChange={(event) => setFeedback(event.target.value)} placeholder="例如：请补充异常分支的测试，并说明 API 错误码的兼容策略。" disabled={busy} autoFocus />
            <div><button className="rc-button rc-button-primary" disabled={busy || !feedback.trim()} onClick={() => void requestChanges()}>发送修改请求</button><button className="rc-button rc-button-quiet" disabled={busy} onClick={() => setRequestingChanges(false)}>取消</button></div>
          </div>
        )}
      </div>
      <footer className="inspector-actions review-actions">
        <button className="rc-button rc-button-primary" disabled={busy} onClick={() => void finishReview()}>{busyAction === "finish" ? "正在完成…" : "完成审核"}</button>
        {status && status.remaining_count > 0 && <button className="rc-button" disabled={busy || !status.can_accept_all} onClick={() => void acceptAllFiles()}>{busyAction === "all-files" ? "接受中…" : "接受全部文件"}</button>}
        <button className="rc-button" onClick={() => open("review")}>完整审核</button>
        <button className="rc-button" disabled={busy} onClick={() => setRequestingChanges((open) => !open)}>{requestingChanges ? "收起修改说明" : "请求修改"}</button>
        <button className="rc-button rc-button-quiet" disabled={busy} onClick={() => void rollback()}>{busyAction === "rollback" ? "回滚中…" : "回滚"}</button>
        <button className="text-link inspector-open-task" onClick={() => open("changes")}>打开任务变更 <IconArrowRight width={14} height={14} /></button>
      </footer>
    </div>
  );
}

function DetailLine({ label, value }: { label: string; value: string }) {
  return <div className="inspector-detail-line"><span>{label}</span><strong>{value}</strong></div>;
}
