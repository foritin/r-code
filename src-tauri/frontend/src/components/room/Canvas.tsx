/**
 * Room 右列画布 —— Summary / Changes·n / Terminal / Review 四页签。
 * 激活页签来自 store.app.canvasTab（页签点击或 视图 菜单切换）。
 * Changes 的文件列表直接用 detail.changes(随 RoomScene 2s 轮询自动刷新,与 changesList 同源)。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  acceptTask,
  changeDiff,
  fileList,
  fileRead,
  fileWrite,
  rollbackFile,
  rollbackTask,
  runVerification,
  terminalCreate,
  terminalKill,
  terminalList,
  terminalRead,
  terminalResize,
  terminalSend,
  terminalSnapshot,
  verificationList,
  verificationOutput,
  type FileContent,
  type FileTreeEntry,
} from "../../lib/ipc";
import type {
  ChangeDiff,
  FileChange,
  ProjectAccessMode,
  TaskDetail,
  TerminalInfo,
  VerificationRecord,
} from "../../lib/types";
import { useAppStore, type CanvasTab } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { usePoll } from "../../lib/poll";
import { isTypingTarget } from "../../lib/keys";
import { clockTime, displayPath, elapsedMinutes, modeLabel } from "../../lib/format";
import { stripAnsi } from "./model";
import type { ActivityTraceState } from "./activity";
import { IconCheck, IconEditor, IconFile, IconPlus, IconProjects, IconTerminal } from "../icons";
import { projectAccessModeLabel } from "../ProjectAccessSelector";

interface Props {
  taskId: string;
  running: boolean;
  activity: ActivityTraceState;
  workspacePath: string | null;
  workspaceAttached: boolean;
}

const TABS: { id: CanvasTab; label: string }[] = [
  { id: "summary", label: "Summary" },
  { id: "changes", label: "Changes" },
  { id: "files", label: "Files" },
  { id: "terminal", label: "Terminal" },
  { id: "review", label: "Review" },
];

export function Canvas({ taskId, running, activity, workspacePath, workspaceAttached }: Props) {
  const tab = useAppStore((s) => s.canvasTab);
  const setTab = useAppStore((s) => s.setCanvasTab);
  const detail = useTasksStore((s) => s.details[taskId]);
  const workspace = useTasksStore((s) =>
    workspacePath ? s.workspaces.find((item) => item.canonical_path === workspacePath) : undefined,
  );
  const workingSubagents = activity.subagents.filter(
    (item) => item.status === "queued" || item.status === "running" || item.status === "waiting_permission"
  ).length;

  return (
    <div className="canvas pane pane-lit">
      <div className="canvas-tabs">
        {TABS.map((t) => (
          <button
            key={t.id}
            className={"ctab" + (tab === t.id ? " on" : "")}
            onClick={() => setTab(t.id)}
          >
            {t.label}
            {t.id === "changes" && (
              <span className="n">{detail?.changes.length ?? 0}</span>
            )}
          </button>
        ))}
        {running && (
          <span className="canvas-live" title={activity.label}>
            <i /> live · {activity.label}{workingSubagents > 0 ? ` · 子代理 ${workingSubagents}` : ""}
          </span>
        )}
      </div>
      <div className="canvas-body">
        {tab === "summary" && (
          <SummaryPanel
            detail={detail}
            running={running}
            activity={activity}
            workspacePath={workspacePath}
            workspaceName={workspace?.display_name ?? null}
            workspaceAttached={workspaceAttached}
            workspaceAccessMode={workspace?.access_mode ?? null}
          />
        )}
        {tab === "changes" && <ChangesPanel taskId={taskId} running={running} detail={detail} />}
        {tab === "files" && <FilesPanel workspacePath={workspacePath} workspaceAttached={workspaceAttached} running={running} />}
        {tab === "terminal" && <TerminalPanel workspacePath={workspacePath} workspaceAttached={workspaceAttached} />}
        {tab === "review" && <ReviewPanel taskId={taskId} />}
      </div>
    </div>
  );
}

// ---------- Summary ----------

const STATE_LABEL: Record<string, string> = {
  idle: "空闲",
  exploring: "探索中",
  in_progress: "运行中",
  interrupted: "已中止",
  review_ready: "待审阅",
  archived: "已归档",
};

const EVENT_LABEL: Record<string, string> = {
  task_created: "任务创建",
  state_changed: "状态变更",
  tool_call: "工具调用",
  tool_result: "工具结果",
  permission_requested: "权限请求",
  permission_decided: "权限批复",
  file_changed: "文件变更",
  verification_run: "运行验证",
  user_steered: "已引导运行",
  user_message_queued: "消息已排队",
  queue_dispatched: "已发送排队消息",
  run_aborted: "运行已中止",
  session_branched: "已创建编辑分支",
  subagent_started: "子代理已启动",
  subagent_finished: "子代理已结束",
  system: "系统事件",
};

function SummaryPanel({
  detail,
  running,
  activity,
  workspacePath,
  workspaceName,
  workspaceAttached,
  workspaceAccessMode,
}: {
  detail: TaskDetail | undefined;
  running: boolean;
  activity: ActivityTraceState;
  workspacePath: string | null;
  workspaceName: string | null;
  workspaceAttached: boolean;
  workspaceAccessMode: ProjectAccessMode | null;
}) {
  const setTab = useAppStore((s) => s.setCanvasTab);
  if (!detail) return <div className="empty">加载中…</div>;
  const { task, runs, events, changes, permissions, verifications, queued_messages: queuedMessages } = detail;
  const pending = permissions.filter((p) => p.decision === "pending").length;
  const passed = verifications.filter((v) => v.status === "passed").length;
  const queued = queuedMessages.filter((message) => message.state === "queued" || message.state === "dispatching").length;
  const subagents = runs.filter((run) => run.agent_kind === "subagent");
  const activeSubagents = subagents.filter((run) => run.ended_at == null).length;
  const activeMainRun = runs.find((run) => run.agent_kind === "main" && run.ended_at == null);
  const completedSubagents = subagents
    .filter((run) => run.ended_at != null && run.summary)
    .sort((a, b) => (b.ended_at ?? "").localeCompare(a.ended_at ?? ""))
    .slice(0, 3);
  const visibleSubagents = activity.subagents
    .filter((item) => item.status === "queued" || item.status === "running" || item.status === "waiting_permission")
    .slice(0, 3);
  const title = task.title.trim() || task.goal.trim() || "未命名会话";
  const hasDistinctGoal = Boolean(task.goal.trim() && task.goal.trim() !== title);
  const workspaceLabel = workspaceName ?? (workspacePath ? "已附加文件夹" : "纯聊天");
  const modelLabel = activeMainRun?.model || task.provider_name || "默认模型服务";

  // run 边界属于审计数据；运行视图由 AgentRun 承担，摘要只保留其他可行动事件。
  const recentEvents = [...events]
    .filter((event) => event.event_type !== "run_started" && event.event_type !== "run_ended")
    .sort((a, b) => b.id - a.id)
    .slice(0, running ? 3 : 5);
  const activityRows = running
    ? activity.recentActivities.slice(0, 2).map((item) => ({
        id: `activity-${item.id}`,
        label: item.label,
        at: new Date(item.at).toISOString(),
        kind: item.kind,
      }))
    : [];
  const eventLabel = (event: (typeof recentEvents)[number]) => {
    switch (event.event_type) {
      case "file_changed":
        return changes[0] ? `文件变更 · ${changes[0].path}` : EVENT_LABEL[event.event_type];
      case "permission_requested":
        return pending > 0 ? `等待权限 · ${permissions.find((p) => p.decision === "pending")?.tool_name ?? ""}` : EVENT_LABEL[event.event_type];
      case "verification_run":
        return verifications[0] ? `验证 · ${verifications[0].command}` : EVENT_LABEL[event.event_type];
      case "tool_call":
      case "tool_result":
        return running ? activity.label : EVENT_LABEL[event.event_type];
      default:
        return EVENT_LABEL[event.event_type] ?? event.event_type;
    }
  };
  return (
    <div className="sum-wrap">
      <div className="sum-head">
        <span className={"st-chip " + (task.state === "review_ready" ? "warn" : runningCls(task.state))}>
          {STATE_LABEL[task.state] ?? task.state}
        </span>
        <span className="sum-title" title={title}>{title}</span>
      </div>
      {hasDistinctGoal && <div className="sum-goal" title={task.goal}>{task.goal}</div>}
      <div className="sum-scope">
        <span title={workspacePath ? displayPath(workspacePath) : "未附加文件夹"}>
          {workspaceLabel}
        </span>
        <span title={modelLabel}>模型 · {modelLabel}</span>
        <span className={workspaceAttached ? "scoped" : ""}>
          {workspaceAttached && workspaceAccessMode
            ? projectAccessModeLabel(workspaceAccessMode)
            : "仅聊天"}
        </span>
      </div>
      {running && (
        <div className="sum-live">
          <div className="sum-live-head">
            <span><i /> 当前运行</span>
            <strong>{activity.label}</strong>
          </div>
          {visibleSubagents.length > 0 && (
            <div className="sum-live-subagents">
              {visibleSubagents.map((subagent) => (
                <span key={subagent.id} title={subagent.detail ?? subagent.label}>
                  {subagent.label} · {subagent.phase}
                </span>
              ))}
            </div>
          )}
        </div>
      )}
      <div className="sum-grid">
        <div className="sum-cell">
          <div className="k">模式</div>
          <div className="v">{modeLabel(task.mode)}</div>
        </div>
        <div className="sum-cell">
          <div className="k">已进行</div>
          <div className="v">{elapsedMinutes(task.created_at)}</div>
        </div>
        <div className="sum-cell">
          <div className="k">队列</div>
          <div className={"v" + (queued > 0 ? " warn" : "")}>{queued}</div>
        </div>
        <div className="sum-cell">
          <div className="k">子代理</div>
          <div className={"v" + (activeSubagents > 0 ? " warn" : "")}>
            {activeSubagents > 0 ? `${activeSubagents} 运行中 / ` : ""}{subagents.length}
          </div>
        </div>
        <button className="sum-cell action" onClick={() => setTab("changes")}>
          <div className="k">变更文件</div>
          <div className="v">{changes.length}</div>
        </button>
        <button className="sum-cell action" onClick={() => setTab("review")}>
          <div className="k">验证</div>
          <div className="v">
            {passed}/{verifications.length} 通过
          </div>
        </button>
        <button className="sum-cell action" onClick={() => setTab("review")}>
          <div className="k">待批权限</div>
          <div className={"v" + (pending > 0 ? " warn" : "")}>{pending}</div>
        </button>
      </div>
      {completedSubagents.length > 0 && (
        <>
          <div className="zone-head">子代理结果</div>
          {completedSubagents.map((run) => (
            <div className="sum-ev subagent-summary" key={run.id}>
              <span className="dot" />
              <span className="t">{run.agent_label || "只读调查"}</span>
              <span className="subagent-summary-text" title={run.summary ?? undefined}>
                {run.summary}
              </span>
            </div>
          ))}
        </>
      )}
      <div className="zone-head">最近事件</div>
      {activityRows.length === 0 && recentEvents.length === 0 && (
        <div className="sum-empty">会话尚未产生可展示的运行事件。</div>
      )}
      {activityRows.map((item) => (
        <div className={"sum-ev live-" + item.kind} key={item.id}>
          <span className="dot" />
          <span className="t">{item.label}</span>
          <span className="at">{clockTime(item.at)}</span>
        </div>
      ))}
      {recentEvents.map((event) => (
        <div className="sum-ev" key={event.id}>
          <span className="dot" />
          <span className="t">{eventLabel(event)}</span>
          <span className="at">{clockTime(event.created_at)}</span>
        </div>
      ))}
    </div>
  );
}

function runningCls(state: string): string {
  if (state === "interrupted") return "bad";
  return state === "in_progress" || state === "exploring" ? "run" : "";
}

// ---------- Changes ----------

function ChangesPanel({
  taskId,
  running,
  detail,
}: {
  taskId: string;
  running: boolean;
  detail: TaskDetail | undefined;
}) {
  const changes = useMemo(() => detail?.changes ?? [], [detail]);
  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const [sel, setSel] = useState<string | null>(null);
  const [diff, setDiff] = useState<ChangeDiff | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [confirmPath, setConfirmPath] = useState<string | null>(null);
  const confirmTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const path = sel ?? changes[0]?.path ?? null;

  // detail 每 2s 刷新 → diff 跟随(运行中即 live following)
  useEffect(() => {
    if (!path) {
      setDiff(null);
      return;
    }
    let dead = false;
    changeDiff(taskId, path)
      .then((d) => {
        if (!dead) {
          setDiff(d);
          setError(null);
        }
      })
      .catch((e) => {
        if (!dead) setError(String(e));
      });
    return () => {
      dead = true;
    };
  }, [taskId, path, changes]);

  const doRollback = async (p: string) => {
    if (confirmPath !== p) {
      setConfirmPath(p);
      if (confirmTimer.current) clearTimeout(confirmTimer.current);
      confirmTimer.current = setTimeout(() => setConfirmPath(null), 3000);
      return;
    }
    setConfirmPath(null);
    setError(null);
    setNotice(null);
    try {
      const result = await rollbackFile(taskId, p);
      setNotice(`已回滚 ${p}(${result})`);
      await refreshDetail(taskId);
    } catch (e) {
      setError(String(e));
    }
  };

  const lines = diff?.supported ? (diff.lines ?? []) : [];
  const adds = lines.filter((l) => l.kind === "add").length;
  const dels = lines.filter((l) => l.kind === "del").length;

  // F7/⇧F7 变更点导航（accessible diff：在 add/del 行间循环跳转）
  const changePoints = useMemo(
    () =>
      lines
        .map((l, i) => (l.kind === "add" || l.kind === "del" ? i : -1))
        .filter((i) => i >= 0),
    [lines]
  );
  const [f7Idx, setF7Idx] = useState(-1);
  const diffBodyRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (changePoints.length === 0) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "F7" || isTypingTarget(e.target)) return;
      e.preventDefault();
      setF7Idx((cur) =>
        e.shiftKey
          ? cur <= 0
            ? changePoints.length - 1
            : cur - 1
          : cur >= changePoints.length - 1
            ? 0
            : cur + 1
      );
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [changePoints.length]);

  useEffect(() => {
    if (f7Idx < 0 || f7Idx >= changePoints.length) return;
    const els = diffBodyRef.current?.querySelectorAll(".dl");
    els?.[changePoints[f7Idx]]?.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [f7Idx, changePoints]);

  // 最新一段连续 add 行 → .fresh shimmer(仅运行中)
  const freshFrom = useMemo(() => {
    if (!running || lines.length === 0) return -1;
    let end = -1;
    for (let i = lines.length - 1; i >= 0; i--) {
      if (lines[i].kind === "add") {
        end = i;
        break;
      }
    }
    if (end < 0) return -1;
    let start = end;
    while (start - 1 >= 0 && lines[start - 1].kind === "add") start--;
    return start;
  }, [lines, running]);
  const freshTo = useMemo(() => {
    if (freshFrom < 0) return -1;
    let end = freshFrom;
    while (end + 1 < lines.length && lines[end + 1].kind === "add") end++;
    return end;
  }, [freshFrom, lines]);

  return (
    <div className="changes-wrap">
      <div className="changes-list">
        {changes.length === 0 && <div className="empty">还没有文件变更。</div>}
        {changes.map((c) => (
          <div
            key={c.id}
            className={"chg-row" + (path === c.path ? " sel" : "")}
            onClick={() => setSel(c.path)}
          >
            <span className={"chg-type t-" + c.change_type}>{typeLabel(c)}</span>
            <span className="chg-path" title={c.path}>
              {c.path}
            </span>
            <button
              className={"chg-rb" + (confirmPath === c.path ? " confirm" : "")}
              title={confirmPath === c.path ? "再次点击确认回滚" : "回滚此文件"}
              onClick={(e) => {
                e.stopPropagation();
                void doRollback(c.path);
              }}
            >
              {confirmPath === c.path ? "确认?" : "回滚"}
            </button>
          </div>
        ))}
      </div>
      <div className="changes-view">
        {error && <div className="panel-error">{error}</div>}
        {notice && <div className="panel-note">{notice}</div>}
        {!path && <div className="empty">没有可查看的变更。</div>}
        {path && diff && !diff.supported && (
          <div className="chg-meta">
            <div className="canvas-head">
              <span className="path">{diff.path}</span>
            </div>
            <div className="empty">
              此文件不支持行级 diff(blob 缺失或二进制)。
              <br />
              类型:{diff.change_type ?? "—"} · before {shortHash(diff.before_hash)} → after{" "}
              {shortHash(diff.after_hash)}
            </div>
            <div className="chg-meta-actions">
              <button
                className={"btn danger sm" + (confirmPath === path ? " confirm" : "")}
                onClick={() => void doRollback(path)}
              >
                {confirmPath === path ? "确认回滚?" : "回滚此文件"}
              </button>
            </div>
          </div>
        )}
        {path && diff?.supported && (
          <>
            <div className="canvas-head">
              <span className="path">{diff.path}</span>
              <span className="stat diffstat">
                <span className="add">+{adds}</span> <span className="del">−{dels}</span>
              </span>
              {changePoints.length > 0 && (
                <span className="kact" title="F7 下一个变更，Shift + F7 上一个">
                  F7 导航 {f7Idx >= 0 ? `${f7Idx + 1}/${changePoints.length}` : changePoints.length}
                </span>
              )}
              {running && (
                <span className="following">
                  <i />
                  live following
                </span>
              )}
            </div>
            <div className="diff-body" ref={diffBodyRef}>
              {diff.truncated && <div className="dl hunk">diff 过大,已截断(全删全增)</div>}
              {lines.map((l, i) =>
                l.kind === "hunk" ? (
                  <div className="dl hunk" key={i}>
                    {l.text}
                  </div>
                ) : (
                  <div
                    className={
                      "dl " +
                      l.kind +
                      (i >= freshFrom && i <= freshTo && freshFrom >= 0 ? " fresh" : "") +
                      (f7Idx >= 0 && changePoints[f7Idx] === i ? " f7-cur" : "")
                    }
                    key={i}
                  >
                    <span className="no">{l.new_no ?? l.old_no ?? ""}</span>
                    <span className="code">{l.text}</span>
                  </div>
                )
              )}
              {lines.length === 0 && <div className="empty">无 diff 内容。</div>}
            </div>
          </>
        )}
        {path && !diff && !error && <div className="empty">加载 diff…</div>}
      </div>
    </div>
  );
}

function typeLabel(c: FileChange): string {
  switch (c.change_type) {
    case "create":
      return "new";
    case "modify":
      return "mod";
    case "delete":
      return "del";
    case "rename":
      return "ren";
  }
}

function shortHash(h: string | null | undefined): string {
  return h ? h.slice(0, 8) : "—";
}

// ---------- Files ----------

interface DirectoryState {
  entries: FileTreeEntry[];
  truncated: boolean;
  loading: boolean;
  error: string | null;
}

function FilesPanel({
  workspacePath,
  workspaceAttached,
  running,
}: {
  workspacePath: string | null;
  workspaceAttached: boolean;
  running: boolean;
}) {
  const [directories, setDirectories] = useState<Record<string, DirectoryState>>({});
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [file, setFile] = useState<FileContent | null>(null);
  const [draft, setDraft] = useState("");
  const [dirty, setDirty] = useState(false);
  const [fileError, setFileError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [reloadToken, setReloadToken] = useState(0);

  const loadDirectory = useCallback(
    async (path: string) => {
      if (!workspacePath || !workspaceAttached) return;
      setDirectories((current) => ({
        ...current,
        [path]: {
          entries: current[path]?.entries ?? [],
          truncated: current[path]?.truncated ?? false,
          loading: true,
          error: null,
        },
      }));
      try {
        const listing = await fileList(workspacePath, path || null);
        setDirectories((current) => ({
          ...current,
          [path]: { ...listing, loading: false, error: null },
        }));
      } catch (cause) {
        setDirectories((current) => ({
          ...current,
          [path]: {
            entries: current[path]?.entries ?? [],
            truncated: false,
            loading: false,
            error: String(cause),
          },
        }));
      }
    },
    [workspacePath, workspaceAttached],
  );

  useEffect(() => {
    setDirectories({});
    setExpanded(new Set());
    setSelectedPath(null);
    setFile(null);
    setDraft("");
    setDirty(false);
    setFileError(null);
    setSaveError(null);
    if (workspacePath && workspaceAttached) void loadDirectory("");
  }, [workspacePath, workspaceAttached, loadDirectory]);

  useEffect(() => {
    if (!workspacePath || !workspaceAttached || !selectedPath) {
      setFile(null);
      return;
    }
    let disposed = false;
    setFile(null);
    setFileError(null);
    setSaveError(null);
    void fileRead(workspacePath, selectedPath)
      .then((next) => {
        if (disposed) return;
        setFile(next);
        setDraft(next.content);
        setDirty(false);
      })
      .catch((cause) => {
        if (!disposed) setFileError(String(cause));
      });
    return () => {
      disposed = true;
    };
  }, [workspacePath, workspaceAttached, selectedPath, reloadToken]);

  const toggleDirectory = (path: string) => {
    const willOpen = !expanded.has(path);
    setExpanded((current) => {
      const next = new Set(current);
      if (willOpen) next.add(path);
      else next.delete(path);
      return next;
    });
    if (willOpen && !directories[path]) void loadDirectory(path);
  };

  const selectFile = (path: string) => {
    if (path === selectedPath) return;
    if (dirty && !window.confirm("当前文件有未保存修改，仍要打开其他文件吗？")) return;
    setSelectedPath(path);
  };

  const reloadFile = () => {
    if (dirty && !window.confirm("重新加载会放弃未保存修改，是否继续？")) return;
    setReloadToken((value) => value + 1);
  };

  const saveFile = async () => {
    if (
      !workspacePath ||
      !selectedPath ||
      !file ||
      !file.is_editable ||
      !dirty ||
      saving
    ) {
      return;
    }
    setSaving(true);
    setSaveError(null);
    try {
      const saved = await fileWrite(workspacePath, selectedPath, draft, file.revision);
      setFile(saved);
      setDraft(saved.content);
      setDirty(false);
    } catch (cause) {
      setSaveError(String(cause));
    } finally {
      setSaving(false);
    }
  };

  const renderDirectory = (path: string, depth: number): React.ReactNode => {
    const directory = directories[path];
    if (!directory) return null;
    if (directory.loading && directory.entries.length === 0) {
      return <div className="files-tree-note" style={{ paddingInlineStart: 10 + depth * 14 }}>读取中…</div>;
    }
    if (directory.error) {
      return <div className="files-tree-note error" style={{ paddingInlineStart: 10 + depth * 14 }}>{directory.error}</div>;
    }
    return (
      <>
        {directory.entries.map((entry) => {
          const isOpen = entry.is_directory && expanded.has(entry.path);
          return (
            <div className="files-tree-node" key={entry.path}>
              <button
                type="button"
                className={`files-tree-row${selectedPath === entry.path ? " selected" : ""}`}
                style={{ paddingInlineStart: 8 + depth * 14 }}
                aria-expanded={entry.is_directory ? isOpen : undefined}
                title={entry.path}
                onClick={() => {
                  if (entry.is_directory) toggleDirectory(entry.path);
                  else selectFile(entry.path);
                }}
              >
                <span className="files-tree-arrow">{entry.is_directory ? (isOpen ? "⌄" : "›") : ""}</span>
                {entry.is_directory ? <IconProjects width={13} height={13} /> : <IconFile width={13} height={13} />}
                <span>{entry.name}</span>
              </button>
              {entry.is_directory && isOpen && renderDirectory(entry.path, depth + 1)}
            </div>
          );
        })}
        {directory.truncated && (
          <div className="files-tree-note" style={{ paddingInlineStart: 10 + depth * 14 }}>
            此目录条目过多，请用搜索定位文件。
          </div>
        )}
      </>
    );
  };

  if (!workspacePath) {
    return <div className="empty">此会话未附加文件夹。附加工作区后即可浏览和编辑文件。</div>;
  }
  if (!workspaceAttached) {
    return <div className="empty">工作区尚未就绪，暂时无法浏览或编辑本地文件。</div>;
  }

  return (
    <div className="files-wrap">
      <div className="files-tree" aria-label="工作区文件">
        <div className="files-tree-head">
          <IconProjects width={13} height={13} />
          <span>文件</span>
        </div>
        {renderDirectory("", 0)}
      </div>
      <div className="files-editor">
        {selectedPath ? (
          <>
            <div className="files-editor-head">
              <span className="files-path" title={selectedPath}>{selectedPath}</span>
              {file && (
                <span className="files-meta">
                  {file.total_lines} 行{file.truncated ? " · 已截断" : ""}{file.is_editable ? "" : " · 只读"}
                </span>
              )}
              <button className="btn ghost sm" disabled={!file || saving} onClick={reloadFile}>重新加载</button>
              <button
                className="btn accent sm"
                disabled={!file?.is_editable || !dirty || saving}
                onClick={() => void saveFile()}
              >
                {saving ? "保存中…" : "保存"}
              </button>
            </div>
            {running && <div className="files-running">智能体正在运行；保存时会检测磁盘是否已被改动。</div>}
            {fileError && <div className="panel-error">{fileError}</div>}
            {saveError && <div className="panel-error">{saveError}</div>}
            {!file && !fileError && <div className="empty">读取文件…</div>}
            {file && !file.is_editable && (
              <div className="files-readonly">
                <IconFile width={17} height={17} />
                <strong>此文件仅可预览</strong>
                <span>{file.truncated ? "文件超过 512 KiB。" : "文件包含二进制或非 UTF-8 内容。"}</span>
              </div>
            )}
            {file?.is_editable && (
              <textarea
                className="files-textarea"
                value={draft}
                spellCheck={false}
                onChange={(event) => {
                  setDraft(event.target.value);
                  setDirty(event.target.value !== file.content);
                }}
                onKeyDown={(event) => {
                  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
                    event.preventDefault();
                    void saveFile();
                  }
                }}
              />
            )}
          </>
        ) : (
          <div className="files-empty">
            <IconEditor width={20} height={20} />
            <strong>选择一个文件</strong>
            <span>从左侧目录树打开文本文件后，可直接编辑并显式保存。</span>
          </div>
        )}
      </div>
    </div>
  );
}

// ---------- Terminal ----------

function defaultShell(): string {
  const ua = navigator.userAgent;
  if (/windows/i.test(ua)) return "auto";
  if (/mac/i.test(ua)) return "zsh";
  return "bash";
}

const TERMINAL_OUTPUT_LIMIT = 200_000;

function appendTerminalOutput(current: string, incoming: string): string {
  if (!incoming) return current;
  const next = current + stripAnsi(incoming);
  return next.length > TERMINAL_OUTPUT_LIMIT
    ? next.slice(next.length - TERMINAL_OUTPUT_LIMIT)
    : next;
}

function TerminalPanel({
  workspacePath,
  workspaceAttached,
}: {
  workspacePath: string | null;
  workspaceAttached: boolean;
}) {
  const [terms, setTerms] = useState<TerminalInfo[]>([]);
  const [selId, setSelId] = useState<string | null>(null);
  const [out, setOut] = useState("");
  const [cmd, setCmd] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [loadedOutputId, setLoadedOutputId] = useState<string | null>(null);
  const outRef = useRef<HTMLPreElement>(null);
  const pinnedRef = useRef(true);
  const wrapRef = useRef<HTMLDivElement>(null);
  const resizeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const list = useCallback(async () => {
    try {
      const ts = await terminalList();
      setTerms(ts);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void list();
  }, [list]);

  const sel = selId ?? terms[0]?.id ?? null;

  // 终端输出可能已被 agent 或早前轮询消费；切换终端时必须从后端 scrollback
  // 快照恢复，而不是只等待下一次增量 read。
  useEffect(() => {
    if (!sel) {
      setOut("");
      setLoadedOutputId(null);
      return;
    }
    let dead = false;
    setOut("");
    setLoadedOutputId(null);
    void terminalSnapshot(sel)
      .then((snapshot) => {
        if (dead) return;
        setOut(stripAnsi(snapshot));
        setError(null);
        setLoadedOutputId(sel);
      })
      .catch((cause) => {
        if (!dead) setError(String(cause));
      });
    return () => {
      dead = true;
    };
  }, [sel]);

  // 输出 1s 轮询(strip ANSI 后 <pre> 展示)
  usePoll(
    async () => {
      if (!sel) return;
      try {
        const raw = await terminalRead(sel);
        setOut((current) => appendTerminalOutput(current, raw));
        setError(null);
      } catch (e) {
        setError(String(e));
      }
    },
    1000,
    sel != null && loadedOutputId === sel
  );

  // 贴底跟随
  useEffect(() => {
    const el = outRef.current;
    if (el && pinnedRef.current) el.scrollTop = el.scrollHeight;
  }, [out]);

  // 容器尺寸 → PTY resize(估算行列,400ms 防抖)
  useEffect(() => {
    const el = wrapRef.current;
    if (!el || !sel) return;
    const ro = new ResizeObserver(() => {
      if (resizeTimer.current) clearTimeout(resizeTimer.current);
      resizeTimer.current = setTimeout(() => {
        const cols = Math.max(20, Math.min(400, Math.floor(el.clientWidth / 7.5)));
        const rows = Math.max(5, Math.min(100, Math.floor(el.clientHeight / 16)));
        void terminalResize(sel, cols, rows).catch(() => {
          /* resize 失败不影响展示 */
        });
      }, 400);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [sel]);

  const create = async () => {
    if (!workspacePath || !workspaceAttached) {
      setError("先为这个会话附加一个文件夹，才能打开终端。");
      return;
    }
    setCreating(true);
    setError(null);
    try {
      const id = await terminalCreate(defaultShell(), workspacePath);
      await list();
      setSelId(id);
    } catch (e) {
      setError(String(e));
    } finally {
      setCreating(false);
    }
  };

  const kill = async (id: string) => {
    setError(null);
    try {
      await terminalKill(id);
      if (selId === id) setSelId(null);
      await list();
    } catch (e) {
      setError(String(e));
    }
  };

  const sendLine = async () => {
    if (!sel || !cmd.trim()) return;
    setError(null);
    const line = cmd;
    setCmd("");
    try {
      await terminalSend(sel, line, true);
    } catch (e) {
      setError(String(e));
      setCmd(line);
    }
  };

  return (
    <div className="term-wrap" ref={wrapRef}>
      <div className="term-side">
        <button className="btn sm term-new" disabled={creating || !workspacePath || !workspaceAttached} onClick={() => void create()}>
          <IconPlus width={11} height={11} /> 新建终端
        </button>
        {terms.map((t) => (
          <div
            key={t.id}
            className={"term-row" + (sel === t.id ? " sel" : "")}
            onClick={() => setSelId(t.id)}
          >
            <IconTerminal width={12} height={12} />
            <span className="t-id" title={t.id}>
              {t.id.slice(0, 8)}
            </span>
            <span className={"lamp" + (t.state === "exited" ? " done" : t.is_busy ? " run" : "")} />
            <button
              className="t-kill"
              title="终止终端"
              onClick={(e) => {
                e.stopPropagation();
                void kill(t.id);
              }}
            >
              ✕
            </button>
          </div>
        ))}
        {terms.length === 0 && <div className="term-hint">无终端</div>}
      </div>
      <div className="term-main">
        {error && <div className="panel-error">{error}</div>}
        {sel ? (
          <>
            <pre
              className="term-out"
              ref={outRef}
              onScroll={(e) => {
                const el = e.currentTarget;
                pinnedRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
              }}
            >
              {out || " "}
            </pre>
            <div className="term-in">
              <input
                className="input"
                value={cmd}
                placeholder="输入命令,Enter 注入终端"
                onChange={(e) => setCmd(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    void sendLine();
                  }
                }}
              />
            </div>
          </>
        ) : (
          <div className="empty">
            {workspacePath && workspaceAttached
              ? "还没有终端 — 点击左侧「新建终端」（将自动选择可用 shell，cwd = 工作区根）。"
              : "附加一个工作区后，才能使用终端。"}
          </div>
        )}
      </div>
    </div>
  );
}

