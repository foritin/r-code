import { useCallback, useEffect, useMemo, useState, type Dispatch, type SetStateAction } from "react";
import {
  legacyMemoryStatus,
  memoryAddEntry,
  memoryApproveCandidate,
  memoryCancelJob,
  memoryClearAll,
  memoryDeleteEntry,
  memoryEditEntry,
  memoryOverview,
  memoryRejectCandidate,
  memoryRetryJob,
  memoryReviewNow,
  memoryUpdateSettings,
  workspaceSetMemoryMode,
} from "../../lib/ipc";
import type {
  AppConfig,
  LegacyMemoryStatus,
  MemoryCandidateView,
  MemoryEntry,
  MemoryKind,
  MemoryOverview,
  MemoryReviewJobView,
  ReviewerSelection,
  Workspace,
  WorkspaceMemoryMode,
} from "../../lib/types";
import { usePoll } from "../../lib/poll";
import { useTasksStore } from "../../store/tasks";
import { IconCheck, IconRefresh, IconText, IconTrash } from "../icons";

const MEMORY_KINDS: Array<{ value: MemoryKind; label: string }> = [
  { value: "preference", label: "偏好" },
  { value: "constraint", label: "约束" },
  { value: "convention", label: "惯例" },
  { value: "decision", label: "决策" },
  { value: "pitfall", label: "易错点" },
];

const PROJECT_MODES: Array<{ value: WorkspaceMemoryMode; label: string; note: string }> = [
  { value: "inherit", label: "读写", note: "注入已有记忆，并自动复盘本项目的成功对话。" },
  { value: "read_only", label: "只读", note: "注入已有记忆，但不从本项目产生新记忆。" },
  { value: "off", label: "关闭", note: "本项目不注入记忆，也不参与自动复盘。" },
];

function errorText(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause);
}

