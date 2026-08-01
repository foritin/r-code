import { useEffect, useState } from "react";
import { useTasksStore } from "../../store/tasks";
import { pushToast } from "../../store/toast";
import { legacyMemoryStatus, taskList, workspaceChoose, workspaceForget } from "../../lib/ipc";
import { displayPath } from "../../lib/format";
import type { LegacyMemoryStatus, Workspace } from "../../lib/types";
import { IconAttach, IconCheck, IconClose, IconProjects } from "../icons";
import { projectAccessModeLabel } from "../ProjectAccessSelector";
import { ConfirmDialog } from "../ui/ConfirmDialog";

interface PendingProjectRemoval {
  workspace: Workspace;
  sessionCount: number;
}

/**
 * 工作区库：系统文件夹选择是唯一主入口；手工路径不再要求用户输入。
 * 当前工作区只是「下一个会话的可选范围」，不会强制用户离开纯聊天。
 */
export function ProjectsScene() {
  const workspaces = useTasksStore((s) => s.workspaces);
  const tasks = useTasksStore((s) => s.tasks);
  const currentWorkspacePath = useTasksStore((s) => s.currentProjectId);
  const setCurrentWorkspace = useTasksStore((s) => s.setCurrentProject);
  const refreshWorkspaces = useTasksStore((s) => s.refreshWorkspaces);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const refreshActivity = useTasksStore((s) => s.refreshActivity);

  const [opening, setOpening] = useState(false);
  const [removing, setRemoving] = useState(false);
  const [checkingRemoval, setCheckingRemoval] = useState<string | null>(null);
  const [pendingRemoval, setPendingRemoval] = useState<PendingProjectRemoval | null>(null);
  const [error, setError] = useState<string | null>(null);

  const chooseWorkspace = async () => {
    if (opening) return;
    setOpening(true);
    setError(null);
    try {
      const workspace = await workspaceChoose();
      if (!workspace) return;
      await refreshWorkspaces();
      setCurrentWorkspace(workspace.canonical_path);
    } catch (cause) {
      setError(`打开工作区失败：${String(cause)}`);
    } finally {
      setOpening(false);
    }
  };

  const prepareRemoval = async (workspace: Workspace) => {
    if (checkingRemoval || removing) return;
    setCheckingRemoval(workspace.canonical_path);
    try {
      const sessions = await taskList(workspace.canonical_path, true);
      setPendingRemoval({ workspace, sessionCount: sessions.length });
    } catch (cause) {
      pushToast({ kind: "error", title: "无法检查项目记录", body: String(cause) });
    } finally {
      setCheckingRemoval(null);
    }
  };

  const removeWorkspace = async () => {
    if (!pendingRemoval || removing) return;
    setRemoving(true);
    try {
      const result = await workspaceForget(pendingRemoval.workspace.canonical_path);
      await Promise.all([refreshWorkspaces(), refreshTasks(), refreshActivity()]);
      pushToast({
        kind: "success",
        title: "项目已从 R-Code 清除",
        body: `${pendingRemoval.workspace.display_name} · 已清除 ${result.removed_sessions} 段对话 · 磁盘文件未更改`,
      });
      setPendingRemoval(null);
    } catch (cause) {
      pushToast({ kind: "error", title: "无法清除项目", body: String(cause) });
    } finally {
      setRemoving(false);
    }
  };

  const currentWorkspace = workspaces.find((workspace) => workspace.canonical_path === currentWorkspacePath);

  return (
    <div className="scene scene-projects">
      <div className="scene-scroll projects-scroll">
        <header className="workspace-hero">
          <div>
            <div className="section-label">WORKSPACES</div>
            <h1>需要本地上下文时，再附加文件夹。</h1>
            <p>工作区决定 R-Code 可访问的文件范围；不附加也可以继续聊天。</p>
          </div>
          <button className="btn accent workspace-choose" disabled={opening} onClick={() => void chooseWorkspace()}>
            <IconAttach width={14} height={14} />
            {opening ? "正在打开…" : "选择文件夹"}
          </button>
        </header>

        {error && <div className="errbar">{error}</div>}

        <section className="workspace-library" aria-label="最近工作区">
          <div className="workspace-section-head">
            <span>最近使用</span>
            <span>{workspaces.length} 个文件夹</span>
          </div>
          {workspaces.length === 0 ? (
            <div className="workspace-empty">
              <IconProjects width={20} height={20} />
              <p>尚未附加过文件夹。</p>
              <button className="quiet-link" onClick={() => void chooseWorkspace()}>从系统选择器开始</button>
            </div>
          ) : (
            <div className="workspace-list">
              {workspaces.map((workspace) => {
                const isCurrent = workspace.canonical_path === currentWorkspacePath;
                const hasLiveTask = tasks.some(
                  (task) => task.workspace_path === workspace.canonical_path
                    && (task.state === "exploring" || task.state === "in_progress"),
                );
                return (
                  <article className={`workspace-row${isCurrent ? " current" : ""}`} key={workspace.canonical_path}>
                    <button
                      className="workspace-main"
                      onClick={() => setCurrentWorkspace(isCurrent ? null : workspace.canonical_path)}
                      title={isCurrent ? "取消附加到新对话" : "附加到下一个新对话"}
                    >
                      <span className="workspace-glyph"><IconProjects width={16} height={16} /></span>
                      <span className="workspace-copy">
                        <strong>{workspace.display_name}</strong>
                        <small title={workspace.canonical_path}>{displayPath(workspace.canonical_path)}</small>
                      </span>
                      {isCurrent && <span className="current-mark"><IconCheck width={13} height={13} /> 已附加</span>}
                    </button>
                    <div className="workspace-actions">
                      <span className="workspace-access scoped">
                        {projectAccessModeLabel(workspace.access_mode)}
                      </span>
                      <button
                        type="button"
                        className="workspace-remove"
                        disabled={hasLiveTask || checkingRemoval === workspace.canonical_path}
                        onClick={() => void prepareRemoval(workspace)}
                        title={hasLiveTask ? "先停止该项目中正在运行的会话" : "清除 R-Code 中的项目记录，不影响磁盘文件"}
                        aria-label={`从 R-Code 中清除 ${workspace.display_name}`}
                      >
                        <IconClose width={13} height={13} />
                        {checkingRemoval === workspace.canonical_path ? "正在检查…" : "清除项目"}
                      </button>
                    </div>
                  </article>
                );
              })}
            </div>
          )}
        </section>

        <MemorySection workspacePath={currentWorkspace?.canonical_path ?? null} />
      </div>
      <ConfirmDialog
        open={pendingRemoval != null}
        title="从 R-Code 中清除这个项目？"
        description={`这会永久清除“${pendingRemoval?.workspace.display_name ?? "该项目"}”在 R-Code 中的项目记录、${pendingRemoval?.sessionCount ?? 0} 段对话以及关联的运行与审计数据。真实文件夹及其中的文件不会被删除、移动或修改。`}
        confirmLabel="清除项目"
        busyLabel="正在清除…"
        busy={removing}
        onCancel={() => {
          if (!removing) setPendingRemoval(null);
        }}
        onConfirm={() => void removeWorkspace()}
      />
    </div>
  );
}

