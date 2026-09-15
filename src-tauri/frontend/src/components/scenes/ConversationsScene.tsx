import { useEffect, useMemo, useState } from "react";
import { taskList } from "../../lib/ipc";
import { elapsedMinutes } from "../../lib/format";
import { sortTasksByUrgency, taskActivity, taskDisplayState, taskStateLabel, taskTitle, visualTaskState, workspaceName } from "../../lib/presentation";
import { usePoll } from "../../lib/poll";
import { selectNeedsYouTaskIds, useTasksStore } from "../../store/tasks";
import { useAppStore } from "../../store/app";
import type { Task } from "../../lib/types";
import { IconHistory, IconPlus, IconSearch } from "../icons";
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
    const snapshot = useTasksStore.getState();
    const ids = snapshot.tasks
      .filter((task) => task.state !== "archived")
      .filter((task) => {
        const detail = snapshot.details[task.id];
        return !detail?.status
          || detail.task.updated_at !== task.updated_at
          || detail.status.active_run_id != null;
      })
      .map((task) => task.id);
    if (ids.length) await refreshDetails(ids);
  }, 2500);

  const filtered = useMemo(() => {
    const normalized = query.trim().toLocaleLowerCase();
    const source = filter === "archived" ? archivedTasks : tasks;
    return sortTasksByUrgency(source, details).filter((task) => {
      const visual = visualTaskState(task, details[task.id]);
      const display = taskDisplayState(task, details[task.id]);
      const matchesFilter = filter === "all"
        || (filter === "running" && visual === "running")
        || (filter === "attention" && visual === "attention")
        || (filter === "review" && visual === "review")
        || (filter === "completed" && visual === "done")
        || (filter === "archived" && display === "archived");
      const haystack = `${taskTitle(task)} ${task.goal} ${workspaceName(task.workspace_path, workspaces)}`.toLocaleLowerCase();
      return matchesFilter && (!normalized || haystack.includes(normalized));
    });
  }, [archivedTasks, details, filter, query, tasks, workspaces]);

  return (
    <div className="scene scene-conversations">
      <div className="opt-page">
        <header className="opt-page-head">
          <div>
            <h1>所有对话</h1>
            <p>跨项目查看任务状态，在需要时回到具体任务继续处理。</p>
          </div>
          <button className="opt-button primary" onClick={() => openNewConversation(null)}><IconPlus width={16} height={16} />新对话</button>
        </header>

        <div className="opt-toolbar">
          <label className="opt-search-field"><IconSearch width={16} height={16} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="筛选任务或项目…" /></label>
          <div className="opt-tabs" role="tablist" aria-label="任务筛选">
            {([
              ["all", "全部"], ["running", "运行中"], ["attention", "待处理"], ["review", "待审核"], ["completed", "已完成"], ["archived", "已归档"],
            ] as [Filter, string][]).map(([value, label]) => (
              <button key={value} role="tab" aria-selected={filter === value} className={filter === value ? "active" : ""} onClick={() => setFilter(value)}>{label}</button>
            ))}
          </div>
        </div>

        <section aria-label="任务列表">
          {archivedLoading || filtered.length === 0 ? (
            <div className="opt-empty">
              <span className="opt-icon"><IconHistory width={20} height={20} /></span>
              <h3>{archivedLoading ? "正在读取归档…" : "没有匹配的对话"}</h3>
              {!archivedLoading && <p>换一个筛选条件，或从新对话开始。</p>}
            </div>
          ) : (
            <table className="opt-table" aria-label="任务列表">
              <thead>
                <tr><th>任务</th><th>项目</th><th>状态</th><th>最近更新</th><th className="opt-last"><span className="sr-only">操作</span></th></tr>
              </thead>
              <tbody>
                {filtered.map((task) => (
                  <ConversationRow
                    key={task.id}
                    task={task}
                    needsAttention={needsIds.has(task.id)}
                    onChanged={filter === "archived" ? () => setArchivedRevision((value) => value + 1) : undefined}
                  />
                ))}
              </tbody>
            </table>
          )}
          <div className="opt-home-foot">
            <span>{filtered.length} 个对话 · 最近活动排列</span>
            <span>选中任务可继续对话或查看详情</span>
          </div>
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
  const highlighted = needsAttention || visual === "attention";
  return (
    <tr className="conversation-row">
      <td>
        <button className="text-link conversation-main" onClick={() => openRoom(task.id)}><strong>{taskTitle(task)}</strong></button>
        <small>{taskActivity(task, detail)}</small>
      </td>
      <td>{workspaceName(task.workspace_path, workspaces)}</td>
      <td><span className={`conversation-status ${visual}`}><i /></span><span className={`conversation-state${highlighted ? " needs" : ""}`}>{taskStateLabel(task, detail)}</span></td>
      <td><time>{elapsedMinutes(task.updated_at)}</time></td>
      <td className="opt-last"><TaskActionsMenu task={task} detail={detail} onChanged={onChanged} /></td>
    </tr>
  );
}
