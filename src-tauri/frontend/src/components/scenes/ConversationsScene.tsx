import { useEffect, useMemo, useState } from "react";
import { taskList } from "../../lib/ipc";
import { elapsedMinutes } from "../../lib/format";
import { isTaskLive, sortTasksByUrgency, taskActivity, taskStateLabel, taskTitle, visualTaskState, workspaceName } from "../../lib/presentation";
import { usePoll } from "../../lib/poll";
import { selectNeedsYouTaskIds, useTasksStore } from "../../store/tasks";
import { useAppStore } from "../../store/app";
import type { Task } from "../../lib/types";
import { IconHistory, IconPlus, IconProjects, IconSearch } from "../icons";
import { TaskActionsMenu } from "../TaskActionsMenu";

type Filter = "all" | "running" | "attention" | "review" | "completed" | "archived";

/** 跨项目的任务/会话列表。它是全局页，因此不渲染项目动态栏。 */
export function ConversationsScene() {
  const tasks = useTasksStore((s) => s.tasks);
  const details = useTasksStore((s) => s.details);
  const workspaces = useTasksStore((s) => s.workspaces);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const refreshDetails = useTasksStore((s) => s.refreshDetails);
  const needsIds = useTasksStore(selectNeedsYouTaskIds);
  const openNewConversation = useAppStore((s) => s.openNewConversation);
  const openRoom = useAppStore((s) => s.openRoom);
  const [filter, setFilter] = useState<Filter>("all");
  const [query, setQuery] = useState("");
  const [archivedTasks, setArchivedTasks] = useState<Task[]>([]);
  const [archivedLoading, setArchivedLoading] = useState(false);
  const [archivedRevision, setArchivedRevision] = useState(0);

  useEffect(() => {
    if (filter !== "archived") return;
    let cancelled = false;
    setArchivedLoading(true);
    void taskList(undefined, true)
      .then((allTasks) => {
        if (!cancelled) setArchivedTasks(allTasks.filter((task) => task.state === "archived"));
      })
      .finally(() => {
        if (!cancelled) setArchivedLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [archivedRevision, filter]);

  usePoll(async () => {
    await refreshTasks();
    const ids = useTasksStore.getState().tasks.filter((task) => task.state !== "idle" && task.state !== "archived").map((task) => task.id);
    if (ids.length) await refreshDetails(ids);
  }, 2500);

  const filtered = useMemo(() => {
    const normalized = query.trim().toLocaleLowerCase();
    const source = filter === "archived" ? archivedTasks : tasks;
    return sortTasksByUrgency(source, details).filter((task) => {
      const visual = visualTaskState(task, details[task.id]);
      const matchesFilter = filter === "all"
        || (filter === "running" && isTaskLive(task, details[task.id]))
        || (filter === "attention" && visual === "attention")
        || (filter === "review" && visual === "review")
        || (filter === "completed" && task.state === "idle")
        || (filter === "archived" && task.state === "archived");
      const haystack = `${taskTitle(task)} ${task.goal} ${workspaceName(task.workspace_path, workspaces)}`.toLocaleLowerCase();
      return matchesFilter && (!normalized || haystack.includes(normalized));
    });
  }, [archivedTasks, details, filter, query, tasks, workspaces]);

  return (
    <div className="scene scene-conversations">
      <div className="conversation-list-page">
        <header className="list-page-header">
          <div>
            <p className="page-kicker">CONVERSATIONS</p>
            <h1>所有对话</h1>
            <p>跨项目查看任务状态，在需要时回到具体任务继续处理。</p>
          </div>
          <button className="rc-button rc-button-primary" onClick={() => openNewConversation(null)}><IconPlus width={16} height={16} />新对话</button>
        </header>

        <div className="conversation-toolbar">
          <label className="conversation-search"><IconSearch width={16} height={16} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="筛选任务或项目…" /></label>
          <div className="conversation-filters" role="tablist" aria-label="任务筛选">
            {([
              ["all", "全部"], ["running", "运行中"], ["attention", "待处理"], ["review", "待审核"], ["completed", "已完成"], ["archived", "已归档"],
            ] as [Filter, string][]).map(([value, label]) => (
              <button key={value} role="tab" aria-selected={filter === value} className={filter === value ? "active" : ""} onClick={() => setFilter(value)}>{label}</button>
            ))}
          </div>
        </div>

        <section className="conversation-list" aria-label="任务列表">
          {archivedLoading ? (
            <div className="conversation-empty"><IconHistory width={24} height={24} /><h2>正在读取归档…</h2></div>
          ) : filtered.length === 0 ? (
            <div className="conversation-empty"><IconHistory width={24} height={24} /><h2>没有匹配的对话</h2><p>换一个筛选条件，或从新对话开始。</p></div>
          ) : filtered.map((task) => (
            <ConversationRow
              key={task.id}
              task={task}
              needsAttention={needsIds.has(task.id)}
              onChanged={filter === "archived" ? () => setArchivedRevision((value) => value + 1) : undefined}
            />
          ))}
        </section>
      </div>
    </div>
  );
}

function ConversationRow({ task, needsAttention, onChanged }: { task: Task; needsAttention: boolean; onChanged?: () => void }) {
  const detail = useTasksStore((s) => s.details[task.id]);
  const workspaces = useTasksStore((s) => s.workspaces);
  const openRoom = useAppStore((s) => s.openRoom);
  const visual = visualTaskState(task, detail);
  return (
    <article className="conversation-row">
      <span className={`conversation-status ${visual}`}><i /></span>
      <button className="conversation-main" onClick={() => openRoom(task.id)}><strong>{taskTitle(task)}</strong><small>{taskActivity(task, detail)}</small></button>
      <span className="conversation-project"><IconProjects width={15} height={15} />{workspaceName(task.workspace_path, workspaces)}</span>
      <span className={`conversation-state ${needsAttention ? "needs" : ""}`}>{taskStateLabel(task.state, detail)}</span>
      <time>{elapsedMinutes(task.updated_at)}</time>
      <span className="conversation-row-actions">
        <TaskActionsMenu task={task} detail={detail} onChanged={onChanged} />
      </span>
    </article>
  );
}