function formatTime(value: string) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString(undefined, {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function kindLabel(kind: MemoryKind) {
  return MEMORY_KINDS.find((item) => item.value === kind)?.label ?? kind;
}

function configuredReviewer(config: AppConfig | null, current: ReviewerSelection | null): ReviewerSelection | null {
  if (current?.provider_name.trim() && current.model.trim()) return current;
  const providerNames = Object.keys(config?.providers ?? {});
  const candidates = [config?.default_provider, ...providerNames].filter(
    (name, index, names): name is string => Boolean(name) && names.indexOf(name) === index,
  );
  for (const providerName of candidates) {
    const model = config?.providers?.[providerName]?.model?.trim();
    if (model) return { provider_name: providerName, model };
  }
  return null;
}

export function MemoryPanel({ workspace, config }: { workspace: Workspace | null; config: AppConfig | null }) {
  const refreshWorkspaces = useTasksStore((state) => state.refreshWorkspaces);
  const [overview, setOverview] = useState<MemoryOverview | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [reviewError, setReviewError] = useState<string | null>(null);
  const [draftKind, setDraftKind] = useState<MemoryKind>("convention");
  const [draftContent, setDraftContent] = useState("");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editKind, setEditKind] = useState<MemoryKind>("convention");
  const [editContent, setEditContent] = useState("");
  const [candidateEdits, setCandidateEdits] = useState<Record<string, string>>({});
  const [clearArmed, setClearArmed] = useState(false);

  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNotice(null), 3_000);
    return () => window.clearTimeout(timer);
  }, [notice]);

  useEffect(() => {
    if (!reviewError) return;
    const timer = window.setTimeout(() => setReviewError(null), 3_000);
    return () => window.clearTimeout(timer);
  }, [reviewError]);

  const load = useCallback(async (quiet = false) => {
    if (!quiet) setLoading(true);
    try {
      setOverview(await memoryOverview());
      setError(null);
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setLoading(false);
    }
  }, []);

  usePoll(() => load(true), 5_000, true, "记忆状态");

  const runAction = useCallback(async (key: string, action: () => Promise<unknown>, success: string): Promise<boolean> => {
    setBusy(key);
    setNotice(null);
    try {
      await action();
      await load(true);
      if (success) setNotice(success);
      return true;
    } catch (cause) {
      setError(errorText(cause));
      return false;
    } finally {
      setBusy(null);
    }
  }, [load]);

  const entries = useMemo(() => {
    if (!overview) return [];
    if (!workspace) return overview.global_entries;
    return overview.project_entries.filter((entry) =>
      entry.owner.scope === "project" && entry.owner.workspace_id === workspace.id
    );
  }, [overview, workspace]);

  const candidates = useMemo(() => {
    if (!overview) return [];
    if (!workspace) return overview.pending_candidates;
    return overview.pending_candidates.filter((candidate) => candidate.source_workspace_id === workspace.id);
  }, [overview, workspace]);

  const jobs = useMemo(() => {
    if (!overview) return [];
    return overview.recent_jobs
      .filter((job) => workspace ? job.source_workspace_id === workspace.id : job.source_workspace_id == null)
      .slice(0, 8);
  }, [overview, workspace]);

  const reviewLatest = useCallback(async () => {
    setBusy("review-now");
    setNotice(null);
    setReviewError(null);
    try {
      const jobId = await memoryReviewNow({
        workspaceId: workspace?.id ?? null,
        workspacePath: workspace?.canonical_path ?? null,
      });
      await load(true);
      setNotice(jobId
        ? "已加入复盘队列，可在下方查看进度"
        : "最近完成的会话没有新的可复盘内容");
    } catch {
      setReviewError("暂时无法提交复盘，请稍后重试");
    } finally {
      setBusy(null);
    }
  }, [load, workspace]);

  const providerNames = Object.keys(config?.providers ?? {});

  if (loading && !overview) return <div className="knowledge-state" role="status">正在读取记忆…</div>;
  if (!overview) {
    return <div className="knowledge-state error" role="alert"><strong>无法读取记忆</strong><p>{error}</p><button className="rc-button" onClick={() => void load()}>重试</button></div>;
  }

  const settings = overview.settings;
  const activeMode = workspace?.memory_mode ?? "inherit";
  const modeNote = PROJECT_MODES.find((item) => item.value === activeMode)?.note;
  const reviewer = configuredReviewer(config, settings.reviewer);
  const projectEnabled = !workspace || activeMode !== "off";
  const effectivelyEnabled = settings.enabled && projectEnabled;
  const manualReviewEnabled = settings.enabled && (!workspace || activeMode === "inherit");
  const engineTitle = !settings.enabled
    ? "记忆已关闭"
    : workspace && activeMode === "off"
      ? "此项目已关闭"
      : workspace && activeMode === "read_only"
        ? "此项目只读"
        : "记忆已开启";
  const engineDescription = !settings.enabled
    ? "默认关闭，不会读取或复盘任何对话。启用后，只有已生效的记忆会进入新对话。"
    : workspace && activeMode === "off"
      ? "这个项目不会读取记忆，也不会参与自动复盘。"
      : workspace && activeMode === "read_only"
        ? "会读取已有记忆，但不会从这个项目产生新记忆。"
        : "新对话会读取已生效记忆；自动复盘在回答完成后异步运行。";
  const engineActionLabel = busy === "engine"
    ? "处理中…"
    : !settings.enabled
      ? reviewer ? "启用记忆" : "配置模型"
      : workspace
        ? activeMode === "inherit" ? "关闭此项目" : "允许自动复盘"
        : "关闭记忆";

  const updateProjectMode = (next: WorkspaceMemoryMode, success: string) => {
    if (!workspace) return;
    void runAction("engine", async () => {
      await workspaceSetMemoryMode(workspace.id, workspace.memory_generation, next);
      await refreshWorkspaces();
    }, success);
  };

  const handleEngineAction = () => {
    if (!settings.enabled) {
      if (!reviewer) {
        document.getElementById("memory-reviewer-provider")?.focus();
        setNotice("先选择用于自动复盘的模型，再保存并启用");
        return;
      }
      void runAction("engine", () => memoryUpdateSettings({
        expected_version: settings.version,
        enabled: true,
        reviewer,
        trigger_every_turns: settings.trigger_every_turns,
        explicit_remember_immediate: settings.explicit_remember_immediate,
        project_notification_mode: settings.project_notification_mode,
      }), "");
      return;
    }
    if (workspace) {
      updateProjectMode(activeMode === "inherit" ? "off" : "inherit", "");
      return;
    }
    void runAction("engine", () => memoryUpdateSettings({
      expected_version: settings.version,
      enabled: false,
      reviewer: settings.reviewer,
      trigger_every_turns: settings.trigger_every_turns,
      explicit_remember_immediate: settings.explicit_remember_immediate,
      project_notification_mode: settings.project_notification_mode,
    }), "");
  };

  return (
    <div className="knowledge-memory-panel">
      <header className="knowledge-panel-head">
        <div>
          <h2>{workspace ? `${workspace.display_name} 的项目记忆` : "全局记忆"}</h2>
          <p>{workspace ? "只对这个项目生效。" : "跨项目使用；新的全局候选需要你确认。"}</p>
        </div>
        <div className="memory-head-actions">
          <button className="memory-icon-button" title="刷新" aria-label="刷新记忆" disabled={busy != null} onClick={() => void load()}><IconRefresh width={14} height={14} /></button>
        </div>
      </header>

      <section className={`memory-engine-strip${effectivelyEnabled ? " enabled" : ""}${activeMode === "read_only" ? " read-only" : ""}`} aria-label="记忆状态">
        <div className="memory-engine-copy">
          <span className="memory-engine-dot" aria-hidden="true" />
          <div><strong>{engineTitle}</strong><p>{engineDescription}</p></div>
        </div>
        <button type="button" className={`rc-button${effectivelyEnabled ? " rc-button-quiet" : " rc-button-primary"}`} disabled={busy != null} onClick={handleEngineAction}>{engineActionLabel}</button>
      </section>

      {(error || reviewError || notice) && <div className={`memory-banner ${error || reviewError ? "error" : "success"}`} role={error || reviewError ? "alert" : "status"}>
        {!error && !reviewError && <IconCheck width={14} height={14} />}
        <span>{error ?? reviewError ?? notice}</span>
        {error && <button onClick={() => { setError(null); void load(); }}>重试</button>}
      </div>}

      {!workspace && <GlobalMemorySettings
        overview={overview}
        providerNames={providerNames}
        config={config}
        busy={busy}
        onSave={(next) => runAction("settings", () => memoryUpdateSettings(next), "设置已保存")}
      />}

      {workspace && <section className="memory-section memory-project-mode">
        <div className="memory-section-heading"><div><h3>项目模式</h3><p>{modeNote}</p></div></div>
        <div className="memory-mode-picker" role="radiogroup" aria-label="项目记忆模式">
          {PROJECT_MODES.map((item) => <button
            key={item.value}
            type="button"
            role="radio"
            aria-checked={activeMode === item.value}
            className={activeMode === item.value ? "active" : ""}
            disabled={busy != null}
            onClick={() => void runAction("project-mode", async () => {
              await workspaceSetMemoryMode(workspace.id, workspace.memory_generation, item.value);
              await refreshWorkspaces();
            }, `项目记忆已切换为${item.label}`)}
          >{item.label}</button>)}
        </div>
      </section>}

      <section className="memory-section">
        <div className="memory-section-heading">
          <div><h3>记忆 <span>{entries.length}</span></h3><p>新对话会自动读取这里的内容。</p></div>
          <button
            className="rc-button rc-button-quiet"
            disabled={!manualReviewEnabled || busy != null}
            title={!settings.enabled
              ? "请先启用记忆"
              : workspace && activeMode !== "inherit"
                ? "将项目模式切换为读写后才能复盘"
                : "立即复盘当前范围内最近完成的会话"}
            onClick={() => void reviewLatest()}
          >{busy === "review-now" ? "正在提交…" : "立即复盘"}</button>
        </div>

        <div className="memory-entry-list">
          {entries.length === 0 && <div className="memory-empty"><IconText width={18} height={18} /><div><strong>暂无{workspace ? "项目" : "全局"}记忆</strong><p>手动添加一条，或让自动复盘从成功对话中提取。</p></div></div>}
          {entries.map((entry) => <MemoryEntryRow
            key={entry.id}
            entry={entry}
            busy={busy}
            editing={editingId === entry.id}
            editKind={editKind}
            editContent={editContent}
            onEditKind={setEditKind}
            onEditContent={setEditContent}
            onBegin={() => { setEditingId(entry.id); setEditKind(entry.kind); setEditContent(entry.content); }}
            onCancel={() => setEditingId(null)}
            onSave={() => void runAction(`edit:${entry.id}`, () => memoryEditEntry(entry.id, { expected_version: entry.version, kind: editKind, content: editContent, pinned: entry.pinned }), "记忆已更新").then((ok) => { if (ok) setEditingId(null); })}
            onDelete={() => void runAction(`delete:${entry.id}`, () => memoryDeleteEntry(entry.id, entry.version), "记忆已删除")}
          />)}
        </div>

        <div className="memory-add-row">
          <label><span>类型</span><select value={draftKind} aria-label="新记忆类型" onChange={(event) => setDraftKind(event.target.value as MemoryKind)}>{MEMORY_KINDS.map((item) => <option key={item.value} value={item.value}>{item.label}</option>)}</select></label>
          <label><span>内容</span><textarea value={draftContent} maxLength={2_000} rows={2} placeholder="稳定、可复用的事实或偏好" onChange={(event) => setDraftContent(event.target.value)} /></label>
          <button className="rc-button rc-button-primary" disabled={busy != null || draftContent.trim().length < 2} onClick={() => void runAction("add", () => memoryAddEntry({ scope: workspace ? "project" : "global", workspace_id: workspace?.id ?? null, kind: draftKind, content: draftContent, pinned: false }), "记忆已添加").then((ok) => { if (ok) setDraftContent(""); })}>添加</button>
        </div>
      </section>

      {!workspace && <CandidateSection candidates={candidates} edits={candidateEdits} busy={busy} setEdits={setCandidateEdits} runAction={runAction} />}

      <JobSection jobs={jobs} busy={busy} runAction={runAction} />

      {!workspace && <section className="memory-section memory-danger-zone">
        <div><h3>本机数据</h3><p>清空会删除所有正文、候选与复盘队列，并关闭记忆引擎；项目文件与 Git 不受影响。</p></div>
        <button className="rc-button" disabled={busy != null} onBlur={() => setClearArmed(false)} onClick={() => {
          if (!clearArmed) { setClearArmed(true); return; }
          void runAction("clear", memoryClearAll, "记忆数据已清空").then((ok) => { if (ok) setClearArmed(false); });
        }}>{clearArmed ? "再次点击确认清空" : "清空记忆数据"}</button>
      </section>}

      {workspace && <LegacyMemorySafety workspacePath={workspace.canonical_path} />}
    </div>
  );
}

