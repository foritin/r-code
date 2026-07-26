/**
 * Fleet Rows —— Deck 的密度看板（fusion-obsidian.html:354-414, 830-916）。
 * 9 列栅格行：cbox / title / proj / branch / act / gates / diff / time / keys。
 * 键盘（仅 rows 密度激活，isTypingTarget 过滤）：
 *   j/k 导航 · x 多选 · esc 清除 · ⏎ open room · e editor
 *   g grant / d deny（permission 行）· a accept / p peek / r rollback（review 行，r 二次确认）
 * 多选时 g/d/a 走批量（循环单命令），底部批量条同步显示。
 */
import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import { useAppStore } from "../../store/app";
import { selectNeedsYou, useTasksStore } from "../../store/tasks";
import { acceptTask, permissionApprove, rollbackTask } from "../../lib/ipc";
import { clockTime, elapsedSince, lampFor } from "../../lib/format";
import { isTypingTarget } from "../../lib/keys";
import {
  actionLineFor,
  baseName,
  gatesFor,
  latestVerification,
  projectName,
  settledItems,
  type GateState,
  type SettledOutcome,
} from "../../lib/deck";
import type { PermissionRequest, Task, TaskDetail, Workspace } from "../../lib/types";

interface RowItem {
  key: string;
  task: Task;
  kind: "permission" | "review" | "running" | "settled";
  permission?: PermissionRequest;
  outcome?: SettledOutcome;
  when?: string;
}

interface FleetRowsProps {
  onRefresh: (taskId: string) => Promise<void>;
  onError: (message: string) => void;
}

