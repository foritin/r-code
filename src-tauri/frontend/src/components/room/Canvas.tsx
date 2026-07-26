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
  sessionMessages,
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
  ChangeDiffLine,
  FileChange,
  ProjectAccessMode,
  SessionMessage,
  TaskDetail,
  TerminalInfo,
  VerificationRecord,
} from "../../lib/types";
import { useAppStore, type CanvasTab } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { usePoll } from "../../lib/poll";
import { useArmedAction } from "../../lib/hooks";
import { isTypingTarget } from "../../lib/keys";
import {
  clockSeconds,
  clockTime,
  displayPath,
  elapsedMinutes,
  modeLabel,
  modeShortLabel,
} from "../../lib/format";
import { stripAnsi } from "./model";
import { buildAuditFeed } from "./audit";
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

// ---------- 列表键盘导航（三个面板共用） ----------
//
// Changes / Terminal / Review 三个列表原本是纯 `<div onClick>`，键盘完全够不着。
// 这里沿用本文件顶部 tablist 已有的 roving tabindex 约定：整个列表只有一行进
// tab 序（tabIndex=0），其余 -1，方向键在行间移动 DOM 焦点。
//
// Changes / Terminal 的行里嵌了 回滚 / 终止 按钮，button 不能套 button；listbox
// 同样不行 —— ARIA 1.2 给 role=option 规定了 Children Presentational: True，行的
// 子节点会被整个剥离出无障碍树，那两个按钮虽然进了 tab 序，读屏却拿不到它们的
// aria-label。所以这两个列表走 grid：gridcell 明确允许交互式子节点，role=row 又
// 保留了 aria-selected（选中语义不丢），焦点落在行上是 APG 布局网格允许的形态。
// 行内只有两格：主格（类型 + 路径 / 图标 + id + 状态灯）和操作格（那颗按钮）。
// Review 行没有内嵌按钮，直接用真 `<button>`（同时是展开输出的 disclosure）。
//
// 三个列表都不用 aria-activedescendant：它只在持有该属性的元素自己拿到 DOM 焦点
// 时才有意义，而这里焦点在行上；APG 也把它和 roving tabindex 列为二选一。

const chgRowId = (index: number) => `chg-row-${index}`;
const termRowId = (index: number) => `term-row-${index}`;
const verRowId = (index: number) => `ver-row-${index}`;

/**
 * ↑ / ↓ 在行间循环移动焦点，Home / End 跳首尾。
 * 行自己用 onFocus 回写 roving 下标，这里只负责把 DOM 焦点挪过去。
 * 返回 true 表示按键已消费，调用方不必再处理。
 */
function moveRowFocus(
  event: React.KeyboardEvent<HTMLElement>,
  index: number,
  count: number,
  rowId: (index: number) => string
): boolean {
  if (count <= 0) return false;
  let next: number;
  if (event.key === "ArrowDown") next = (index + 1) % count;
  else if (event.key === "ArrowUp") next = (index - 1 + count) % count;
  else if (event.key === "Home") next = 0;
  else if (event.key === "End") next = count - 1;
  else return false;
  event.preventDefault();
  // 目标行此刻 tabIndex 还是 -1，但程序化 focus() 对 -1 同样有效。
  document.getElementById(rowId(next))?.focus();
  return true;
}

/** Enter / Space 激活；行内按钮（回滚 / 终止）冒泡上来的按键不算，交给它们自己处理。 */
function isRowActivate(event: React.KeyboardEvent<HTMLElement>): boolean {
  if (event.target !== event.currentTarget) return false;
  return event.key === "Enter" || event.key === " ";
}

