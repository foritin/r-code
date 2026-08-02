import { useState } from "react";
import { permissionApprove, taskDelete, taskRestore } from "../../lib/ipc";
import { elapsedMinutes, elapsedSince, permissionRiskLabel } from "../../lib/format";
import { taskTitle } from "../../lib/presentation";
import { usePoll } from "../../lib/poll";
import { useTasksStore } from "../../store/tasks";
import { useAppStore } from "../../store/app";
import { pushToast } from "../../store/toast";
import type {
  DashboardAttentionItem,
  DashboardTaskSummary,
  PermissionDecision,
  ProjectActivityItem,
  Task,
} from "../../lib/types";
import {
  IconArrowRight,
  IconEditor,
  IconFile,
  IconProjects,
  IconRestore,
  IconShield,
  IconTrash,
} from "../icons";
import { ConfirmDialog } from "../ui/ConfirmDialog";

/** 项目仪表盘：唯一包含「项目动态」右栏的场景，数据来自 cmd_workspace_dashboard。 */
export function DashboardScene() {
  const workspacePath = useTasksStore((s) => s.currentProjectId);
  const workspaces = useTasksStore((s) => s.workspaces);
  const dashboard = useTasksStore((s) => workspacePath ? s.dashboards[workspacePath] : undefined);
  const activityPage = useTasksStore((s) => workspacePath ? s.projectActivities[workspacePath] : undefined);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const refreshDashboard = useTasksStore((s) => s.refreshDashboard);
  const refreshProjectActivity = useTasksStore((s) => s.refreshProjectActivity);
  const openRoom = useAppStore((s) => s.openRoom);
  const setScene = useAppStore((s) => s.setScene);
  const [error, setError] = useState<string | null>(null);

  const workspace = dashboard?.workspace ?? workspaces.find((item) => item.canonical_path === workspacePath);
  const refresh = async () => {
    if (!workspacePath) return;
    try {
      await Promise.all([
        refreshTasks(),
        refreshDashboard(workspacePath),
        refreshProjectActivity(workspacePath),
      ]);
    } catch (cause) {
      setError(`项目数据加载失败：${String(cause)}`);
    }
  };

  usePoll(refresh, 2500, Boolean(workspacePath));

  if (!workspacePath || !workspace) {
    return (
      <div className="scene scene-dashboard dashboard-empty-scene">
        <div className="dashboard-empty">
          <span className="dashboard-empty-icon"><IconProjects width={26} height={26} /></span>
          <div>
            <h1>先选择一个项目。</h1>
            <p>每个项目都有自己的任务概览、待处理事项和项目动态。</p>
          </div>
          <button className="rc-button rc-button-primary" onClick={() => setScene("projects")}>管理项目</button>
        </div>
      </div>
    );
  }

  const metrics = dashboard?.metrics;
  const taskSummaries = dashboard?.tasks ?? [];
  const attention = dashboard?.attention ?? [];
  const archived = dashboard?.archived ?? [];

  return (
    <div className="scene scene-dashboard">
      <div className="dashboard-main">
        <div className="dashboard-scroll">
          <header className="dashboard-header">
            <div className="dashboard-project-mark"><IconProjects width={20} height={20} /></div>
            <div>
              <span className="dashboard-context-label">项目概览</span>
              <h1>{workspace.display_name}</h1>
              <p>查看正在推进的任务、需要处理的事项与归档记录。</p>
            </div>
            <div className="dashboard-header-actions">
              <button className="rc-button rc-button-quiet" onClick={() => setScene("editor")}><IconEditor width={15} height={15} />项目文件</button>
              <button className="rc-button rc-button-primary" onClick={() => setScene("home")}>新建任务</button>
            </div>
          </header>

          <section className="dashboard-metrics" aria-label="项目摘要">
            <Metric label="待处理" value={(metrics?.pending_permission_count ?? 0) + (metrics?.review_ready_count ?? 0)} tone={(metrics?.pending_permission_count ?? 0) + (metrics?.review_ready_count ?? 0) > 0 ? "warm" : undefined} />
            <Metric label="运行中" value={metrics?.running_task_count ?? 0} tone="success" />
            <Metric label="子代理" value={metrics?.active_subagent_count ?? 0} />
            <Metric label="已归档" value={metrics?.archived_task_count ?? 0} />
          </section>

          {error && <div className="dashboard-error" role="alert">{error}</div>}

          {attention.length > 0 && (
            <section className="dashboard-section dashboard-attention-section">
              <div className="dashboard-section-title">
                <div><h2>需要你处理</h2><p>这些任务在等待你的决定。</p></div>
                <button className="text-link" onClick={() => setScene("inbox")}>查看全部 <IconArrowRight width={14} height={14} /></button>
              </div>
              <div className="dashboard-attention-list">
                {attention.slice(0, 2).map((item) => (
                  item.kind === "permission"
                    ? <ProjectPermission key={item.permission?.id ?? item.task.id} item={item} workspacePath={workspacePath} onError={setError} />
                    : <ProjectReview key={item.task.id} item={item} summary={taskSummaries.find((entry) => entry.task.id === item.task.id)} />
                ))}
              </div>
            </section>
          )}

          <section className="dashboard-section">
            <div className="dashboard-section-title">
              <div><h2>项目任务</h2><p>当前项目中未归档的对话。</p></div>
              <span className="section-meta">{metrics?.task_count ?? taskSummaries.length} 个任务</span>
            </div>
            {taskSummaries.length === 0 ? (
              <div className="dashboard-blank-row">
                {archived.length > 0 ? "当前没有未归档任务。你可以从下方还原对话，或新建一个任务。" : "这个项目还没有任务。创建一个任务，仪表盘会在这里开始积累进度。"}
              </div>
            ) : (
              <div className="dashboard-task-table" role="table" aria-label="项目任务">
                <div className="dashboard-task-head" role="row">
                  <span>任务</span><span>当前活动</span><span>负责人</span><span>变更</span><span>更新时间</span>
                </div>
                {taskSummaries.slice(0, 12).map((summary) => (
                  <TaskRow key={summary.task.id} summary={summary} onOpen={() => openRoom(summary.task.id)} />
                ))}
              </div>
            )}
          </section>

          <section className="dashboard-section dashboard-archived-section">
            <div className="dashboard-section-title">
              <div><h2>已归档</h2><p>归档对话不会出现在项目任务与项目动态中。</p></div>
              <span className="section-meta">{metrics?.archived_task_count ?? archived.length} 个归档</span>
            </div>
            <div className="dashboard-archived-list" role="table" aria-label="已归档对话">
              {archived.length > 0 && (
                <div className="dashboard-archived-head" role="row">
                  <span>对话</span><span>归档时间</span><span>操作</span>
                </div>
              )}
              {archived.map((task) => (
                <ArchivedTaskRow key={task.id} task={task} onChanged={refresh} onError={setError} />
              ))}
              {!archived.length && <div className="dashboard-blank-row compact">还没有归档对话。</div>}
            </div>
          </section>
        </div>
      </div>
      <ProjectActivityRail items={activityPage?.items ?? []} />
    </div>
  );
}