function GlobalMemorySettings({ overview, providerNames, config, busy, onSave }: {
  overview: MemoryOverview;
  providerNames: string[];
  config: AppConfig | null;
  busy: string | null;
  onSave: (next: Parameters<typeof memoryUpdateSettings>[0]) => Promise<boolean>;
}) {
  const current = overview.settings;
  const initialProvider = current.reviewer?.provider_name ?? config?.default_provider ?? providerNames[0] ?? "";
  const [provider, setProvider] = useState(initialProvider);
  const [model, setModel] = useState(current.reviewer?.model ?? config?.providers?.[initialProvider]?.model ?? "");
  const [cadence, setCadence] = useState(current.trigger_every_turns);
  const [explicit, setExplicit] = useState(current.explicit_remember_immediate);

  useEffect(() => {
    const nextProvider = current.reviewer?.provider_name ?? config?.default_provider ?? providerNames[0] ?? "";
    setProvider(nextProvider);
    setModel(current.reviewer?.model ?? config?.providers?.[nextProvider]?.model ?? "");
    setCadence(current.trigger_every_turns);
    setExplicit(current.explicit_remember_immediate);
  }, [current.version, config, providerNames.join("\0")]);

  const save = () => onSave({
    expected_version: current.version,
    enabled: true,
    reviewer: provider && model.trim() ? { provider_name: provider, model: model.trim() } : null,
    trigger_every_turns: cadence,
    explicit_remember_immediate: explicit,
    project_notification_mode: current.project_notification_mode,
  });

  return <form className="memory-section memory-settings" onSubmit={(event) => { event.preventDefault(); void save(); }}>
    <div className="memory-section-heading"><div><h3>自动复盘</h3><p>用一个轻量模型，从成功对话中提取可复用信息。</p></div></div>
    <div className="memory-settings-grid">
      <label><span>模型服务</span><select id="memory-reviewer-provider" aria-label="模型服务" value={provider} onChange={(event) => { const next = event.target.value; setProvider(next); setModel(config?.providers?.[next]?.model ?? ""); }}><option value="">选择模型服务</option>{providerNames.map((name) => <option key={name} value={name}>{name}</option>)}</select></label>
      <label><span>复盘模型</span><input aria-label="复盘模型" value={model} placeholder="模型名称" onChange={(event) => setModel(event.target.value)} /></label>
      <div className="memory-settings-behavior">
        <label className="memory-cadence"><span>每</span><input aria-label="复盘间隔" type="number" min={5} max={50} value={cadence} onChange={(event) => setCadence(Math.max(5, Math.min(50, Number(event.target.value) || 5)))} /><span>轮自动复盘</span></label>
        <label className="memory-check"><input type="checkbox" checked={explicit} onChange={(event) => setExplicit(event.target.checked)} /><span>明确说“请记住”时立即复盘</span></label>
      </div>
    </div>
    <div className="memory-settings-actions"><small>{current.enabled ? "更改会从下一次复盘开始生效。" : "保存后会同时开启记忆。"}</small><button type="submit" className="rc-button rc-button-primary" disabled={busy != null || !provider || !model.trim()}>{current.enabled ? "保存设置" : "保存并启用"}</button></div>
  </form>;
}

