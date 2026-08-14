import { useState } from "react";
import { displayPath } from "../../lib/format";
import { workspaceChoose } from "../../lib/ipc";
import { useAppStore } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { IconAttach, IconCheck, IconProjects } from "../icons";
import { projectAccessModeLabel } from "../ProjectAccessSelector";

/** 添加项目入口。项目打开、文件、知识与移除等后续动作统一回到左侧项目树。 */
export function ProjectsScene() {
  const workspaces = useTasksStore((state) => state.workspaces);
  const currentWorkspacePath = useTasksStore((state) => state.currentProjectId);
  const refreshWorkspaces = useTasksStore((state) => state.refreshWorkspaces);
  const openDashboard = useAppStore((state) => state.openDashboard);
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
      openDashboard(workspace.canonical_path);
    } catch (cause) {
      setError(`打开工作区失败：${String(cause)}`);
    } finally {
      setOpening(false);
    }
  };

  return (
    <div className="scene scene-projects">
      <div className="scene-scroll projects-scroll">
        <header className="workspace-hero">
          <div>
            <div className="section-label">PROJECTS</div>
            <h1>添加或打开本地项目。</h1>
            <p>点击已有项目会直接进入项目工作台；移除、项目文件和知识设置都在左侧项目行的菜单中。</p>
          </div>
          <button className="btn accent workspace-choose" disabled={opening} onClick={() => void chooseWorkspace()}>
            <IconAttach width={14} height={14} />
            {opening ? "正在打开…" : "选择文件夹"}
          </button>
        </header>

        {error && <div className="errbar" role="alert">{error}</div>}

        <section className="workspace-library" aria-label="最近工作区">
          <div className="workspace-section-head"><span>最近使用</span><span>{workspaces.length} 个文件夹</span></div>
          {workspaces.length === 0 ? (
            <div className="workspace-empty">
              <IconProjects width={20} height={20} />
              <p>尚未添加项目。</p>
              <button className="quiet-link" onClick={() => void chooseWorkspace()}>从系统选择器开始</button>
            </div>
          ) : (
            <div className="workspace-list">
              {workspaces.map((workspace) => {
                const current = workspace.canonical_path === currentWorkspacePath;
                return (
                  <article className={`workspace-row${current ? " current" : ""}`} key={workspace.id}>
                    <button className="workspace-main" onClick={() => openDashboard(workspace.canonical_path)} title={`打开 ${workspace.display_name} 项目`}>
                      <span className="workspace-glyph"><IconProjects width={16} height={16} /></span>
                      <span className="workspace-copy"><strong>{workspace.display_name}</strong><small title={displayPath(workspace.canonical_path)}>{displayPath(workspace.canonical_path)}</small></span>
                      {current && <span className="current-mark"><IconCheck width={13} height={13} /> 当前项目</span>}
                    </button>
                    <div className="workspace-actions"><span className="workspace-access scoped">{projectAccessModeLabel(workspace.access_mode)}</span></div>
                  </article>
                );
              })}
            </div>
          )}
        </section>
      </div>
    </div>
  );
}