function Metric({ label, value, tone }: { label: string; value: number; tone?: "warm" | "success" }) {
  return <div className={`dashboard-metric${tone ? ` ${tone}` : ""}`}><span>{label}</span><strong>{value}</strong></div>;
}

function summaryVisual(summary: DashboardTaskSummary): "running" | "attention" | "review" | "done" | "stopped" | "idle" {
  if (summary.pending_permission_count > 0) return "attention";
  if (summary.task.state === "review_ready") return "review";
  if (summary.active_run?.ended_at === null || summary.task.state === "exploring" || summary.task.state === "in_progress") return "running";
  if (summary.task.state === "interrupted") return "stopped";
  if (summary.task.state === "idle") return "done";
  return "idle";
}

function summaryStateLabel(summary: DashboardTaskSummary): string {
  if (summary.pending_permission_count > 0) return "等待你的处理";
  if (summary.task.state === "exploring") return "正在分析";
  if (summary.task.state === "in_progress") return "正在执行";
  if (summary.task.state === "review_ready") return "等待审查";
  if (summary.task.state === "interrupted") return "已中止";
  if (summary.task.state === "archived") return "已归档";
  return "已完成";
}

function TaskRow({ summary, onOpen }: { summary: DashboardTaskSummary; onOpen: () => void }) {
  const stat = summary.change_summary;
  return (
    <button className="dashboard-task-row" onClick={onOpen} role="row">
      <span className="dashboard-task-name"><i className={`task-state-dot ${summaryVisual(summary)}`} /><strong>{taskTitle(summary.task)}</strong><small>{summaryStateLabel(summary)}</small></span>
      <span className="dashboard-task-activity">{summary.activity}</span>
      <span className="dashboard-task-agent">{summary.agent_label}</span>
      <span className="dashboard-task-diff">{stat.files ? <><b>+{stat.created + stat.modified}</b><em>−{stat.removed}</em></> : "—"}</span>
      <time>{elapsedMinutes(summary.task.updated_at)}</time>
    </button>
  );
}

