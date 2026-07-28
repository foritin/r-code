import { useMemo } from "react";
import { elapsedMinutes } from "../../lib/format";
import { isTaskLive, pendingPermissionCount, sortTasksByUrgency, taskActivity, taskStateLabel, taskTitle, visualTaskState, workspaceName } from "../../lib/presentation";
import { usePoll } from "../../lib/poll";
import { selectNeedsYou, selectRunning, useTasksStore } from "../../store/tasks";
import { useAppStore } from "../../store/app";
import type { ProjectActivityItem, Task } from "../../lib/types";
import { IconActivity, IconArrowRight, IconCheck, IconProjects, IconShield } from "../icons";

/** 全局活动页：跨项目的概览，不含项目专属的活动侧栏。 */
export function ActivityScene() {
  const tasks = useTasksStore((s) => s.tasks);
  const details = useTasksStore((s) => s.details);
  const workspaces = useTasksStore((s) => s.workspaces);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const refreshDetails = useTasksStore((s) => s.refreshDetails);
  const refreshActivity = useTasksStore((s) => s.refreshActivity);
  const activityPage = useTasksStore((s) => s.activityPage);
  const running = useTasksStore(selectRunning);
  const needsYou = useTasksStore(selectNeedsYou);
  const openRoom = useAppStore((s) => s.openRoom);
  const setScene = useAppStore((s) => s.setScene);

  usePoll(async () => {
    await refreshTasks();
    const ids = useTasksStore.getState().tasks.filter((task) => task.state !== "idle" && task.state !== "archived").map((task) => task.id);
    await Promise.all([refreshActivity(), ids.length ? refreshDetails(ids) : Promise.resolve()]);
  }, 2500);

  const activeTasks = useMemo(() => sortTasksByUrgency(tasks.filter((task) => task.state !== "archived"), details), [details, tasks]);
  const recentEvents = activityPage?.items ?? [];
  const completed = tasks.filter((task) => task.state === "idle").length;

  return (
    <div className="scene scene-activity">
      <div className="activity-page">
        <header className="activity-header">
          <div><p className="page-kicker">ACTIVITY</p><h1>活动</h1><p>跨项目掌握正在推进、需要决定和刚刚完成的工作。</p></div>
          <button className="rc-button rc-button-quiet" onClick={() => setScene("conversations")}>全部对话 <IconArrowRight width={14} height={14} /></button>
        </header>

        <section className="activity-summary" aria-label="工作概览">
          <Summary value={needsYou.length} label="待处理" tone="warm" />
          <Summary value={running.length} label="运行中" tone="success" />
          <Summary value={tasks.reduce((total, task) => total + (details[task.id]?.runs.filter((run) => run.agent_kind === "subagent" && !run.ended_at).length ?? 0), 0)} label="活跃子代理" />
          <Summary value={completed} label="已完成" />
        </section>

        <section className="activity-section">
          <div className="activity-section-head"><div><p className="section-kicker">IN PROGRESS</p><h2>正在推进</h2></div><span>{activeTasks.filter((task) => isTaskLive(task, details[task.id])).length} 个任务</span></div>
          <div className="activity-work-list">
            {activeTasks.filter((task) => isTaskLive(task, details[task.id])).length === 0 ? <p className="activity-empty">没有正在运行的任务。</p> : activeTasks.filter((task) => isTaskLive(task, details[task.id])).slice(0, 8).map((task) => <ActivityTaskRow key={task.id} task={task} />)}
          </div>
        </section>

        {needsYou.length > 0 && <section className="activity-section activity-needs-section"><div className="activity-section-head"><div><p className="section-kicker">NEEDS YOU</p><h2>等待决定</h2></div><button className="text-link" onClick={() => setScene("inbox")}>打开待处理 <IconArrowRight width={14} height={14} /></button></div><div className="activity-needs-list">{needsYou.slice(0, 4).map((item) => <button className="activity-need-row" key={item.kind === "permission" ? item.permission!.id : item.task.id} onClick={() => setScene("inbox")}><span>{item.kind === "permission" ? <IconShield width={16} height={16} /> : <IconCheck width={16} height={16} />}</span><strong>{taskTitle(item.task)}</strong><small>{item.kind === "permission" ? `权限请求 · ${item.permission!.tool_name}` : "等待审核"}</small><time>{elapsedMinutes(item.since)}</time></button>)}</div></section>}

        <section className="activity-section activity-recent-section">
          <div className="activity-section-head"><div><p className="section-kicker">RECENTLY</p><h2>最近动态</h2></div></div>
          <div className="activity-recent-list">{recentEvents.length === 0 ? <p className="activity-empty">任务的运行记录会在这里显示。</p> : recentEvents.slice(0, 9).map((item) => <ActivityEventRow key={item.id} item={item} />)}</div>
        </section>
      </div>
    </div>
  );
}

function Summary({ value, label, tone }: { value: number; label: string; tone?: "warm" | "success" }) { return <div className={`activity-summary-metric${tone ? ` ${tone}` : ""}`}><strong>{value}</strong><span>{label}</span></div>; }

function ActivityTaskRow({ task }: { task: Task }) {
  const detail = useTasksStore((s) => s.details[task.id]);
  const workspaces = useTasksStore((s) => s.workspaces);
  const openRoom = useAppStore((s) => s.openRoom);
  const waiting = pendingPermissionCount(detail);
  return <button className="activity-work-row" onClick={() => openRoom(task.id)}><i className={`task-state-dot ${visualTaskState(task, detail)}`} /><span><strong>{taskTitle(task)}</strong><small>{taskActivity(task, detail)}</small></span><em>{workspaceName(task.workspace_path, workspaces)}</em><b>{waiting ? `等待 ${waiting} 项授权` : taskStateLabel(task.state, detail)}</b><IconArrowRight width={15} height={15} /></button>;
}

function ActivityEventRow({ item }: { item: ProjectActivityItem }) {
  const openRoom = useAppStore((s) => s.openRoom);
  const tone = item.kind === "permission_requested" ? "attention" : item.kind === "change_requested" ? "review" : item.kind === "run_ended" || item.kind === "verification_run" ? "done" : "running";
  return <button className="activity-recent-row" onClick={() => openRoom(item.task_id)}><i className={`task-state-dot ${tone}`} /><span><strong>{item.summary}</strong><small>{item.task_title} · <IconProjects width={12} height={12} /> {item.actor ?? "主代理"}</small></span><time>{elapsedMinutes(item.at)}</time></button>;
}
