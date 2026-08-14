import { useCallback, useEffect, useMemo, useState } from "react";
import { errText } from "../../lib/format";
import {
  knowledgePromptsGet,
  knowledgePromptsReset,
  knowledgePromptsSave,
  settingsGet,
  workflowSkillDelete,
  workflowSkillReset,
  workflowSkillSave,
  workflowSkillsList,
  workflowSkillSyncToGlobal,
} from "../../lib/ipc";
import type {
  AppConfig,
  KnowledgePromptSnapshot,
  ProjectPromptMode,
  WorkflowSkill,
  WorkflowSkillDraft,
  WorkflowSkillScope,
} from "../../lib/types";
import { useAppStore, type KnowledgeTab } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { IconCheck, IconProjects, IconShield, IconText } from "../icons";
import { MemoryPanel } from "./MemoryPanel";

type KnowledgeScope = "global" | string;

const TABS: Array<{ id: KnowledgeTab; label: string; description: string }> = [
  { id: "memory", label: "记忆", description: "全局与项目作用域" },
  { id: "prompts", label: "协作 Prompt", description: "主 Agent 与子代理" },
  { id: "skills", label: "Skills", description: "内置、全局与项目能力" },
];

/** Settings-owned knowledge control plane. All editable data stays in AppData. */
export function KnowledgeSettingsPane() {
  const workspaces = useTasksStore((state) => state.workspaces);
  const currentProjectPath = useTasksStore((state) => state.currentProjectId);
  const tab = useAppStore((state) => state.knowledgeTab);
  const openKnowledge = useAppStore((state) => state.openKnowledge);
  const [scope, setScope] = useState<KnowledgeScope>(() => currentProjectPath ?? "global");
  const [config, setConfig] = useState<AppConfig | null>(null);

  const loadConfig = useCallback(async () => {
    try {
      const response = await settingsGet();
      setConfig(response.config);
    } catch {
      // Memory remains independently usable when provider configuration cannot be read.
      setConfig(null);
    }
  }, []);

  useEffect(() => {
    void loadConfig();
  }, [loadConfig]);

  // Project-menu deep links update the shared current project before opening this already-mounted
  // Settings pane. Follow that external navigation once; changing scope inside this pane does not
  // mutate currentProjectId, so the user's manual scope choice remains stable.
  useEffect(() => {
    setScope(currentProjectPath ?? "global");
  }, [currentProjectPath]);

  useEffect(() => {
    if (scope === "global") return;
    if (!workspaces.some((workspace) => workspace.canonical_path === scope)) setScope("global");
  }, [scope, workspaces]);

  const selectedWorkspace = useMemo(
    () => scope === "global" ? null : workspaces.find((workspace) => workspace.canonical_path === scope) ?? null,
    [scope, workspaces],
  );
  const workspacePath = selectedWorkspace?.canonical_path ?? null;

  return (
    <section className="knowledge-settings" aria-label="知识与指令">
      <div className="knowledge-settings-meta">
        <span><IconShield width={14} height={14} />配置与正文仅保存在本机</span>
        <p>全局内容自动提供给每个项目；项目内容只在对应工作区生效。</p>
      </div>
      <div className="knowledge-layout">
        <aside className="knowledge-scope" aria-label="知识作用域">
          <div className="knowledge-scope-group">
            <span>公共</span>
            <button type="button" className={scope === "global" ? "active" : ""} aria-pressed={scope === "global"} onClick={() => setScope("global")}>
              <IconText width={15} height={15} />
              <strong>全局</strong>
            </button>
          </div>
          <div className="knowledge-scope-group project-scopes">
            <span>项目</span>
            {workspaces.length === 0 ? <p>还没有添加项目。</p> : workspaces.map((workspace) => (
              <button
                type="button"
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
              <button
                key={item.id}
                type="button"
                role="tab"
                aria-label={item.label}
                aria-selected={tab === item.id}
                className={tab === item.id ? "active" : ""}
                title={item.description}
                onClick={() => openKnowledge(item.id)}
              >
                <strong>{item.label}</strong>
              </button>
            ))}
          </nav>

          <div className="knowledge-panel" role="tabpanel">
            {tab === "memory" && <MemoryPanel workspace={selectedWorkspace} config={config} />}
            {tab === "prompts" && <AgentPromptsSection key={scope} workspacePath={workspacePath} workspaceName={selectedWorkspace?.display_name ?? null} />}
            {tab === "skills" && <WorkflowSkillsSection key={scope} workspacePath={workspacePath} workspaceName={selectedWorkspace?.display_name ?? null} />}
          </div>
        </div>
      </div>
    </section>
  );
}

