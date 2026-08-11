import { useCallback, useEffect, useMemo, useState } from "react";
import { settingsGet } from "../../lib/ipc";
import type { AppConfig } from "../../lib/types";
import { useAppStore, type KnowledgeTab } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { IconProjects, IconShield, IconText } from "../icons";
import { AgentPromptsSection, WorkflowSkillsSection } from "./SettingsScene";
import { MemoryPanel } from "./MemoryPanel";

type KnowledgeScope = "global" | string;

const TABS: Array<{ id: KnowledgeTab; label: string; description: string }> = [
  { id: "memory", label: "记忆", description: "全局与项目作用域" },
  { id: "prompts", label: "协作 Prompt", description: "主 Agent 与子代理" },
  { id: "skills", label: "Skills", description: "内置与自定义能力" },
];

/** 用户级知识控制面：真实配置都保存在 AppData，不把任何正文写进项目或 Git。 */
export function KnowledgeScene() {
  const workspaces = useTasksStore((state) => state.workspaces);
  const currentProjectPath = useTasksStore((state) => state.currentProjectId);
  const tab = useAppStore((state) => state.knowledgeTab);
  const openKnowledge = useAppStore((state) => state.openKnowledge);
  const [scope, setScope] = useState<KnowledgeScope>(() => currentProjectPath ?? "global");
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [configError, setConfigError] = useState<string | null>(null);

  const loadConfig = useCallback(async () => {
    try {
      const response = await settingsGet();
      setConfig(response.config);
      setConfigError(null);
    } catch (cause) {
      setConfigError(String(cause));
    }
  }, []);

  useEffect(() => {
    void loadConfig();
  }, [loadConfig]);

  useEffect(() => {
    if (scope === "global") return;
    if (!workspaces.some((workspace) => workspace.canonical_path === scope)) setScope("global");
  }, [scope, workspaces]);

  const selectedWorkspace = useMemo(
    () => scope === "global" ? null : workspaces.find((workspace) => workspace.canonical_path === scope) ?? null,
    [scope, workspaces],
  );

  return (
    <div className="scene scene-knowledge">
      <section className="knowledge-center" aria-label="知识与指令">
        <header className="knowledge-header">
          <div>
            <h1>知识与指令</h1>
            <p>管理 Agent 的记忆、协作方式和扩展能力。</p>
          </div>
          <span className="knowledge-local-badge"><IconShield width={14} height={14} />数据仅存本机</span>
        </header>

        <div className="knowledge-layout">
          <aside className="knowledge-scope" aria-label="知识作用域">
            <div className="knowledge-scope-group">
              <span>用户</span>
              <button className={scope === "global" ? "active" : ""} aria-pressed={scope === "global"} aria-label="全局" onClick={() => setScope("global")}>
                <IconText width={15} height={15} />
                <strong>全局</strong>
              </button>
            </div>
            <div className="knowledge-scope-group project-scopes">
              <span>项目</span>
              {workspaces.length === 0 ? <p>还没有添加项目。</p> : workspaces.map((workspace) => (
                <button
                  key={workspace.id}
                  className={scope === workspace.canonical_path ? "active" : ""}
                  aria-pressed={scope === workspace.canonical_path}
                  aria-label={workspace.display_name}
                  title={workspace.display_name}
                  onClick={() => setScope(workspace.canonical_path)}
                >
                  <IconProjects width={15} height={15} />
                  <strong>{workspace.display_name}</strong>
                </button>
              ))}
            </div>
          </aside>

          <div className="knowledge-content">
            <nav className="knowledge-tabs" role="tablist" aria-label="知识类型">
              {TABS.map((item) => (
                <button key={item.id} type="button" role="tab" aria-label={item.label} aria-selected={tab === item.id} className={tab === item.id ? "active" : ""} title={item.description} onClick={() => openKnowledge(item.id)}>
                  <strong>{item.label}</strong>
                </button>
              ))}
            </nav>

            <div className="knowledge-panel" role="tabpanel">
              {tab === "memory" && <MemoryPanel workspace={selectedWorkspace} config={config} />}
              {tab === "prompts" && (
                config
                  ? <AgentPromptsSection config={config} reload={loadConfig} />
                  : <KnowledgeLoading error={configError} onRetry={loadConfig} label="协作 Prompt" />
              )}
              {tab === "skills" && <WorkflowSkillsSection />}
            </div>
          </div>
        </div>
      </section>
    </div>
  );
}

function KnowledgeLoading({ error, label, onRetry }: { error: string | null; label: string; onRetry: () => Promise<void> }) {
  if (error) {
    return <div className="knowledge-state error" role="alert"><strong>无法读取{label}</strong><p>{error}</p><button className="rc-button" onClick={() => void onRetry()}>重试</button></div>;
  }
  return <div className="knowledge-state" role="status">正在读取{label}…</div>;
}