// ---------- Review ----------

const VER_STATUS: Record<string, { cls: string; label: string }> = {
  running: { cls: "run", label: "运行中" },
  passed: { cls: "ok", label: "通过" },
  failed: { cls: "bad", label: "失败" },
  timeout: { cls: "bad", label: "超时" },
  stale: { cls: "", label: "已过期" },
  superseded: { cls: "", label: "被取代" },
};

function ReviewPanel({ taskId }: { taskId: string }) {
  const [records, setRecords] = useState<VerificationRecord[]>([]);
  const [cmd, setCmd] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [runningCmd, setRunningCmd] = useState(false);
  const [confirm, setConfirm] = useState<null | "accept" | "rollback">(null);
  const confirmTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  // 输出查看（点击记录行展开，懒加载 + 缓存）
  const [openId, setOpenId] = useState<string | null>(null);
  const [outputs, setOutputs] = useState<Record<string, string>>({});

  const toggleOutput = async (id: string) => {
    if (openId === id) {
      setOpenId(null);
      return;
    }
    setOpenId(id);
    if (outputs[id] !== undefined) return;
    try {
      const text = await verificationOutput(id);
      setOutputs((o) => ({ ...o, [id]: text || "（无输出）" }));
    } catch (e) {
      setOutputs((o) => ({ ...o, [id]: `读取输出失败：${String(e)}` }));
    }
  };

  usePoll(
    async () => {
      try {
        const rs = await verificationList(taskId);
        setRecords([...rs].sort((a, b) => b.started_at.localeCompare(a.started_at)));
        setError(null);
      } catch (e) {
        setError(String(e));
      }
    },
    2000,
    true
  );

  const run = async () => {
    const command = cmd.trim();
    if (!command || runningCmd) return;
    setRunningCmd(true);
    setError(null);
    setNotice(null);
    try {
      await runVerification(taskId, command);
      setRecords(await verificationList(taskId));
    } catch (e) {
      setError(String(e));
    } finally {
      setRunningCmd(false);
    }
  };

  const act = async (kind: "accept" | "rollback") => {
    if (confirm !== kind) {
      setConfirm(kind);
      if (confirmTimer.current) clearTimeout(confirmTimer.current);
      confirmTimer.current = setTimeout(() => setConfirm(null), 3000);
      return;
    }
    setConfirm(null);
    setError(null);
    setNotice(null);
    try {
      if (kind === "accept") {
        await acceptTask(taskId);
        setNotice("已接受全部变更,任务关闭。");
      } else {
        const results = await rollbackTask(taskId);
        setNotice(`已回滚 ${results.length} 个文件。`);
      }
      await refreshDetail(taskId);
      await refreshTasks();
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="review-wrap">
      <div className="review-gate">
        <input
          className="input"
          value={cmd}
          placeholder="验证命令，例如 cargo test 或 npm test"
          onChange={(e) => setCmd(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              void run();
            }
          }}
        />
        <button className="btn accent" disabled={!cmd.trim() || runningCmd} onClick={() => void run()}>
          {runningCmd ? "运行中…" : "运行验证"}
        </button>
      </div>
      {error && <div className="panel-error">{error}</div>}
      {notice && <div className="panel-note">{notice}</div>}
      <div className="review-list">
        {records.length === 0 && <div className="empty">还没有验证记录 — 审阅门是显式的:先跑一条命令。</div>}
        {records.map((r) => {
          const st = VER_STATUS[r.status] ?? { cls: "", label: r.status };
          return (
            <div key={r.id}>
              <div
                className={"ver-row" + (openId === r.id ? " open" : "")}
                onClick={() => void toggleOutput(r.id)}
                title="点击查看输出"
              >
                <span className={"st-chip " + st.cls}>{st.label}</span>
                <span className="ver-cmd" title={r.command}>
                  {r.command}
                </span>
                <span className="ver-meta">
                  exit {r.exit_code ?? "—"} · {dur(r)} · {clockTime(r.started_at)}
                </span>
              </div>
              {openId === r.id && (
                <pre className="ver-output">{outputs[r.id] ?? "读取中…"}</pre>
              )}
            </div>
          );
        })}
      </div>
      <div className="review-actions">
        <button
          className={"btn accent" + (confirm === "accept" ? " confirm" : "")}
          onClick={() => void act("accept")}
        >
          <IconCheck width={12} height={12} /> {confirm === "accept" ? "确认接受?" : "Accept all"}
        </button>
        <button
          className={"btn danger" + (confirm === "rollback" ? " confirm" : "")}
          onClick={() => void act("rollback")}
        >
          {confirm === "rollback" ? "确认回滚?" : "Rollback"}
        </button>
        <span className="review-hint">审阅门永远显式 — 接受或回滚,不留中间态。</span>
      </div>
    </div>
  );
}

function dur(r: VerificationRecord): string {
  if (!r.ended_at) return "…";
  const ms = Date.parse(r.ended_at) - Date.parse(r.started_at);
  if (Number.isNaN(ms) || ms < 0) return "—";
  return `${(ms / 1000).toFixed(1)}s`;
}
