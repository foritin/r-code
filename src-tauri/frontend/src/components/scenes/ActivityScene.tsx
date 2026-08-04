import { useMemo } from "react";
import { elapsedMinutes } from "../../lib/format";
import {
  isTaskLive,
  sortTasksByUrgency,
  taskActivity,
  taskStateLabel,
  taskTitle,
  visualTaskState,
  workspaceName,
} from "../../lib/presentation";
import { usePoll } from "../../lib/poll";
import type { Task } from "../../lib/types";
import { useAppStore } from "../../store/app";
import { selectNeedsYou, useTasksStore, type NeedsYouItem } from "../../store/tasks";
import { IconArrowRight } from "../icons";

/** 跨项目工作摘要：只展示可行动状态与每段对话的最新结果，不复述底层事件流水。 */
export function ActivityScene() {
  const tasks = useTasksStore((state) => state.tasks);
  const details = useTasksStore((state) => state.details);
  const refreshTasks = useTasksStore((state) => state.refreshTasks);
  const refreshDetails = useTasksStore((state) => state.refreshDetails);
  const needsYou = useTasksStore(selectNeedsYou);
  const setScene = useAppStore((state) => state.setScene);

  usePoll(async () => {
    await refreshTasks();
    const ids = useTasksStore.getState().tasks
      .filter((task) => task.state !== "idle" && task.state !== "archived")
      .map((task) => task.id);
    if (ids.length) await refreshDetails(ids);
  }, 2_500, true, "工作进展");

  const { liveTasks, recentTasks } = useMemo(() => {
    const attentionIds = new Set(needsYou.map((item) => item.task.id));
    const ordered = sortTasksByUrgency(tasks, details);
    return {
      // 等待用户决策的运行会话只出现在第一段，避免同一件事重复两次。
      liveTasks: ordered.filter((task) => isTaskLive(task, details[task.id]) && !attentionIds.has(task.id)),
      recentTasks: [...tasks]
        .filter((task) => task.state === "idle" || task.state === "interrupted")
        .sort((left, right) => right.updated_at.localeCompare(left.updated_at))
        .slice(0, 8),
    };
  }, [details, needsYou, tasks]);

  return (
    <div className="scene scene-activity">
      <div className="activity-page">
        <header className="activity-header">
          <div><h1>活动</h1><p>需要你处理、正在运行和最近结束的对话。</p></div>
          <button className="rc-button rc-button-quiet" onClick={() => setScene("conversations")}>全部对话 <IconArrowRight width={14} height={14} /></button>
        </header>

        <ActivitySection title="需要处理" count={needsYou.length} action={needsYou.length ? <button className="text-link" onClick={() => setScene("inbox")}>查看全部 <IconArrowRight width={14} height={14} /></button> : null}>
          <div className="activity-needs-list">
            {needsYou.length === 0
              ? <p className="activity-empty">当前没有需要你处理的事项。</p>
              : needsYou.slice(0, 5).map((item) => <ActivityNeedRow key={item.kind === "permission" ? item.permission!.id : item.task.id} item={item} />)}
          </div>
        </ActivitySection>

        <ActivitySection title="正在进行" count={liveTasks.length}>
          <div className="activity-work-list">
            {liveTasks.length === 0
              ? <p className="activity-empty">当前没有后台任务在运行。</p>
              : liveTasks.slice(0, 8).map((task) => <ActivityTaskRow key={task.id} task={task} />)}
          </div>
        </ActivitySection>

        <ActivitySection title="最近结束" count={recentTasks.length}>
          <div className="activity-recent-list">
            {recentTasks.length === 0
              ? <p className="activity-empty">完成或停止的对话会显示在这里。</p>
              : recentTasks.map((task) => <RecentTaskRow key={task.id} task={task} />)}
          </div>
        </ActivitySection>
      </div>
    </div>
  );
}

function ActivitySection({ title, count, action, children }: { title: string; count: number; action?: React.ReactNode; children: React.ReactNode }) {
  return (
    <section className="activity-section">
      <div className="activity-section-head">
        <div><h2>{title}</h2><span>{count}</span></div>
        {action}
      </div>
      {children}
    </section>
  );
}

function ActivityNeedRow({ item }: { item: NeedsYouItem }) {
  const details = useTasksStore((state) => state.details);
  const workspaces = useTasksStore((state) => state.workspaces);
  const openRoom = useAppStore((state) => state.openRoom);
  const detail = details[item.task.id];
  const description = item.kind === "permission"
    ? `等待授权 · ${item.permission!.tool_name}`
    : `${detail?.changes.length ?? 0} 个文件等待审核`;

  return (
    <button className="activity-need-row" onClick={() => openRoom(item.task.id, item.kind === "review_ready" ? "review" : undefined)}>
      <i className={`task-state-dot ${item.kind === "permission" ? "attention" : "review"}`} />
      <span><strong>{taskTitle(item.task)}</strong><small>{description}</small></span>
      <em>{workspaceName(item.task.workspace_path, workspaces)}</em>
      <time>{elapsedMinutes(item.since)}</time>
      <IconArrowRight width={15} height={15} />
    </button>
  );
}

function ActivityTaskRow({ task }: { task: Task }) {
  const detail = useTasksStore((state) => state.details[task.id]);
  const workspaces = useTasksStore((state) => state.workspaces);
  const openRoom = useAppStore((state) => state.openRoom);
  const subagentCount = detail?.runs.filter((run) => run.agent_kind === "subagent" && !run.ended_at).length ?? 0;
  const status = subagentCount > 0 ? `${taskStateLabel(task.state, detail)} · ${subagentCount} 个子代理` : taskStateLabel(task.state, detail);

  return (
    <button className="activity-work-row" onClick={() => openRoom(task.id)}>
      <i className={`task-state-dot ${visualTaskState(task, detail)}`} />
      <span><strong>{taskTitle(task)}</strong><small>{taskActivity(task, detail)}</small></span>
      <em>{workspaceName(task.workspace_path, workspaces)}</em>
      <b>{status}</b>
      <IconArrowRight width={15} height={15} />
    </button>
  );
}

function RecentTaskRow({ task }: { task: Task }) {
  const detail = useTasksStore((state) => state.details[task.id]);
  const workspaces = useTasksStore((state) => state.workspaces);
  const openRoom = useAppStore((state) => state.openRoom);
  const latestVerification = detail?.verifications[detail.verifications.length - 1];
  const signal = task.state === "interrupted"
    ? "已停止，可继续"
    : latestVerification?.status === "passed"
      ? `验证通过${detail?.changes.length ? ` · ${detail.changes.length} 个文件` : ""}`
      : detail?.changes.length
        ? `${detail.changes.length} 个文件变更`
        : "已完成，可继续追问";

  return (
    <button className="activity-recent-row" onClick={() => openRoom(task.id)}>
      <i className={`task-state-dot ${visualTaskState(task, detail)}`} />
      <span><strong>{taskTitle(task)}</strong><small>{taskActivity(task, detail)}</small></span>
      <b>{signal}</b>
      <em>{workspaceName(task.workspace_path, workspaces)}</em>
      <time>{elapsedMinutes(task.updated_at)}</time>
      <IconArrowRight width={15} height={15} />
    </button>
  );
}
