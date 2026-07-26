import { useEffect, useState } from "react";
import { useTasksStore } from "../../store/tasks";
import { memoryGet, memorySet, workspaceChoose } from "../../lib/ipc";
import { displayPath } from "../../lib/format";
import { IconAttach, IconCheck, IconProjects } from "../icons";
import { projectAccessModeLabel } from "../ProjectAccessSelector";

/**
 * 工作区库：系统文件夹选择是唯一主入口；手工路径不再要求用户输入。
 * 当前工作区只是「下一个会话的可选范围」，不会强制用户离开纯聊天。
 */
export function ProjectsScene() {
  const workspaces = useTasksStore((s) => s.workspaces);
  const currentWorkspacePath = useTasksStore((s) => s.currentProjectId);
  const setCurrentWorkspace = useTasksStore((s) => s.setCurrentProject);
  const refreshWorkspaces = useTasksStore((s) => s.refreshWorkspaces);

  const [opening, setOpening] = useState(false);
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
                    </div>
                  </article>
                );
              })}
            </div>
          )}
        </section>

        <MemorySection workspacePath={currentWorkspace?.canonical_path ?? null} />
      </div>
    </div>
  );
}

function MemorySection({ workspacePath }: { workspacePath: string | null }) {
  const [text, setText] = useState("");
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    let alive = true;
    setText("");
    setError(null);
    setLoaded(false);
    if (!workspacePath) {
      setLoaded(true);
      return () => {
        alive = false;
      };
    }
    void memoryGet(workspacePath)
      .then((content) => {
        if (alive) {
          setText(content);
          setLoaded(true);
        }
      })
      .catch((cause) => {
        if (alive) {
          setError(String(cause));
          setLoaded(true);
        }
      });
    return () => {
      alive = false;
    };
  }, [workspacePath]);

  const save = async () => {
    if (!workspacePath || saving) return;
    setSaving(true);
    setError(null);
    try {
      await memorySet(workspacePath, text);
    } catch (cause) {
      setError(String(cause));
    } finally {
      setSaving(false);
    }
  };

  return (
    <section className="workspace-memory">
      <div>
        <div className="section-label">PROJECT MEMORY</div>
        <h2>项目记忆</h2>
        <p>保存在当前附加工作区的 <code>.r-code/memory.md</code> 中。</p>
      </div>
      {!workspacePath ? (
        <div className="memory-placeholder">先从上方选择一个工作区；这不会影响纯聊天会话。</div>
      ) : !loaded ? (
        <div className="memory-placeholder">读取中…</div>
      ) : (
        <div className="memory-editor">
          {error && <div className="errbar">读取或保存失败：{error}</div>}
          <textarea
            value={text}
            onChange={(event) => setText(event.target.value)}
            placeholder="记录架构约定、开发偏好与重要上下文…"
            spellCheck={false}
          />
          <div><button className="btn accent" disabled={saving} onClick={() => void save()}>{saving ? "保存中…" : "保存记忆"}</button></div>
        </div>
      )}
    </section>
  );
}