function MemoryEntryRow({ entry, busy, editing, editKind, editContent, onEditKind, onEditContent, onBegin, onCancel, onSave, onDelete }: {
  entry: MemoryEntry; busy: string | null; editing: boolean; editKind: MemoryKind; editContent: string;
  onEditKind: (kind: MemoryKind) => void; onEditContent: (value: string) => void; onBegin: () => void; onCancel: () => void; onSave: () => void; onDelete: () => void;
}) {
  return <article className={`memory-entry ${editing ? "editing" : ""}`}>
    {editing ? <>
      <select value={editKind} onChange={(event) => onEditKind(event.target.value as MemoryKind)}>{MEMORY_KINDS.map((item) => <option key={item.value} value={item.value}>{item.label}</option>)}</select>
      <textarea rows={3} maxLength={2_000} value={editContent} onChange={(event) => onEditContent(event.target.value)} />
      <div className="memory-entry-actions"><button className="rc-button rc-button-quiet" onClick={onCancel}>取消</button><button className="rc-button rc-button-primary" disabled={busy != null || editContent.trim().length < 2} onClick={onSave}>保存</button></div>
    </> : <>
      <div className="memory-entry-meta"><span>{kindLabel(entry.kind)}</span><time>{formatTime(entry.updated_at)}</time></div>
      <p>{entry.content}</p>
      <div className="memory-entry-actions"><button className="rc-button rc-button-quiet" disabled={busy != null} onClick={onBegin}>编辑</button><button className="memory-icon-button danger" title="删除" aria-label="删除记忆" disabled={busy != null} onClick={onDelete}><IconTrash width={13} height={13} /></button></div>
    </>}
  </article>;
}