function AgentPromptsSection({ workspacePath, workspaceName }: { workspacePath: string | null; workspaceName: string | null }) {
  const [snapshot, setSnapshot] = useState<KnowledgePromptSnapshot | null>(null);
  const [draft, setDraft] = useState({ main_agent: "", subagent: "" });
  const [mode, setMode] = useState<ProjectPromptMode>("append");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const applySnapshot = useCallback((next: KnowledgePromptSnapshot) => {
    setSnapshot(next);
    if (workspacePath) {
      setDraft({
        main_agent: next.project?.main_agent ?? "",
        subagent: next.project?.subagent ?? "",
      });
      setMode(next.project?.mode ?? "append");
    } else {
      setDraft(next.global);
      setMode("append");
    }
  }, [workspacePath]);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      applySnapshot(await knowledgePromptsGet(workspacePath));
    } catch (cause) {
      setError(errText(cause));
    } finally {
      setLoading(false);
    }
  }, [applySnapshot, workspacePath]);

  useEffect(() => {
    void load();
  }, [load]);

  const save = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      applySnapshot(await knowledgePromptsSave(workspacePath, mode, draft.main_agent, draft.subagent));
      setNotice(workspacePath ? `${workspaceName} 的协作 Prompt 已保存并应用。` : "全局协作 Prompt 已保存，所有项目会自动继承。" );
    } catch (cause) {
      setError(errText(cause));
    } finally {
      setBusy(false);
    }
  };

  const reset = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      applySnapshot(await knowledgePromptsReset(workspacePath));
      setNotice(workspacePath ? "已移除项目 Prompt，当前项目恢复继承全局内容。" : "已恢复内置全局 Prompt。" );
    } catch (cause) {
      setError(errText(cause));
    } finally {
      setBusy(false);
    }
  };

  if (loading && !snapshot) return <div className="knowledge-state" role="status">正在读取协作 Prompt…</div>;

  return (
    <section className="knowledge-flat-section knowledge-prompt-settings">
      <header className="knowledge-section-head">
        <div>
          <span>{workspacePath ? "项目作用域" : "公共作用域"}</span>
          <h3>{workspacePath ? `${workspaceName} 的协作 Prompt` : "全局协作 Prompt"}</h3>
          <p>{workspacePath ? "在继承全局内容的基础上，为这个项目增加或替换协作规则。" : "作为公共基线提供给所有新建及已有项目。"}</p>
        </div>
        {workspacePath && <small>{snapshot?.project_configured ? "已配置项目规则" : "当前仅继承全局"}</small>}
      </header>

      {workspacePath && (
        <div className="prompt-merge-choice" role="group" aria-label="项目 Prompt 与全局 Prompt 的关系">
          <button type="button" className={mode === "append" ? "active" : ""} aria-pressed={mode === "append"} disabled={busy} onClick={() => setMode("append")}>
            <strong>追加</strong><span>先应用全局，再应用项目内容</span>
          </button>
          <button type="button" className={mode === "override" ? "active" : ""} aria-pressed={mode === "override"} disabled={busy} onClick={() => setMode("override")}>
            <strong>覆盖</strong><span>仅使用这个项目的内容</span>
          </button>
        </div>
      )}

      {error && <div className="errbar" role="alert">保存协作 Prompt 失败：{error}</div>}
      {notice && <div className="notebar" role="status"><IconCheck width={14} height={14} />{notice}</div>}
      <div className="knowledge-form-grid">
        <div className="field agent-prompt-field">
          <label htmlFor="knowledge-main-agent-prompt">主 Agent</label>
          <textarea id="knowledge-main-agent-prompt" className="input" rows={7} value={draft.main_agent} disabled={busy} placeholder={workspacePath ? "输入仅针对当前项目的主 Agent 规则" : undefined} onChange={(event) => setDraft((current) => ({ ...current, main_agent: event.target.value }))} />
          <span className="hint">说明委派边界、汇总方式与最终责任。</span>
        </div>
        <div className="field agent-prompt-field">
          <label htmlFor="knowledge-subagent-prompt">子代理</label>
          <textarea id="knowledge-subagent-prompt" className="input" rows={7} value={draft.subagent} disabled={busy} placeholder={workspacePath ? "输入仅针对当前项目的子代理规则" : undefined} onChange={(event) => setDraft((current) => ({ ...current, subagent: event.target.value }))} />
          <span className="hint">约束任务范围、输出形式与验证责任。</span>
        </div>
      </div>
      <div className="footbar knowledge-actions">
        <span className="spacer" />
        <button className="btn" type="button" disabled={busy} onClick={() => void reset()}>{workspacePath ? "移除项目规则" : "恢复内置 Prompt"}</button>
        <button className="btn accent" type="button" disabled={busy} onClick={() => void save()}>{busy ? "保存中…" : "保存并应用"}</button>
      </div>
    </section>
  );
}