/** roving tabindex 的落点：优先跟随最近一次焦点，否则回到选中行（都没有就第一行）。 */
function rovingIndexOf(focusIndex: number, selectedIndex: number, count: number): number {
  if (focusIndex >= 0 && focusIndex < count) return focusIndex;
  return selectedIndex >= 0 && selectedIndex < count ? selectedIndex : 0;
}

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
      <div
        className="canvas-tabs"
        role="tablist"
        aria-label="画布视图"
        onKeyDown={(event) => {
          // ← → 在页签间移动，符合 WAI-ARIA tablist 约定
          if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
          event.preventDefault();
          const index = TABS.findIndex((item) => item.id === tab);
          const delta = event.key === "ArrowRight" ? 1 : -1;
          const next = TABS[(index + delta + TABS.length) % TABS.length];
          setTab(next.id);
          requestAnimationFrame(() => {
            document.getElementById(`ctab-${next.id}`)?.focus();
          });
        }}
      >
        {TABS.map((t) => (
          <button
            key={t.id}
            id={`ctab-${t.id}`}
            role="tab"
            aria-selected={tab === t.id}
            aria-controls="canvas-panel"
            tabIndex={tab === t.id ? 0 : -1}
            className={"ctab ring-inset" + (tab === t.id ? " on" : "")}
            onClick={() => setTab(t.id)}
          >
            {t.label}
            {t.id === "changes" && (
              <span className="n" aria-label={`${detail?.changes.length ?? 0} 项变更`}>
                {detail?.changes.length ?? 0}
              </span>
            )}
          </button>
        ))}
        {running && (
          <span className="canvas-live" title={activity.label}>
            <i /> live · {activity.label}{workingSubagents > 0 ? ` · 子代理 ${workingSubagents}` : ""}
          </span>
        )}
      </div>
      <div className="canvas-body" id="canvas-panel" role="tabpanel" tabIndex={-1}>
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