function CandidateSection({ candidates, edits, busy, setEdits, runAction }: {
  candidates: MemoryCandidateView[]; edits: Record<string, string>; busy: string | null;
  setEdits: Dispatch<SetStateAction<Record<string, string>>>;
  runAction: (key: string, action: () => Promise<unknown>, success: string) => Promise<boolean>;
}) {
  if (!candidates.length) return null;
  return <section className="memory-section memory-candidates">
    <div className="memory-section-heading"><div><h3>待审批的全局候选 <span>{candidates.length}</span></h3><p>Reviewer 不能直接写入全局记忆。你可以先修改正文，再决定是否接纳。</p></div></div>
    <div className="memory-candidate-list">{candidates.map((candidate) => {
      const value = edits[candidate.id] ?? candidate.proposed_content;
      return <article className="memory-candidate" key={candidate.id}>
        <div><span>{kindLabel(candidate.kind)}</span><small>置信度 {Math.round(candidate.confidence * 100)}%</small></div>
        <textarea rows={3} maxLength={2_000} value={value} onChange={(event) => setEdits((current) => ({ ...current, [candidate.id]: event.target.value }))} />
        <p>{candidate.reason}</p>
        <div className="memory-entry-actions"><button className="rc-button rc-button-quiet" disabled={busy != null} onClick={() => void runAction(`reject:${candidate.id}`, () => memoryRejectCandidate(candidate.id), "候选已拒绝")}>拒绝</button><button className="rc-button rc-button-primary" disabled={busy != null || value.trim().length < 2} onClick={() => void runAction(`approve:${candidate.id}`, () => memoryApproveCandidate(candidate.id, value), "候选已加入全局记忆")}>批准</button></div>
      </article>;
    })}</div>
  </section>;
}

