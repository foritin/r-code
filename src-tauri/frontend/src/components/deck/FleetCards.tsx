/**
 * Fleet Cards —— 3 列栅格，每活跃（running）任务一卡。
 * 结构照 fusion-obsidian.html:303-337, 793-815。
 */
import { useAppStore } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { elapsedSince } from "../../lib/format";
import {
  actionLineFor,
  baseName,
  gatesFor,
  projectName,
  type GateState,
} from "../../lib/deck";
import type { Task } from "../../lib/types";
import { VerifyRow } from "./shared";

export function FleetCards({ tasks }: { tasks: Task[] }) {
  return (
    <section className="fleet-cards">
      {tasks.length === 0 && (
        <div className="empty fleet-empty">
          舰队空闲 — 没有进行中的任务。
          <br />
          回 Home 发起新任务。
        </div>
      )}
      {tasks.map((t) => (
        <FleetCard key={t.id} task={t} />
      ))}
    </section>
  );
}

function FleetCard({ task }: { task: Task }) {
  const detail = useTasksStore((s) => s.details[task.id]);
  const workspaces = useTasksStore((s) => s.workspaces);
  const openRoom = useAppStore((s) => s.openRoom);
  const gates = gatesFor(task, detail);
  const action = actionLineFor(task, detail);
  const activeSubagents = detail?.runs.filter(
    (run) => run.agent_kind === "subagent" && run.ended_at == null
  ).length ?? 0;

  return (
    <article className="fcard" onClick={() => openRoom(task.id)} title="打开房间">
      <div className="top">
        <span className="chip">{projectName(workspaces, task.workspace_path)}</span>
        {task.worktree_path && <span className="chip wt">⎇ {baseName(task.worktree_path)}</span>}
        {activeSubagents > 0 && <span className="chip">子代理 · {activeSubagents}</span>}
        <span className="elapsed">{elapsedSince(task.created_at)}</span>
      </div>
      <h3>{task.title || task.goal}</h3>
      <div className="action-line">
        {action ? (
          <>
            <span className="verb">{action.verb}</span>
            <span className="t">{action.target}</span>
          </>
        ) : (
          <span className="verb">standing by</span>
        )}
        <span className="caret" />
      </div>
      <div className="gates">
        <Gate label="Plan" state={gates.plan} />
        <span className="gate-link" />
        <Gate
          label={`Perms${gates.permCount > 0 ? ` · ${gates.permCount}` : ""}`}
          state={gates.perms}
        />
        <span className="gate-link" />
        <Gate label="Verify" state={gates.verify} />
      </div>
      <VerifyRow detail={detail} />
    </article>
  );
}

function Gate({ label, state }: { label: string; state: GateState }) {
  return (
    <span className={`gate${state !== "idle" ? ` ${state}` : ""}`}>
      <i />
      {label}
    </span>
  );
}