/** 审计流一次展示的最大条数；再多就该去时间线或 Review 页翻。 */
const AUDIT_LIMIT = 12;

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
  const [messages, setMessages] = useState<SessionMessage[]>([]);
  const taskId = detail?.task.id ?? null;
  // 审计流的工具明细来自会话 JSONL（tool_call/tool_result 才有工具名、输入、输出），
  // task_events 只提供时间锚点。RoomScene 已经 2s 刷新 detail，事件/变更/验证数量一变
  // 就说明有新动作落盘，据此重取即可，不再叠加一层定时轮询。
  const auditStamp = detail
    ? `${detail.events.length}:${detail.changes.length}:${detail.verifications.length}`
    : "";

  useEffect(() => {
    setMessages([]);
  }, [taskId]);

  useEffect(() => {
    if (!taskId) return;
    let dead = false;
    sessionMessages(taskId)
      .then((list) => {
        if (!dead) setMessages(list);
      })
      .catch(() => {
        /* 审计流是只读视图，取不到就保留上一批，等下一次事件变化重试 */
      });
    return () => {
      dead = true;
    };
  }, [taskId, auditStamp]);

  const audit = useMemo(
    () =>
      detail
        ? buildAuditFeed(
            {
              messages,
              events: detail.events,
              changes: detail.changes,
              verifications: detail.verifications,
              permissions: detail.permissions,
              runs: detail.runs,
            },
            AUDIT_LIMIT
          )
        : [],
    [detail, messages]
  );

  if (!detail) return <div className="empty">加载中…</div>;
  const { task, runs, changes, permissions, verifications, queued_messages: queuedMessages } = detail;
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

  // 会话模式（Ask/Edit/Auto）和项目权限（请求批准/风险/完全）是两件事，但连读起来
  // 高度重合。合成一枚「策略」芯片：短标签并排，完整解释放 title，不再单独占一格指标。
  const accessLabel =
    workspaceAttached && workspaceAccessMode ? projectAccessModeLabel(workspaceAccessMode) : null;
  const policyLabel = `${modeShortLabel(task.mode)} · ${accessLabel ?? "仅聊天"}`;
  const policyTitle = `${modeLabel(task.mode)}\n项目权限：${accessLabel ?? "未附加文件夹，只能聊天"}`;

  return (
    <div className="sum-wrap">
      <div className="sum-head">
        <span className={"st-chip " + (task.state === "review_ready" ? "warn" : runningCls(task.state))}>
          {STATE_LABEL[task.state] ?? task.state}
        </span>
        <span className="sum-title" title={title}>{title}</span>
        <span className="sum-age" title={`会话开始于 ${clockSeconds(task.created_at)}`}>
          {elapsedMinutes(task.created_at)}
        </span>
      </div>
      {hasDistinctGoal && <div className="sum-goal" title={task.goal}>{task.goal}</div>}
      <div className="sum-scope">
        <span title={workspacePath ? displayPath(workspacePath) : "未附加文件夹"}>
          {workspaceLabel}
        </span>
        <span title={modelLabel}>模型 · {modelLabel}</span>
        <span className={workspaceAttached ? "scoped" : ""} title={policyTitle}>
          {policyLabel}
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
      {/* 常驻三格都是可点开的产出；队列和子代理只在真的有值时出现，避免长期陈列 0。 */}
      <div className="sum-grid">
        <button className="sum-cell action" onClick={() => setTab("changes")}>
          <div className="k">变更文件</div>
          <div className="v">{changes.length}</div>
        </button>
        <button className="sum-cell action" onClick={() => setTab("review")}>
          <div className="k">验证</div>
          <div className="v">
            {verifications.length === 0 ? "未运行" : `${passed}/${verifications.length} 通过`}
          </div>
        </button>
        <button className="sum-cell action" onClick={() => setTab("review")}>
          <div className="k">待批权限</div>
          <div className={"v" + (pending > 0 ? " warn" : "")}>{pending}</div>
        </button>
        {queued > 0 && (
          <div className="sum-cell">
            <div className="k">队列</div>
            <div className="v warn">{queued}</div>
          </div>
        )}
        {subagents.length > 0 && (
          <div className="sum-cell">
            <div className="k">子代理</div>
            <div className={"v" + (activeSubagents > 0 ? " warn" : "")}>
              {activeSubagents > 0 ? `${activeSubagents} 运行中 / ` : ""}{subagents.length}
            </div>
          </div>
        )}
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
      <div className="zone-head">
        运行审计
        <span className="zone-hint">工具 · 目标 · 结果</span>
      </div>
      {audit.length === 0 ? (
        <div className="sum-empty">会话尚未产生可审计的动作。</div>
      ) : (
        <div className="audit-list">
          {audit.map((row) => (
            <div
              className={`audit-row kind-${row.kind} state-${row.state}`}
              key={row.id}
              title={row.title}
            >
              <span className="audit-tag">{row.tag}</span>
              <span className="audit-text">{row.text}</span>
              {row.result && <span className="audit-result">{row.result}</span>}
              <span className="audit-at">{clockSeconds(row.atIso)}</span>
            </div>
          ))}
        </div>
      )}
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
  // A11Y-005：设置里的「文本差异视图」开关，此处是它唯一的消费点。
  const accessibleDiff = useAppStore((s) => s.accessibleDiffMode);
  const [sel, setSel] = useState<string | null>(null);
  const [diff, setDiff] = useState<ChangeDiff | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [confirmPath, setConfirmPath] = useState<string | null>(null);
  const [rowFocus, setRowFocus] = useState(-1);
  const confirmTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const path = sel ?? changes[0]?.path ?? null;
  const selIndex = changes.findIndex((item) => item.path === path);
  const rovingIndex = rovingIndexOf(rowFocus, selIndex, changes.length);

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
  // 已经跳过一次的 f7Idx。detail 每 2s 轮询 → changes/lines/changePoints 全是新引用，
  // 下面那个 effect 会跟着重跑；只认引用的话 scrollIntoView + focus 就会每 2 秒把焦点
  // 从用户当前位置抢回 diff 行。用它把「effect 重跑」和「用户真的按了 F7」区分开。
  const f7DoneRef = useRef(-1);

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
    // 只有 f7Idx 真的变了才跳。挂载时 f7Idx 与 f7DoneRef 同为 -1，天然不触发；
    // 轮询刷新只换引用不换 f7Idx，走到这里就直接返回，焦点留在用户自己那儿。
    if (f7Idx === f7DoneRef.current) return;
    if (f7Idx < 0 || f7Idx >= changePoints.length) return;
    // 按 lines 下标定位，两种呈现共用同一套 data-dline，也顺手修掉了原先按
    // querySelectorAll(".dl") 序号定位在 truncated 时整体偏一行的问题。
    const target = diffBodyRef.current?.querySelector<HTMLElement>(
      `[data-dline="${changePoints[f7Idx]}"]`
    );
    // 目标行还没渲染出来（切文件那一帧）：不记账，等下一次 render 再跳。
    if (!target) return;
    f7DoneRef.current = f7Idx;
    target.scrollIntoView({ block: "center", behavior: "smooth" });
    // 无障碍模式下把焦点也放到该行，屏幕阅读器才会读出跳到了哪一行。
    if (accessibleDiff) target.focus({ preventScroll: true });
  }, [f7Idx, changePoints, accessibleDiff]);

  // 无障碍模式：按 @@ 头把行切成变更块，块标题给出「第 N 行起，新增 X 行，删除 Y 行」。
  const hunks = useMemo<DiffHunk[]>(() => {
    if (!accessibleDiff || lines.length === 0) return [];
    let current: DiffHunk = { key: 0, raw: null, start: null, adds: 0, dels: 0, items: [] };
    const groups: DiffHunk[] = [current];
    for (let index = 0; index < lines.length; index++) {
      const line = lines[index];
      if (line.kind === "hunk") {
        if (current.items.length === 0) {
          current.raw = line.text;
        } else {
          current = { key: groups.length, raw: line.text, start: null, adds: 0, dels: 0, items: [] };
          groups.push(current);
        }
        continue;
      }
      if (current.start == null) current.start = line.new_no ?? line.old_no ?? null;
      if (line.kind === "add") current.adds += 1;
      else if (line.kind === "del") current.dels += 1;
      current.items.push({ index, line });
    }
    return groups.filter((group) => group.items.length > 0);
  }, [accessibleDiff, lines]);

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
        {changes.length > 0 && (
          <div className="chg-options" role="grid" aria-label="变更文件">
            {changes.map((c, index) => (
              <div
                key={c.id}
                id={chgRowId(index)}
                role="row"
                aria-selected={path === c.path}
                tabIndex={index === rovingIndex ? 0 : -1}
                className={"chg-row ring-inset" + (path === c.path ? " sel" : "")}
                onFocus={() => setRowFocus(index)}
                onClick={() => setSel(c.path)}
                onKeyDown={(event) => {
                  if (moveRowFocus(event, index, changes.length, chgRowId)) return;
                  if (!isRowActivate(event)) return;
                  event.preventDefault();
                  setSel(c.path);
                }}
              >
                <span className="rcell rcell-main" role="gridcell">
                  {/* new/mod/del/ren 三字母缩写只有视觉意义，读屏走后面的完整中文 */}
                  <span className={"chg-type t-" + c.change_type} aria-hidden="true">
                    {typeLabel(c)}
                  </span>
                  <span className="sr-only">{typeFullLabel(c)}</span>
                  <span className="chg-path" title={c.path}>
                    {c.path}
                  </span>
                </span>
                <span className="rcell" role="gridcell">
                  <button
                    className={"chg-rb" + (confirmPath === c.path ? " confirm" : "")}
                    // 只有 roving 落点那一行的行内按钮进 tab 序，否则 Tab 会把整列按钮走一遍
                    tabIndex={index === rovingIndex ? 0 : -1}
                    title={confirmPath === c.path ? "再次点击确认回滚" : "回滚此文件"}
                    aria-label={
                      confirmPath === c.path ? `再次确认，回滚 ${c.path}` : `回滚 ${c.path}`
                    }
                    onClick={(e) => {
                      e.stopPropagation();
                      void doRollback(c.path);
                    }}
                  >
                    {confirmPath === c.path ? "确认?" : "回滚"}
                  </button>
                </span>
              </div>
            ))}
          </div>
        )}
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
                <span
                  className="kact"
                  title="F7 下一个变更，Shift + F7 上一个"
                  aria-live={accessibleDiff ? "polite" : undefined}
                >
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
            <div
              // 无障碍模式下整块是可聚焦的滚动区（键盘能滚），ring-inset 避免焦点环被裁
              className={"diff-body" + (accessibleDiff ? " diff-body-a11y ring-inset" : "")}
              ref={diffBodyRef}
              role={accessibleDiff ? "region" : undefined}
              aria-label={accessibleDiff ? `${diff.path} 的文本差异` : undefined}
              tabIndex={accessibleDiff ? 0 : undefined}
            >
              {accessibleDiff ? (
                <>
                  {diff.truncated && (
                    <p className="dnote">diff 过大，已按「全删全增」截断呈现。</p>
                  )}
                  {lines.length > 0 && (
                    <p className="dsum">
                      共 {hunks.length} 个变更块，新增 {adds} 行，删除 {dels} 行。
                    </p>
                  )}
                  {hunks.map((hunk) => (
                    <div
                      className="dhunk"
                      role="group"
                      aria-labelledby={`dhunk-${hunk.key}`}
                      key={hunk.key}
                    >
                      <h3 className="dhunk-head" id={`dhunk-${hunk.key}`}>
                        {hunkTitle(hunk)}
                        {hunk.raw && (
                          <span className="dhunk-raw" aria-hidden="true">
                            {hunk.raw}
                          </span>
                        )}
                      </h3>
                      <ol className="dlines" role="list">
                        {hunk.items.map(({ index, line }) => {
                          const no = line.new_no ?? line.old_no ?? null;
                          return (
                            <li
                              className={
                                "dla ring-inset dla-" +
                                line.kind +
                                (f7Idx >= 0 && changePoints[f7Idx] === index ? " f7-cur" : "")
                              }
                              key={index}
                              data-dline={index}
                              tabIndex={-1}
                            >
                              {/* 增删靠这段文本区分，不依赖底色；上下文行按约定留空 */}
                              <span className="dla-mark">{DIFF_MARK[line.kind]}</span>
                              <span className="dla-no">
                                {no == null ? (
                                  <span className="sr-only">无行号</span>
                                ) : (
                                  <>
                                    <span className="sr-only">第 </span>
                                    {no}
                                    <span className="sr-only"> 行</span>
                                  </>
                                )}
                              </span>
                              <span className="dla-code">{line.text}</span>
                            </li>
                          );
                        })}
                      </ol>
                    </div>
                  ))}
                </>
              ) : (
                <>
                  {diff.truncated && <div className="dl hunk">diff 过大,已截断(全删全增)</div>}
                  {lines.map((l, i) =>
                    l.kind === "hunk" ? (
                      <div className="dl hunk" key={i} data-dline={i}>
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
                        data-dline={i}
                      >
                        <span className="no">{l.new_no ?? l.old_no ?? ""}</span>
                        <span className="code">{l.text}</span>
                      </div>
                    )
                  )}
                </>
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

/** 读屏用的完整中文，替掉只有视觉意义的三字母缩写 + 颜色。 */
function typeFullLabel(c: FileChange): string {
  switch (c.change_type) {
    case "create":
      return "新增文件";
    case "modify":
      return "修改文件";
    case "delete":
      return "删除文件";
    case "rename":
      return "重命名文件";
  }
}

// ---------- 无障碍 diff（accessibleDiffMode） ----------

/** 一个 @@ 变更块；items 里的 index 是行在 lines 中的下标，F7 靠它对齐。 */
interface DiffHunk {
  key: number;
  /** 原始 @@ 头，仅作视觉参考 */
  raw: string | null;
  /** 块内第一行的行号 */
  start: number | null;
  adds: number;
  dels: number;
  items: { index: number; line: ChangeDiffLine }[];
}

/** 每行前置的显式文本标记；上下文行留空，读屏不会被噪声淹没。 */
const DIFF_MARK: Record<ChangeDiffLine["kind"], string> = {
  add: "+ 新增",
  del: "- 删除",
  ctx: "",
  hunk: "",
};

function hunkTitle(hunk: DiffHunk): string {
  const where = hunk.start == null ? "文件开头" : `第 ${hunk.start} 行起`;
  if (hunk.adds === 0 && hunk.dels === 0) return `${where}，${hunk.items.length} 行上下文`;
  const parts: string[] = [];
  if (hunk.adds > 0) parts.push(`新增 ${hunk.adds} 行`);
  if (hunk.dels > 0) parts.push(`删除 ${hunk.dels} 行`);
  return `${where}，${parts.join("、")}`;
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
  // 未保存时的二次确认走项目自研的 armed 模式：window.confirm 在 Tauri WebView 里
  // 抢焦点、无法主题化，也拿不到统一焦点环。
  const [pendingPath, setPendingPath] = useState<string | null>(null);
  const switchGuard = useArmedAction(() => {
    if (!pendingPath) return;
    setSelectedPath(pendingPath);
    setPendingPath(null);
  });
  const reloadGuard = useArmedAction(() => setReloadToken((value) => value + 1));

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
    setPendingPath(null);
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
    if (!dirty) {
      switchGuard.disarm();
      setPendingPath(null);
      setSelectedPath(path);
      return;
    }
    // 换了目标文件就重新计时，避免上一个待确认的目标被误提交
    if (pendingPath !== path) {
      switchGuard.disarm();
      setPendingPath(path);
    }
    switchGuard.trigger();
  };

  const reloadFile = () => {
    if (!dirty) {
      reloadGuard.disarm();
      setReloadToken((value) => value + 1);
      return;
    }
    reloadGuard.trigger();
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
              <button
                className={"btn ghost sm" + (reloadGuard.armed ? " confirm" : "")}
                disabled={!file || saving}
                onClick={reloadFile}
              >
                {reloadGuard.armed ? "确认放弃修改?" : "重新加载"}
              </button>
              <button
                className="btn accent sm"
                disabled={!file?.is_editable || !dirty || saving}
                onClick={() => void saveFile()}
              >
                {saving ? "保存中…" : "保存"}
              </button>
            </div>
            {switchGuard.armed && pendingPath && (
              <div className="files-guard" role="status">
                <span>
                  当前文件有未保存修改。再次点击 <strong>{pendingPath}</strong> 将放弃修改并打开它。
                </span>
                <button
                  className="btn ghost sm"
                  onClick={() => {
                    switchGuard.disarm();
                    setPendingPath(null);
                  }}
                >
                  取消
                </button>
              </div>
            )}
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

/** .lamp 只有颜色，读屏得靠这段文本。 */
function terminalStateLabel(t: TerminalInfo): string {
  if (t.state === "exited") return "已退出";
  return t.is_busy ? "运行中" : "空闲";
}

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
  const [rowFocus, setRowFocus] = useState(-1);
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
  const selIndex = terms.findIndex((item) => item.id === sel);
  const rovingIndex = rovingIndexOf(rowFocus, selIndex, terms.length);

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
        {terms.length > 0 && (
          <div className="term-options" role="grid" aria-label="终端列表">
            {terms.map((t, index) => (
              <div
                key={t.id}
                id={termRowId(index)}
                role="row"
                aria-selected={sel === t.id}
                tabIndex={index === rovingIndex ? 0 : -1}
                className={"term-row ring-inset" + (sel === t.id ? " sel" : "")}
                onFocus={() => setRowFocus(index)}
                onClick={() => setSelId(t.id)}
                onKeyDown={(event) => {
                  if (moveRowFocus(event, index, terms.length, termRowId)) return;
                  if (!isRowActivate(event)) return;
                  event.preventDefault();
                  setSelId(t.id);
                }}
              >
                <span className="rcell rcell-main" role="gridcell">
                  <IconTerminal width={12} height={12} aria-hidden="true" />
                  <span className="t-id" title={t.id}>
                    {t.id.slice(0, 8)}
                  </span>
                  {/* 状态灯是纯颜色编码，读屏走后面的文本 */}
                  <span
                    className={"lamp" + (t.state === "exited" ? " done" : t.is_busy ? " run" : "")}
                    aria-hidden="true"
                  />
                  <span className="sr-only">{terminalStateLabel(t)}</span>
                </span>
                <span className="rcell" role="gridcell">
                  <button
                    className="t-kill"
                    tabIndex={index === rovingIndex ? 0 : -1}
                    title="终止终端"
                    aria-label={`终止终端 ${t.id.slice(0, 8)}`}
                    onClick={(e) => {
                      e.stopPropagation();
                      void kill(t.id);
                    }}
                  >
                    ✕
                  </button>
                </span>
              </div>
            ))}
          </div>
        )}
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
  const [rowFocus, setRowFocus] = useState(-1);

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

  // 没有「选中」概念，roving 落点退回已展开的那一条，都没有就第一条
  const rovingIndex = rovingIndexOf(rowFocus, records.findIndex((r) => r.id === openId), records.length);

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
        {records.length > 0 && (
          <div className="ver-items" role="list" aria-label="验证记录">
            {records.map((r, index) => {
              const st = VER_STATUS[r.status] ?? { cls: "", label: r.status };
              const open = openId === r.id;
              return (
                <div key={r.id} role="listitem">
                  {/* 行内没有嵌套按钮，可以直接用真 button（同时是输出的 disclosure） */}
                  <button
                    type="button"
                    id={verRowId(index)}
                    className={"ver-row ring-inset" + (open ? " open" : "")}
                    tabIndex={index === rovingIndex ? 0 : -1}
                    aria-expanded={open}
                    aria-controls={open ? `ver-out-${index}` : undefined}
                    onFocus={() => setRowFocus(index)}
                    onClick={() => void toggleOutput(r.id)}
                    onKeyDown={(event) => {
                      moveRowFocus(event, index, records.length, verRowId);
                    }}
                    title="展开或收起输出"
                  >
                    <span className={"st-chip " + st.cls}>{st.label}</span>
                    <span className="ver-cmd" title={r.command}>
                      {r.command}
                    </span>
                    <span className="ver-meta">
                      exit {r.exit_code ?? "—"} · {dur(r)} · {clockTime(r.started_at)}
                    </span>
                  </button>
                  {open && (
                    <pre className="ver-output" id={`ver-out-${index}`}>
                      {outputs[r.id] ?? "读取中…"}
                    </pre>
                  )}
                </div>
              );
            })}
          </div>
        )}
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
