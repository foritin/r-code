import { useCallback, useEffect, useMemo, useState } from "react";
import { legacyMemoryStatus, settingsGet } from "../../lib/ipc";
import type { AppConfig, LegacyMemoryStatus, Workspace } from "../../lib/types";
import { useTasksStore } from "../../store/tasks";
import { IconCheck, IconProjects, IconShield, IconText } from "../icons";
import { AgentPromptsSection, WorkflowSkillsSection } from "./SettingsScene";

type KnowledgeTab = "memory" | "prompts" | "skills";
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
  const [tab, setTab] = useState<KnowledgeTab>("memory");
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
            <p className="page-kicker">KNOWLEDGE &amp; INSTRUCTIONS</p>
            <h1>知识与指令</h1>
            <p>统一管理会被 Agent 使用的记忆、协作 Prompt 与 Skills。数据保存在 R-Code AppData，不进入项目目录或 Git。</p>
          </div>
          <span className="knowledge-local-badge"><IconShield width={14} height={14} />仅本机</span>
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
                <button key={item.id} type="button" role="tab" aria-label={item.label} aria-selected={tab === item.id} className={tab === item.id ? "active" : ""} onClick={() => setTab(item.id)}>
                  <strong>{item.label}</strong>
                  <span>{item.description}</span>
                </button>
              ))}
            </nav>

            <div className="knowledge-panel" role="tabpanel">
              {tab === "memory" && <MemoryPanel workspace={selectedWorkspace} />}
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

function MemoryPanel({ workspace }: { workspace: Workspace | null }) {
  return (
    <div className="knowledge-memory-panel">
      <header className="knowledge-panel-head">
        <div>
          <span>{workspace ? "PROJECT MEMORY" : "GLOBAL MEMORY"}</span>
          <h2>{workspace ? `${workspace.display_name} 的项目记忆` : "全局记忆"}</h2>
          <p>{workspace ? "只会注入这个项目的新运行，绝不会自动提升为全局记忆。" : "跨项目生效；自动复盘产生的内容必须逐条审批后才能生效。"}</p>
        </div>
        <span className="knowledge-engine-state">安全关闭</span>
      </header>

      <div className="knowledge-memory-status">
        <div className="knowledge-memory-empty">
          <IconText width={21} height={21} />
          <div>
            <strong>当前没有生效的{workspace ? "项目" : "全局"}记忆</strong>
            <p>作用域、审批和 Reviewer 数据契约已经冻结；持久化与后台复盘尚未启用，因此当前不会生成、保存或注入记忆正文。</p>
          </div>
        </div>
        <ul aria-label="记忆安全边界">
          <li><IconCheck width={14} height={14} /><span>Reviewer 只是总结器，不会按 Provider 分叉记忆</span></li>
          <li><IconCheck width={14} height={14} /><span>所有正文只允许进入 AppData SQLite</span></li>
          <li><IconCheck width={14} height={14} /><span>{workspace ? "项目提案只留在当前项目作用域" : "全局提案未经审批绝不生效"}</span></li>
        </ul>
      </div>

      {workspace && <LegacyMemorySafety workspacePath={workspace.canonical_path} />}
    </div>
  );
}

function LegacyMemorySafety({ workspacePath }: { workspacePath: string }) {
  const [status, setStatus] = useState<LegacyMemoryStatus | null>(null);
  const [error, setError] = useState(false);

  useEffect(() => {
    let active = true;
    setStatus(null);
    setError(false);
    void legacyMemoryStatus(workspacePath).then((next) => {
      if (active) setStatus(next);
    }).catch(() => {
      if (active) setError(true);
    });
    return () => { active = false; };
  }, [workspacePath]);

  return (
    <section className="knowledge-memory-safety">
      <div className="knowledge-memory-safety-head"><span>旧版文件检查</span><small>只读元数据检查，不读取文件正文</small></div>
      {error
        ? <div className="legacy-memory-status unknown"><strong>无法检查旧版记忆文件状态</strong><p>R-Code 没有读取或修改项目文件，请稍后重试。</p></div>
        : status
          ? <LegacyMemoryNotice status={status} />
          : <div className="memory-placeholder">正在检查旧版记忆文件状态…</div>}
    </section>
  );
}

function LegacyMemoryNotice({ status }: { status: LegacyMemoryStatus }) {
  if (status.git_tracking === "tracked") {
    return <div className="legacy-memory-status tracked" role="alert"><strong>{status.exists ? "旧版记忆文件可能已进入 Git 历史" : "工作树中未发现旧版记忆文件，但 Git 仍有跟踪记录"}</strong><p>{status.exists ? "检测到该文件当前受 Git 跟踪。" : "该文件当前不在工作树中，但 Git 索引仍记录它，历史也可能保留内容。"} R-Code 不会读取、导入、修改或删除 <code>.r-code/memory.md</code>，也不会自动执行 git rm 或取消跟踪；请自行审查 Git 索引及历史。</p></div>;
  }
  if (status.git_tracking === "unknown") {
    return <div className="legacy-memory-status unknown"><strong>无法检测旧版记忆文件的 Git 跟踪状态</strong><p>{status.exists ? "工作树中发现了旧版记忆文件。" : "工作树中未发现旧版记忆文件，但无法据此判断 Git 历史中是否保留过它。"} R-Code 不会读取、导入、修改或删除 <code>.r-code/memory.md</code>；请在 R-Code 之外自行审查。</p></div>;
  }
  if (!status.exists) {
    return <div className="legacy-memory-status absent"><strong>未发现旧版记忆文件</strong><p>当前工作树中没有 <code>.r-code/memory.md</code>，Git 索引也未跟踪该路径。R-Code 未检查 Git 历史；若过去曾提交，仍需自行审查历史，R-Code 不会自动操作。</p></div>;
  }
  return <div className="legacy-memory-status untracked"><strong>发现未被 Git 跟踪的旧版记忆文件</strong><p>R-Code 不会读取、导入、修改或删除 <code>.r-code/memory.md</code>；请在 R-Code 之外自行审查与处置。</p></div>;
}
