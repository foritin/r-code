import { useCallback, useMemo, useState } from "react";
import { taskDelete, taskList, taskRestore } from "../../lib/ipc";
import { taskTitle, workspaceName } from "../../lib/presentation";
import { usePoll } from "../../lib/poll";
import type { Task } from "../../lib/types";
import { useAppStore } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { pushToast } from "../../store/toast";
import { IconArchive, IconArrowRight, IconRestore, IconSearch, IconTrash } from "../icons";
import { ConfirmDialog } from "../ui/ConfirmDialog";

/** 全局归档库：集中查看只读历史，并把恢复设为最显眼的操作。 */
export function ArchiveScene() {
  const workspaces = useTasksStore((state) => state.workspaces);
  const setScene = useAppStore((state) => state.setScene);
  const [archived, setArchived] = useState<Task[]>([]);
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const tasks = await taskList(undefined, true);
      setArchived(tasks
        .filter((task) => task.state === "archived")
        .sort((left, right) => right.updated_at.localeCompare(left.updated_at)));
      setError(null);
    } catch (cause) {
      setError(`无法读取归档：${String(cause)}`);
    } finally {
      setLoading(false);
    }
  }, []);

  usePoll(load, 5_000, true, "归档");

  const filtered = useMemo(() => {
    const normalized = query.trim().toLocaleLowerCase();
    if (!normalized) return archived;
    return archived.filter((task) => `${taskTitle(task)} ${task.goal} ${workspaceName(task.workspace_path, workspaces)}`.toLocaleLowerCase().includes(normalized));
  }, [archived, query, workspaces]);

  return (
    <div className="scene scene-archive">
      <div className="archive-page">
        <header className="archive-header">
          <div><h1>归档</h1><p>归档对话保持只读，不会出现在项目任务或活动中。</p></div>
          <span>{archived.length} 个对话</span>
        </header>

        <div className="archive-toolbar">
          <label className="archive-search"><IconSearch width={16} height={16} /><input aria-label="搜索归档" value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索对话或项目" /></label>
          <button className="text-link" onClick={() => setScene("conversations")}>全部对话 <IconArrowRight width={14} height={14} /></button>
        </div>

        {error && <div className="archive-error" role="alert"><span>{error}</span><button onClick={() => void load()}>重试</button></div>}

        <section className="archive-list" aria-label="已归档对话">
          {!loading && filtered.length > 0 && <div className="archive-list-head" aria-hidden="true"><span>对话</span><span>项目</span><span>归档时间</span><span>操作</span></div>}
          {loading ? (
            <div className="archive-empty" role="status">正在读取归档…</div>
          ) : filtered.length === 0 ? (
            <div className="archive-empty"><IconArchive width={20} height={20} /><strong>{query.trim() ? "没有匹配的归档" : "还没有归档对话"}</strong><p>{query.trim() ? "换一个关键词。" : "归档后的对话会集中显示在这里。"}</p></div>
          ) : filtered.map((task) => <ArchiveRow key={task.id} task={task} onChanged={load} />)}
        </section>
      </div>
    </div>
  );
}

function archiveTime(iso: string): string {
  const value = new Date(iso);
  if (Number.isNaN(value.getTime())) return "—";
  return new Intl.DateTimeFormat("zh-CN", {
    year: value.getFullYear() === new Date().getFullYear() ? undefined : "numeric",
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(value);
}

function ArchiveRow({ task, onChanged }: { task: Task; onChanged: () => Promise<void> }) {
  const workspaces = useTasksStore((state) => state.workspaces);
  const refreshTasks = useTasksStore((state) => state.refreshTasks);
  const openRoom = useAppStore((state) => state.openRoom);
  const forgetTaskNavigation = useAppStore((state) => state.forgetTaskNavigation);
  const [busy, setBusy] = useState<"restore" | "delete" | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const title = taskTitle(task);

  const restore = async () => {
    if (busy) return;
    setBusy("restore");
    try {
      await taskRestore(task.id);
      await Promise.all([refreshTasks(), onChanged()]);
      pushToast({ kind: "success", title: "对话已还原", body: title });
    } catch (cause) {
      pushToast({ kind: "error", title: "无法还原对话", body: String(cause) });
    } finally {
      setBusy(null);
    }
  };

  const remove = async () => {
    if (busy) return;
    setBusy("delete");
    try {
      await taskDelete(task.id);
      forgetTaskNavigation(task.id);
      await Promise.all([refreshTasks(), onChanged()]);
      setConfirmDelete(false);
      pushToast({ kind: "success", title: "对话已永久删除", body: title });
    } catch (cause) {
      pushToast({ kind: "error", title: "无法删除对话", body: String(cause) });
    } finally {
      setBusy(null);
    }
  };

  return (
    <>
      <article className="archive-row">
        <button className="archive-row-main" onClick={() => openRoom(task.id)}><strong>{title}</strong><small>打开只读历史</small></button>
        <span className="archive-row-project">{workspaceName(task.workspace_path, workspaces)}</span>
        <time dateTime={task.updated_at}>{archiveTime(task.updated_at)}</time>
        <div className="archive-row-actions">
          <button className="archive-restore" disabled={busy != null} onClick={() => void restore()}><IconRestore width={14} height={14} />{busy === "restore" ? "还原中…" : "还原"}</button>
          <button className="archive-delete" aria-label={`永久删除 ${title}`} title="永久删除" disabled={busy != null} onClick={() => setConfirmDelete(true)}><IconTrash width={14} height={14} /></button>
        </div>
      </article>
      <ConfirmDialog
        open={confirmDelete}
        title="永久删除这段对话？"
        description={`“${title}”的消息、运行记录与审计历史会被永久删除。项目目录和其中的文件不会被删除。`}
        confirmLabel="永久删除"
        busy={busy === "delete"}
        onCancel={() => { if (!busy) setConfirmDelete(false); }}
        onConfirm={() => void remove()}
      />
    </>
  );
}
