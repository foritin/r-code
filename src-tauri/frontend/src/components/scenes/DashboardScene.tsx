import { useState } from "react";
import { permissionApprove } from "../../lib/ipc";
import { elapsedMinutes, elapsedSince, permissionRiskLabel } from "../../lib/format";
import { taskTitle } from "../../lib/presentation";
import { usePoll } from "../../lib/poll";
import { useTasksStore } from "../../store/tasks";
import { useAppStore } from "../../store/app";
import type {
  DashboardAttentionItem,
  DashboardTaskSummary,
  PermissionDecision,
  ProjectActivityItem,
  Task,
} from "../../lib/types";
import {
  IconActivity,
  IconArrowRight,
  IconCheck,
  IconEditor,
  IconFile,
  IconProjects,
  IconShield,
} from "../icons";

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
  const setCanvasTab = useAppStore((s) => s.setCanvasTab);
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
            <p className="page-kicker">PROJECT DASHBOARD</p>
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
  const completed = dashboard?.completed ?? [];

  return (
    <div className="scene scene-dashboard">
      <main className="dashboard-main">
        <div className="dashboard-scroll">
          <header className="dashboard-header">
            <div className="dashboard-project-mark"><IconProjects width={20} height={20} /></div>
            <div>
              <p className="page-kicker">PROJECT DASHBOARD</p>
              <h1>{workspace.display_name}</h1>
              <p>任务、变更与验证都围绕这个项目展开。</p>
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
            <Metric label="近 1 小时完成" value={metrics?.completed_last_hour_count ?? 0} />
          </section>

          {error && <div className="dashboard-error" role="alert">{error}</div>}

          {attention.length > 0 && (
            <section className="dashboard-section dashboard-attention-section">
              <div className="dashboard-section-title">
                <div><p className="section-kicker">NEEDS YOU</p><h2>需要你处理</h2></div>
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
              <div><p className="section-kicker">TASKS</p><h2>项目任务</h2></div>
              <span className="section-meta">{metrics?.task_count ?? taskSummaries.length} 个任务</span>
            </div>
            {taskSummaries.length === 0 ? (
              <div className="dashboard-blank-row">这个项目还没有任务。创建一个任务，仪表盘会在这里开始积累进度。</div>
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

          <section className="dashboard-section dashboard-completed-section">
            <div className="dashboard-section-title">
              <div><p className="section-kicker">VERIFICATION</p><h2>最近完成</h2></div>
            </div>
            <div className="dashboard-completed-list">
              {completed.slice(0, 3).map((summary) => {
                const latest = summary.latest_verification;
                return (
                  <button className="completed-row" key={summary.task.id} onClick={() => openRoom(summary.task.id)}>
                    <IconCheck width={16} height={16} />
                    <strong>{taskTitle(summary.task)}</strong>
                    <span>{latest ? `${latest.command} · ${latest.status === "passed" ? "已通过" : latest.status}` : "任务已完成"}</span>
                    <time>{elapsedMinutes(summary.task.updated_at)}</time>
                  </button>
                );
              })}
              {!completed.length && <div className="dashboard-blank-row compact">最近还没有完成记录。</div>}
            </div>
          </section>
        </div>
      </main>
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
  const setCanvasTab = useAppStore((s) => s.setCanvasTab);
  return (
    <article className="attention-item review-item">
      <span className="attention-icon"><IconFile width={18} height={18} /></span>
      <div className="attention-copy">
        <p><span className="attention-label">等待审核</span></p>
        <h3>{taskTitle(item.task)}</h3>
        <small>{summary?.change_summary.files ?? 0} 个文件变更 · 等待 {elapsedSince(item.since)}</small>
      </div>
      <div className="attention-actions">
        <button className="rc-button rc-button-primary" onClick={() => { setCanvasTab("review"); openRoom(item.task.id); }}>查看审核</button>
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
  // 仪表盘只保留能改变任务判断的节点；逐次工具调用仍可在任务详情和全局活动页查看。
  // 若后端暂时只返回底层事件，则回退显示最近几条，避免产生错误的空态。
  const significant = items.filter((item) => !["state_changed", "queue_dispatched", "tool_call", "tool_result", "system"].includes(item.kind));
  const visibleItems = (significant.length > 0 ? significant : items).slice(0, 8);
  return (
    <aside className="project-activity-rail" aria-label="项目动态">
      <div className="project-activity-head"><div><p className="section-kicker">PROJECT ACTIVITY</p><h2>项目动态</h2></div><IconActivity width={18} height={18} /></div>
      <div className="project-activity-list">
        {visibleItems.length === 0 ? <p className="project-activity-empty">任务产生运行、变更或验证后，动态会显示在这里。</p> : visibleItems.map((item) => (
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
