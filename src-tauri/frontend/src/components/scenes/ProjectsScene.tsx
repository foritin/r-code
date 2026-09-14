import { useState } from "react";
import { displayPath } from "../../lib/format";
import { workspaceChoose } from "../../lib/ipc";
import { useAppStore } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { IconAttach, IconCheck, IconProjects, IconShield } from "../icons";
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
      <div className="opt-page opt-projects-page">
        <header className="opt-page-head">
          <div>
            <span className="opt-eyebrow">PROJECTS</span>
            <h1>项目</h1>
            <p>点击已有项目会直接进入项目工作台；移除、项目文件和知识设置都在左侧项目行的菜单中。</p>
          </div>
          <button className="opt-button primary" disabled={opening} onClick={() => void chooseWorkspace()}>
            <IconAttach width={14} height={14} />
            {opening ? "正在打开…" : "选择文件夹"}
          </button>
        </header>

        {error && <div className="errbar" role="alert">{error}</div>}

        <section className="opt-projects-list" aria-label="最近工作区">
          <div className="opt-list-caption">
            <span>最近使用<b>{String(workspaces.length).padStart(2, "0")}</b></span>
            <span>会话</span>
            <span>访问策略</span>
            <span />
          </div>
          {workspaces.length === 0 ? (
            <div className="opt-empty">
              <span className="opt-icon"><IconProjects width={20} height={20} /></span>
              <h3>尚未添加项目</h3>
              <p>从系统选择器添加第一个本地项目。</p>
              <button className="opt-button" onClick={() => void chooseWorkspace()}>从系统选择器开始</button>
            </div>
          ) : (
            workspaces.map((workspace) => {
              const current = workspace.canonical_path === currentWorkspacePath;
              return (
                <article className={`opt-project-line${current ? " selected" : ""}`} key={workspace.id}>
                  <span className="opt-icon"><IconProjects width={20} height={20} /></span>
                  <button className="opt-project-title" onClick={() => openDashboard(workspace.canonical_path)} title={`打开 ${workspace.display_name} 项目`}>
                    <strong>{workspace.display_name}</strong>
                    {current && <span className="opt-current-label"><IconCheck width={13} height={13} /> 当前项目</span>}
                    <small title={displayPath(workspace.canonical_path)}>{displayPath(workspace.canonical_path)}</small>
                  </button>
                  <span aria-hidden="true" />
                  <span className="opt-project-policy"><IconShield width={13} height={13} />{projectAccessModeLabel(workspace.access_mode)}</span>
                  <button className="opt-button" onClick={() => openDashboard(workspace.canonical_path)}>打开</button>
                </article>
              );
            })
          )}
        </section>

        <footer className="opt-projects-footer">
          <span>项目文件与知识设置，在每行的菜单中管理。</span>
          <span>移除项目会保留磁盘文件。</span>
        </footer>
      </div>
    </div>
  );
}