export function FleetRows({ onRefresh, onError }: FleetRowsProps) {
  const active = useAppStore((s) => s.deckDensity) === "rows";
  const openRoom = useAppStore((s) => s.openRoom);
  const setCanvasTab = useAppStore((s) => s.setCanvasTab);
  const setScene = useAppStore((s) => s.setScene);
  const tasks = useTasksStore((s) => s.tasks);
  const details = useTasksStore((s) => s.details);
  const workspaces = useTasksStore((s) => s.workspaces);
  const needsYou = useTasksStore(selectNeedsYou);

  const [kb, setKb] = useState(0);
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [rollbackArm, setRollbackArm] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const armTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const needsRows = useMemo<RowItem[]>(
    () =>
      needsYou.map((i) =>
        i.kind === "permission" && i.permission
          ? { key: `perm:${i.permission.id}`, task: i.task, kind: "permission", permission: i.permission }
          : { key: `review:${i.task.id}`, task: i.task, kind: "review" },
      ),
    [needsYou],
  );
  const runningRows = useMemo<RowItem[]>(
    () =>
      tasks
        .filter((t) => t.state === "in_progress" || t.state === "exploring")
        .map((t) => ({ key: `run:${t.id}`, task: t, kind: "running" as const })),
    [tasks],
  );
  const settledRows = useMemo<RowItem[]>(
    () =>
      settledItems(tasks, details, 10).map((s) => ({
        key: `settled:${s.task.id}`,
        task: s.task,
        kind: "settled" as const,
        outcome: s.outcome,
        when: s.when,
      })),
    [tasks, details],
  );

  const rows = useMemo(
    () => [...needsRows, ...runningRows, ...settledRows],
    [needsRows, runningRows, settledRows],
  );
  const sections = [
    { title: "Needs you", attn: true, rows: needsRows },
    { title: "Running", attn: false, rows: runningRows },
    { title: "Settled", attn: false, rows: settledRows },
  ];

  // 行集变化后收回越界的 kb 光标与失效多选
  useEffect(() => {
    setKb((i) => Math.min(i, Math.max(0, rows.length - 1)));
    setSelected((s) => {
      const keys = new Set(rows.map((r) => r.key));
      const next = new Set([...s].filter((k) => keys.has(k)));
      return next.size === s.size ? s : next;
    });
  }, [rows]);

  useEffect(
    () => () => {
      if (armTimer.current) clearTimeout(armTimer.current);
    },
    [],
  );

  const exec = async (fn: () => Promise<void>) => {
    if (busy) return;
    setBusy(true);
    try {
      await fn();
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const grantRow = (r: RowItem) =>
    exec(async () => {
      if (!r.permission) return;
      await permissionApprove(r.permission.id, "allow");
      await onRefresh(r.task.id);
    });
  const denyRow = (r: RowItem) =>
    exec(async () => {
      if (!r.permission) return;
      await permissionApprove(r.permission.id, "deny");
      await onRefresh(r.task.id);
    });
  const acceptRow = (r: RowItem) =>
    exec(async () => {
      await acceptTask(r.task.id);
      await onRefresh(r.task.id);
    });
  const rollbackRow = (r: RowItem) =>
    exec(async () => {
      await rollbackTask(r.task.id);
      await onRefresh(r.task.id);
    });

  const openRowRoom = (r: RowItem) => openRoom(r.task.id);
  const peekRow = (r: RowItem) => {
    openRoom(r.task.id);
    setCanvasTab("changes");
  };

  const selRows = rows.filter((r) => selected.has(r.key));

  /** 批量 = 循环单命令（跳过不适用行）。 */
  const batch = (action: "grant" | "deny" | "accept") =>
    exec(async () => {
      for (const r of selRows) {
        if (action === "grant" && r.permission) {
          await permissionApprove(r.permission.id, "allow");
          await onRefresh(r.task.id);
        } else if (action === "deny" && r.permission) {
          await permissionApprove(r.permission.id, "deny");
          await onRefresh(r.task.id);
        } else if (action === "accept" && r.kind === "review") {
          await acceptTask(r.task.id);
          await onRefresh(r.task.id);
        }
      }
      setSelected(new Set());
    });

  const armRollback = (key: string) => {
    setRollbackArm(key);
    if (armTimer.current) clearTimeout(armTimer.current);
    armTimer.current = setTimeout(() => setRollbackArm(null), 3500);
  };

  const rollbackWithConfirm = (r: RowItem) => {
    if (rollbackArm !== r.key) {
      armRollback(r.key);
      return;
    }
    setRollbackArm(null);
    void rollbackRow(r);
  };

  const toggleKey = (key: string) =>
    setSelected((s) => {
      const next = new Set(s);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });

  // 局部 keydown：仅 rows 密度激活；无依赖数组，每次渲染换最新闭包
  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      if (isTypingTarget(e.target) || e.metaKey || e.ctrlKey || e.altKey) return;
      const current = rows[kb];
      switch (e.key) {
        case "j":
          setKb((i) => Math.min(rows.length - 1, i + 1));
          e.preventDefault();
          break;
        case "k":
          setKb((i) => Math.max(0, i - 1));
          e.preventDefault();
          break;
        case "x":
          if (current) toggleKey(current.key);
          break;
        case "Escape":
          setRollbackArm(null);
          setSelected((s) => (s.size > 0 ? new Set() : s));
          break;
        case "Enter":
          if (current) openRowRoom(current);
          break;
        case "e":
          setScene("editor");
          break;
        case "p":
          if (current && (current.kind === "review" || current.kind === "running")) peekRow(current);
          break;
        case "g":
          if (selected.size > 0) void batch("grant");
          else if (current?.kind === "permission") void grantRow(current);
          break;
        case "d":
          if (selected.size > 0) void batch("deny");
          else if (current?.kind === "permission") void denyRow(current);
          break;
        case "a":
          if (selected.size > 0) void batch("accept");
          else if (current?.kind === "review") void acceptRow(current);
          break;
        case "r":
          if (current?.kind === "review") rollbackWithConfirm(current);
          break;
        default:
          break;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  let flatIndex = -1;

  return (
    <section className="fleet-rows">
      <div className="rows-box">
        {sections.map((sec) => (
          <Fragment key={sec.title}>
            <div className={`rhead${sec.attn && sec.rows.length > 0 ? " attn" : ""}`}>
              <span>{sec.title}</span>
              <span className="line" />
              <span>{sec.rows.length}</span>
            </div>
            {sec.rows.map((r) => {
              flatIndex += 1;
              const i = flatIndex;
              return (
                <Row
                  key={r.key}
                  row={r}
                  detail={details[r.task.id]}
                  workspaces={workspaces}
                  kb={i === kb}
                  checked={selected.has(r.key)}
                  armed={rollbackArm === r.key}
                  onHover={() => setKb(i)}
                  onToggle={() => toggleKey(r.key)}
                  onOpen={() => openRowRoom(r)}
                />
              );
            })}
          </Fragment>
        ))}
        {rows.length === 0 && <div className="empty">还没有任务，回到新对话发起第一个任务。</div>}
        {selected.size > 0 && (
          <div className="rows-bulk">
            <span className="n">{selected.size} selected</span>
            <button
              className="bbtn primary"
              disabled={busy || !selRows.some((r) => r.kind === "review")}
              onClick={() => void batch("accept")}
            >
              Accept · a
            </button>
            <button
              className="bbtn"
              disabled={busy || !selRows.some((r) => r.kind === "permission")}
              onClick={() => void batch("grant")}
            >
              Grant once · g
            </button>
            <button
              className="bbtn"
              disabled={busy || !selRows.some((r) => r.kind === "permission")}
              onClick={() => void batch("deny")}
            >
              Deny · d
            </button>
            <span className="hint">J、K 键移动，X 键选择，Esc 清除选择</span>
          </div>
        )}
      </div>
    </section>
  );
}

// ---------- 行 ----------

interface RowProps {
  row: RowItem;
  detail: TaskDetail | undefined;
  workspaces: Workspace[];
  kb: boolean;
  checked: boolean;
  armed: boolean;
  onHover: () => void;
  onToggle: () => void;
  onOpen: () => void;
}

function Row({ row, detail, workspaces, kb, checked, armed, onHover, onToggle, onOpen }: RowProps) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (kb) ref.current?.scrollIntoView({ block: "nearest" });
  }, [kb]);

  const gates = gatesFor(row.task, detail);
  const lamp =
    row.kind === "settled"
      ? row.outcome === "rolled_back"
        ? "fail"
        : "done"
      : lampFor(row.task.state, row.kind === "permission" || row.kind === "review");

  return (
    <div
      ref={ref}
      className={`rrow${kb ? " kb" : ""}${checked ? " checked" : ""}${
        row.kind === "settled" ? " settled" : ""
      }`}
      onMouseEnter={onHover}
      onClick={onOpen}
      role="button"
      tabIndex={-1}
    >
      <span
        onClick={(e) => {
          e.stopPropagation();
          onToggle();
        }}
      >
        <span className="cbox">{checked ? "✓" : ""}</span>
      </span>
      <span className="r-title">
        <span className={`lamp${lamp ? ` ${lamp}` : ""}`} />
        <span className="t">{row.task.title || row.task.goal}</span>
      </span>
      <span className="r-proj">{projectName(workspaces, row.task.workspace_path)}</span>
      <span className={`r-branch${row.task.worktree_path ? " wt" : ""}`}>
        {row.task.worktree_path ? `⎇ ${baseName(row.task.worktree_path)}` : "main"}
      </span>
      <ActCell row={row} detail={detail} />
      <span className="gsegs">
        <GSeg s={gates.plan} />
        <GSeg s={gates.perms} />
        <GSeg s={gates.verify} />
      </span>
      <DiffCell detail={detail} />
      <span className="r-time">
        {elapsedSince(row.kind === "settled" && row.when ? row.when : row.task.created_at)}
      </span>
      <span className="r-keys">
        <RowKeys row={row} armed={armed} />
      </span>
    </div>
  );
}