function JobSection({ jobs, busy, runAction }: {
  jobs: MemoryReviewJobView[]; busy: string | null;
  runAction: (key: string, action: () => Promise<unknown>, success: string) => Promise<boolean>;
}) {
  if (!jobs.length) return null;
  const labels: Record<MemoryReviewJobView["status"], string> = { queued: "排队中", running: "总结中", succeeded: "已完成", failed: "失败", interrupted: "已中断", cancelled: "已取消" };
  return <section className="memory-section memory-jobs">
    <div className="memory-section-heading"><div><h3>最近复盘</h3><p>复盘在主回答完成后异步执行；失败不会影响对话，也不会写入未验证正文。</p></div></div>
    <div className="memory-job-list">{jobs.map((job) => <div className="memory-job" key={job.id}>
      <span className={`memory-job-state ${job.status}`}>{labels[job.status]}</span>
      <div><strong>{job.provider_name} · {job.model}</strong><small>{job.trigger === "cadence" ? "周期触发" : job.trigger === "manual" ? "手动触发" : "明确记住"}{job.status === "succeeded" && job.effect_count != null ? job.effect_count > 0 ? ` · 产生 ${job.effect_count} 项变更` : " · 未发现可复用记忆" : ""} · {formatTime(job.updated_at)}</small></div>
      {job.error_code && <code>{job.error_code}</code>}
      {(job.status === "failed" || job.status === "interrupted") && <button className="rc-button rc-button-quiet" disabled={busy != null} onClick={() => void runAction(`retry:${job.id}`, () => memoryRetryJob(job.id), "复盘已重新排队")}>重试</button>}
      {(job.status === "queued" || job.status === "running") && <button className="rc-button rc-button-quiet" disabled={busy != null} onClick={() => void runAction(`cancel:${job.id}`, () => memoryCancelJob(job.id), "复盘已取消")}>取消</button>}
    </div>)}</div>
  </section>;
}

function LegacyMemorySafety({ workspacePath }: { workspacePath: string }) {
  const [status, setStatus] = useState<LegacyMemoryStatus | null>(null);
  const [error, setError] = useState(false);
  useEffect(() => {
    let active = true;
    setStatus(null); setError(false);
    void legacyMemoryStatus(workspacePath).then((next) => { if (active) setStatus(next); }).catch(() => { if (active) setError(true); });
    return () => { active = false; };
  }, [workspacePath]);
  return <section className="knowledge-memory-safety"><div className="knowledge-memory-safety-head"><span>旧版文件检查</span><small>只读元数据检查，不读取文件正文</small></div>{error ? <div className="legacy-memory-status unknown"><strong>无法检查旧版记忆文件状态</strong><p>R-Code 没有读取或修改项目文件，请稍后重试。</p></div> : status ? <LegacyMemoryNotice status={status} /> : <div className="memory-placeholder">正在检查旧版记忆文件状态…</div>}</section>;
}

function LegacyMemoryNotice({ status }: { status: LegacyMemoryStatus }) {
  if (status.git_tracking === "tracked") return <div className="legacy-memory-status tracked" role="alert"><strong>{status.exists ? "旧版记忆文件可能已进入 Git 历史" : "工作树中未发现旧版记忆文件，但 Git 仍有跟踪记录"}</strong><p>{status.exists ? "检测到该文件当前受 Git 跟踪。" : "该文件当前不在工作树中，但 Git 索引仍记录它，历史也可能保留内容。"} R-Code 不会读取、导入、修改或删除 <code>.r-code/memory.md</code>，也不会自动执行 git rm 或取消跟踪；请自行审查 Git 索引及历史。</p></div>;
  if (status.git_tracking === "unknown") return <div className="legacy-memory-status unknown"><strong>无法检测旧版记忆文件的 Git 跟踪状态</strong><p>{status.exists ? "工作树中发现了旧版记忆文件。" : "工作树中未发现旧版记忆文件，但无法据此判断 Git 历史中是否保留过它。"} R-Code 不会读取、导入、修改或删除 <code>.r-code/memory.md</code>；请在 R-Code 之外自行审查。</p></div>;
  if (!status.exists) return <div className="legacy-memory-status absent"><strong>未发现旧版记忆文件</strong><p>当前工作树中没有 <code>.r-code/memory.md</code>，Git 索引也未跟踪该路径。R-Code 未检查 Git 历史；若过去曾提交，仍需自行审查历史，R-Code 不会自动操作。</p></div>;
  return <div className="legacy-memory-status untracked"><strong>发现未被 Git 跟踪的旧版记忆文件</strong><p>R-Code 不会读取、导入、修改或删除 <code>.r-code/memory.md</code>；请在应用外自行处置。</p></div>;
}