function MemorySection({ workspacePath }: { workspacePath: string | null }) {
  const [status, setStatus] = useState<LegacyMemoryStatus | null>(null);
  const [loading, setLoading] = useState(false);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let alive = true;
    setStatus(null);
    setFailed(false);
    if (!workspacePath) {
      setLoading(false);
      return () => {
        alive = false;
      };
    }
    setLoading(true);
    void legacyMemoryStatus(workspacePath)
      .then((nextStatus) => {
        if (alive) {
          setStatus(nextStatus);
          setLoading(false);
        }
      })
      .catch(() => {
        if (alive) {
          setFailed(true);
          setLoading(false);
        }
      });
    return () => {
      alive = false;
    };
  }, [workspacePath]);

  return (
    <section className="workspace-memory">
      <div>
        <div className="section-label">MEMORY SAFETY</div>
        <h2>记忆与旧版文件</h2>
        <p>新的 Memory Center 尚未启用。启用后，数据只保存在 R-Code AppData，不会写入项目目录。</p>
      </div>
      {!workspacePath ? (
        <div className="memory-placeholder">先从上方附加一个工作区，以检查旧版记忆文件的风险状态。</div>
      ) : loading ? (
        <div className="memory-placeholder">正在检查旧版记忆文件状态…</div>
      ) : failed ? (
        <div className="legacy-memory-status unknown">
          <strong>无法检查旧版记忆文件状态</strong>
          <p>R-Code 没有读取或修改项目文件，请稍后重试。</p>
        </div>
      ) : status ? (
        <LegacyMemoryNotice status={status} />
      ) : (
        <div className="memory-placeholder">没有可用的旧版记忆文件状态。</div>
      )}
    </section>
  );
}

function LegacyMemoryNotice({ status }: { status: LegacyMemoryStatus }) {
  if (status.git_tracking === "tracked") {
    return (
      <div className="legacy-memory-status tracked" role="alert">
        <strong>{status.exists ? "旧版记忆文件可能已进入 Git 历史" : "工作树中未发现旧版记忆文件，但 Git 仍有跟踪记录"}</strong>
        <p>{status.exists ? "检测到该文件当前受 Git 跟踪。" : "该文件当前不在工作树中，但 Git 索引仍记录它，历史也可能保留内容。"} R-Code 不会读取、导入、修改或删除 <code>.r-code/memory.md</code>，也不会自动执行 git rm 或取消跟踪；请自行审查 Git 索引及历史。</p>
      </div>
    );
  }

  if (status.git_tracking === "unknown") {
    return (
      <div className="legacy-memory-status unknown">
        <strong>无法检测旧版记忆文件的 Git 跟踪状态</strong>
        <p>{status.exists ? "工作树中发现了旧版记忆文件。" : "工作树中未发现旧版记忆文件，但无法据此判断 Git 历史中是否保留过它。"} R-Code 不会读取、导入、修改或删除 <code>.r-code/memory.md</code>；请在 R-Code 之外自行审查。</p>
      </div>
    );
  }

  if (!status.exists) {
    return (
      <div className="legacy-memory-status absent">
        <strong>未发现旧版记忆文件</strong>
        <p>当前工作树中没有 <code>.r-code/memory.md</code>，Git 索引也未跟踪该路径。R-Code 未检查 Git 历史；若过去曾提交，仍需自行审查历史，R-Code 不会自动操作。</p>
      </div>
    );
  }

  return (
    <div className="legacy-memory-status untracked">
      <strong>发现未被 Git 跟踪的旧版记忆文件</strong>
      <p>R-Code 不会读取、导入、修改或删除 <code>.r-code/memory.md</code>；请在 R-Code 之外自行审查与处置。</p>
    </div>
  );
}