/** GateState → gseg 类名（active → now，idle → 无）。 */
function GSeg({ s }: { s: GateState }) {
  const cls = s === "active" ? "now" : s === "idle" ? "" : s;
  return <span className={`gseg${cls ? ` ${cls}` : ""}`} />;
}

function ActCell({ row, detail }: { row: RowItem; detail: TaskDetail | undefined }) {
  if (row.kind === "permission" && row.permission) {
    return (
      <span className="r-act attn">
        <span className="v">perm</span> {row.permission.tool_name} ·{" "}
        {elapsedSince(row.permission.created_at)}
      </span>
    );
  }
  if (row.kind === "review") {
    const v = latestVerification(detail);
    const txt = !v
      ? "no checks yet"
      : v.status === "passed"
        ? "all checks passed"
        : v.status === "running"
          ? "verifying…"
          : v.status === "failed" || v.status === "timeout"
            ? "checks failed"
            : v.status;
    return <span className="r-act attn">review ready · {txt}</span>;
  }
  if (row.kind === "settled") {
    const label =
      row.outcome === "accepted"
        ? "accepted"
        : row.outcome === "answered"
          ? "answered"
          : "rolled back";
    return (
      <span className="r-act idle">
        {label}
        {row.when ? ` · ${clockTime(row.when)}` : ""}
      </span>
    );
  }
  const a = actionLineFor(row.task, detail);
  if (!a) return <span className="r-act idle">standing by</span>;
  return (
    <span className="r-act">
      <span className="v">{a.verb}</span> {a.target}
    </span>
  );
}

function DiffCell({ detail }: { detail: TaskDetail | undefined }) {
  const changes = detail?.changes ?? [];
  if (changes.length === 0) return <span className="r-diff none">—</span>;
  const count = (k: string) => changes.filter((c) => c.change_type === k).length;
  const title = `${changes.length} files: ${count("create")} new, ${count("modify")} mod, ${count(
    "rename",
  )} ren, ${count("delete")} del`;
  return (
    <span className="r-diff" title={title}>
      {changes.length} files
    </span>
  );
}

function RowKeys({ row, armed }: { row: RowItem; armed: boolean }) {
  switch (row.kind) {
    case "permission":
      return (
        <>
          <span className="kact hot">g grant</span>
          <span className="kact">d deny</span>
        </>
      );
    case "review":
      return armed ? (
        <span className="kact hot">r confirm?</span>
      ) : (
        <>
          <span className="kact hot">a accept</span>
          <span className="kact">p peek</span>
          <span className="kact">r roll</span>
        </>
      );
    default:
      return <span className="kact">⏎ open</span>;
  }
}