function archivedAt(iso: string): string {
  const value = new Date(iso);
  if (Number.isNaN(value.getTime())) return "—";
  return new Intl.DateTimeFormat("zh-CN", {
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(value);
}

function ArchivedTaskRow({
  task,
  onChanged,
  onError,
}: {
  task: Task;
  onChanged: () => Promise<void>;
  onError: (message: string | null) => void;
}) {
  const openRoom = useAppStore((s) => s.openRoom);
  const forgetTaskNavigation = useAppStore((s) => s.forgetTaskNavigation);
  const [busy, setBusy] = useState<"restore" | "delete" | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const title = taskTitle(task);

  const restore = async () => {
    if (busy) return;
    setBusy("restore");
    onError(null);
    try {
      await taskRestore(task.id);
      await onChanged();
      pushToast({ kind: "success", title: "对话已还原", body: title });
    } catch (cause) {
      const message = `无法还原对话：${String(cause)}`;
      onError(message);
      pushToast({ kind: "error", title: "无法还原对话", body: String(cause) });
    } finally {
      setBusy(null);
    }
  };

  const remove = async () => {
    if (busy) return;
    setBusy("delete");
    onError(null);
    try {
      await taskDelete(task.id);
      forgetTaskNavigation(task.id);
      await onChanged();
      setConfirmDelete(false);
      pushToast({ kind: "success", title: "对话已永久删除", body: title });
    } catch (cause) {
      const message = `无法删除对话：${String(cause)}`;
      onError(message);
      pushToast({ kind: "error", title: "无法删除对话", body: String(cause) });
    } finally {
      setBusy(null);
    }
  };

  return (
    <>
      <div className="dashboard-archived-row" role="row">
        <button className="dashboard-archived-main" type="button" onClick={() => openRoom(task.id)}>
          <strong>{title}</strong>
          <small>打开只读历史</small>
        </button>
        <time dateTime={task.updated_at}>{archivedAt(task.updated_at)}</time>
        <div className="dashboard-archived-actions">
          <button className="archived-action restore" type="button" disabled={busy != null} onClick={() => void restore()}>
            <IconRestore width={14} height={14} />{busy === "restore" ? "还原中" : "还原"}
          </button>
          <button className="archived-action danger" type="button" disabled={busy != null} onClick={() => setConfirmDelete(true)} aria-label={`永久删除 ${title}`}>
            <IconTrash width={14} height={14} />删除
          </button>
        </div>
      </div>
      <ConfirmDialog
        open={confirmDelete}
        title="永久删除这段对话？"
        description={`“${title}”的消息、运行记录与审计历史会被永久删除。项目目录和其中的文件不会被删除。`}
        confirmLabel="永久删除"
        busy={busy === "delete"}
        onCancel={() => {
          if (!busy) setConfirmDelete(false);
        }}
        onConfirm={() => void remove()}
      />
    </>
  );
}

function ProjectPermission({ item, workspacePath, onError }: { item: DashboardAttentionItem; workspacePath: string; onError: (message: string | null) => void }) {
  const permission = item.permission;
  const refreshDashboard = useTasksStore((s) => s.refreshDashboard);
  const refreshProjectActivity = useTasksStore((s) => s.refreshProjectActivity);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const openRoom = useAppStore((s) => s.openRoom);
  const [busy, setBusy] = useState(false);
  if (!permission) return null;
  const decide = async (decision: Exclude<PermissionDecision, "pending">) => {
    if (busy) return;
    setBusy(true);
    onError(null);
    try {
      await permissionApprove(permission.id, decision);
      await Promise.all([refreshTasks(), refreshDashboard(workspacePath), refreshProjectActivity(workspacePath)]);
    } catch (cause) {
      onError(`权限裁决失败：${String(cause)}`);
    } finally {
      setBusy(false);
    }
  };
  return (
    <article className="attention-item permission-item">
      <span className="attention-icon"><IconShield width={18} height={18} /></span>
      <div className="attention-copy">
        <p><span className="attention-label">权限请求</span><span className="risk-badge">{permission.risk_level} · {permissionRiskLabel(permission.risk_level)}</span></p>
        <h3>{permission.tool_name}</h3>
        <small>{permission.input_summary || taskTitle(item.task)} · 等待 {elapsedSince(item.since)}</small>
      </div>
      <div className="attention-actions">
        <button className="rc-button rc-button-primary" disabled={busy} onClick={() => void decide("allow")}>允许一次</button>
        <button className="rc-button" disabled={busy} onClick={() => void decide("deny")}>拒绝</button>
        <button className="rc-button rc-button-quiet" onClick={() => openRoom(item.task.id)}>查看</button>
      </div>
    </article>
  );
}

function ProjectReview({ item, summary }: { item: DashboardAttentionItem; summary?: DashboardTaskSummary }) {
  const openRoom = useAppStore((s) => s.openRoom);
  return (
    <article className="attention-item review-item">
      <span className="attention-icon"><IconFile width={18} height={18} /></span>
      <div className="attention-copy">
        <p><span className="attention-label">等待审核</span></p>
        <h3>{taskTitle(item.task)}</h3>
        <small>{summary?.change_summary.files ?? 0} 个文件变更 · 等待 {elapsedSince(item.since)}</small>
      </div>
      <div className="attention-actions">
        <button className="rc-button rc-button-primary" onClick={() => openRoom(item.task.id, "review")}>查看审核</button>
      </div>
    </article>
  );
}

function activityTone(item: ProjectActivityItem): "running" | "attention" | "review" | "done" {
  if (item.kind === "permission_requested") return "attention";
  if (item.kind === "change_requested") return "review";
  if (item.kind === "run_ended" || item.kind === "verification_run") return "done";
  return "running";
}

function ProjectActivityRail({ items }: { items: ProjectActivityItem[] }) {
  const openRoom = useAppStore((s) => s.openRoom);
  // 仪表盘为每段未归档对话只保留最新关键节点；完整事件仍在任务详情和全局活动页。
  const significant = items.filter((item) => !["state_changed", "queue_dispatched", "tool_call", "tool_result", "system"].includes(item.kind));
  const visibleItems: ProjectActivityItem[] = [];
  const seenTasks = new Set<string>();
  for (const item of significant) {
    if (seenTasks.has(item.task_id)) continue;
    seenTasks.add(item.task_id);
    visibleItems.push(item);
    if (visibleItems.length === 5) break;
  }
  return (
    <aside className="project-activity-rail" aria-label="项目动态">
      <div className="project-activity-head"><div><h2>项目动态</h2><p>每个对话的最新关键节点</p></div></div>
      <div className="project-activity-list">
        {visibleItems.length === 0 ? <p className="project-activity-empty">还没有可显示的关键动态。</p> : visibleItems.map((item) => (
          <button className="project-activity-item" key={item.id} onClick={() => openRoom(item.task_id)}>
            <i className={`task-state-dot ${activityTone(item)}`} />
            <span><strong>{item.summary}</strong><small>{item.task_title}{item.actor ? ` · ${item.actor}` : ""}</small></span>
            <time>{elapsedMinutes(item.at)}</time>
          </button>
        ))}
      </div>
    </aside>
  );
}