type SkillView = "builtin" | "global" | "inherited" | "project";

function WorkflowSkillsSection({ workspacePath, workspaceName }: { workspacePath: string | null; workspaceName: string | null }) {
  const [skills, setSkills] = useState<WorkflowSkill[]>([]);
  const [view, setView] = useState<SkillView>(workspacePath ? "inherited" : "builtin");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [draft, setDraft] = useState<WorkflowSkillDraft | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);

  const matchesView = useCallback((skill: WorkflowSkill, target = view) => {
    if (target === "builtin") return skill.scope === "global" && skill.source === "builtin";
    if (target === "global") return skill.scope === "global" && skill.source === "custom";
    if (target === "inherited") return skill.scope === "global";
    return skill.scope === "project";
  }, [view]);

  const select = useCallback((skill: WorkflowSkill) => {
    setSelectedId(skill.id);
    setDraft({
      id: skill.id,
      name: skill.name,
      description: skill.description,
      instructions: skill.instructions,
      source: skill.source,
      enabled: skill.enabled,
      scope: skill.scope,
    });
    setConfirmDelete(false);
  }, []);

  const load = useCallback(async (preferredId?: string, preferredView?: SkillView) => {
    const loaded = await workflowSkillsList(workspacePath);
    setSkills(loaded);
    const targetView = preferredView ?? view;
    const selected = loaded.find((skill) => skill.id === preferredId && matchesView(skill, targetView))
      ?? loaded.find((skill) => matchesView(skill, targetView));
    if (selected) select(selected);
    else {
      setSelectedId(null);
      setDraft(null);
    }
  }, [matchesView, select, view, workspacePath]);

  useEffect(() => {
    setLoading(true);
    void load().catch((cause) => setError(errText(cause))).finally(() => setLoading(false));
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const switchView = (next: SkillView) => {
    setView(next);
    const selected = skills.find((skill) => matchesView(skill, next));
    if (selected) select(selected);
    else {
      setSelectedId(null);
      setDraft(null);
    }
    setNotice(null);
    setError(null);
  };

  const startCustom = () => {
    const scope: WorkflowSkillScope = workspacePath ? "project" : "global";
    const nextView: SkillView = workspacePath ? "project" : "global";
    setView(nextView);
    setSelectedId(null);
    setDraft({ name: "", description: "", instructions: "", source: "custom", enabled: true, scope });
    setConfirmDelete(false);
    setNotice(null);
    setError(null);
  };

  const inherited = Boolean(workspacePath && draft?.scope === "global");

  const save = async () => {
    if (!draft || inherited || busy) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const saved = await workflowSkillSave(draft, workspacePath);
      await load(saved.id);
      setNotice(saved.scope === "project" ? `项目 Skill /${saved.name} 已保存，仅在 ${workspaceName} 中可用。` : `全局 Skill /${saved.name} 已保存，所有项目会自动继承。`);
    } catch (cause) {
      setError(errText(cause));
    } finally {
      setBusy(false);
    }
  };

  const reset = async () => {
    if (!draft?.id || draft.source !== "builtin" || inherited || busy) return;
    setBusy(true);
    setError(null);
    try {
      const restored = await workflowSkillReset(draft.id);
      await load(restored.id);
      setNotice("已恢复随应用发布的默认 Skill。" );
    } catch (cause) {
      setError(errText(cause));
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!draft?.id || draft.source !== "custom" || inherited || busy) return;
    if (!confirmDelete) {
      setConfirmDelete(true);
      window.setTimeout(() => setConfirmDelete(false), 5000);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await workflowSkillDelete(draft.id, draft.scope, workspacePath);
      setSelectedId(null);
      setDraft(null);
      await load();
      setNotice("Skill 已删除。" );
    } catch (cause) {
      setError(errText(cause));
    } finally {
      setBusy(false);
      setConfirmDelete(false);
    }
  };

  const syncToGlobal = async () => {
    if (!workspacePath || !draft?.id || draft.scope !== "project" || busy) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const synced = await workflowSkillSyncToGlobal(draft.id, workspacePath);
      setView("inherited");
      await load(synced.id, "inherited");
      setNotice(`/${synced.name} 已同步到全局；项目副本已移除，当前项目改为自动继承。`);
    } catch (cause) {
      setError(errText(cause));
    } finally {
      setBusy(false);
    }
  };

  const views: Array<{ id: SkillView; label: string }> = workspacePath
    ? [{ id: "inherited", label: "继承自全局" }, { id: "project", label: "项目专属" }]
    : [{ id: "builtin", label: "内置" }, { id: "global", label: "全局自定义" }];
  const visible = skills.filter((skill) => matchesView(skill));

  return (
    <section className="knowledge-flat-section workflow-skills-settings">
      <header className="knowledge-section-head workflow-skills-title">
        <div>
          <span>{workspacePath ? "项目作用域" : "公共作用域"}</span>
          <h3>{workspacePath ? `${workspaceName} 的 Skills` : "全局 Skills"}</h3>
          <p>{workspacePath ? "全局 Skills 已自动继承；项目专属 Skill 可在成熟后同步为全局能力。" : "全局 Skills 在每个项目中可用，调用名在有效作用域内保持唯一。"}</p>
        </div>
        <button className="btn accent" type="button" disabled={busy} onClick={startCustom}>{workspacePath ? "新建项目 Skill" : "新建全局 Skill"}</button>
      </header>
      {error && <div className="errbar" role="alert">{error}</div>}
      {notice && <div className="notebar" role="status"><IconCheck width={14} height={14} />{notice}</div>}
      <div className="workflow-skills-tabs" role="tablist" aria-label="Skill 范围">
        {views.map((item) => (
          <button key={item.id} type="button" role="tab" aria-selected={view === item.id} className={view === item.id ? "on" : ""} onClick={() => switchView(item.id)}>
            {item.label} <span>{skills.filter((skill) => matchesView(skill, item.id)).length}</span>
          </button>
        ))}
      </div>
      {loading ? <div className="knowledge-state" role="status">正在读取 Skills…</div> : (
        <div className="workflow-skills-manager">
          <nav className="workflow-skills-list" aria-label={views.find((item) => item.id === view)?.label ?? "Skills"}>
            {visible.length === 0 && <p>{view === "project" ? "还没有项目专属 Skill。" : "这个分类暂时没有 Skill。"}</p>}
            {visible.map((skill) => (
              <button type="button" key={`${skill.scope}:${skill.id}`} className={selectedId === skill.id ? "selected" : ""} onClick={() => select(skill)}>
                <strong>/{skill.name}</strong>
                <span>{skill.enabled ? "已启用" : "已停用"}{skill.inherited ? " · 自动继承" : ""}{skill.overridden ? " · 已覆盖" : ""}</span>
                <small>{skill.description}</small>
              </button>
            ))}
          </nav>
          <div className="workflow-skill-editor">
            {!draft ? <div className="empty">选择一个 Skill，或新建自定义 Skill。</div> : <>
              {inherited && <div className="workflow-inherited-note"><IconCheck width={14} height={14} /><span>这项能力来自全局。如需修改，请切换到“全局”作用域。</span></div>}
              <div className="field"><label htmlFor="workflow-skill-name">调用名</label><input id="workflow-skill-name" className="input" value={draft.name} disabled={busy || inherited || draft.source === "builtin"} placeholder="例如 release-check" onChange={(event) => setDraft({ ...draft, name: event.target.value })} /><span className="hint">小写字母、数字与单连字符；全局与当前项目不能重名。</span></div>
              <div className="field"><label htmlFor="workflow-skill-description">简介</label><textarea id="workflow-skill-description" className="input" rows={3} value={draft.description} disabled={busy || inherited} onChange={(event) => setDraft({ ...draft, description: event.target.value })} /></div>
              <div className="field agent-prompt-field"><label htmlFor="workflow-skill-instructions">Skill 指令</label><textarea id="workflow-skill-instructions" className="input" rows={10} value={draft.instructions} disabled={busy || inherited} onChange={(event) => setDraft({ ...draft, instructions: event.target.value })} /></div>
              <label className="workflow-skill-enabled"><input type="checkbox" checked={draft.enabled} disabled={busy || inherited} onChange={(event) => setDraft({ ...draft, enabled: event.target.checked })} />在 / 补全中启用</label>
              {!inherited && <div className="footbar workflow-skill-actions">
                {draft.scope === "project" && draft.id && <button className="btn" type="button" disabled={busy} onClick={() => void syncToGlobal()}>同步到全局</button>}
                <span className="spacer" />
                {draft.source === "builtin" ? <button className="btn" type="button" disabled={busy || !draft.id} onClick={() => void reset()}>恢复默认</button> : draft.id ? <button className={`btn danger${confirmDelete ? " confirm" : ""}`} type="button" disabled={busy} onClick={() => void remove()}>{confirmDelete ? "再次点击确认删除" : "删除"}</button> : null}
                <button className="btn accent" type="button" disabled={busy || !draft.name.trim() || !draft.description.trim() || !draft.instructions.trim()} onClick={() => void save()}>{busy ? "保存中…" : "保存 Skill"}</button>
              </div>}
            </>}
          </div>
        </div>
      )}
    </section>
  );
}
