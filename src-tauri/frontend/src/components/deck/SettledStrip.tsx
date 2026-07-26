/**
 * Settled strip —— 已 accept / answered / rolled back 的任务（最近 10 条）。
 * 结构照 fusion-obsidian.html:339-352, 817-828。
 */
import { useAppStore } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { clockTime } from "../../lib/format";
import { projectName, type SettledItem } from "../../lib/deck";
import { DiffStat } from "./shared";

const OUTCOME_LABEL: Record<SettledItem["outcome"], string> = {
  accepted: "accepted",
  answered: "answered",
  rolled_back: "rolled back",
  aborted: "aborted",
};

export function SettledStrip({ items }: { items: SettledItem[] }) {
  const openRoom = useAppStore((s) => s.openRoom);
  const workspaces = useTasksStore((s) => s.workspaces);
  if (items.length === 0) return null;

  return (
    <div className="settled-wrap">
      <div className="zone-head">
        <span>Settled</span>
        <span className="n">{items.length}</span>
      </div>
      <section className="settled-strip">
        {items.map((s) => (
          <button key={s.task.id} className="settled-item" onClick={() => openRoom(s.task.id)}>
            <span
              className={`s-lamp${
                s.outcome === "accepted" ? " ok" : s.outcome === "rolled_back" || s.outcome === "aborted" ? " bad" : ""
              }`}
            />
            <span className="s-copy">
              <b>{s.task.title || s.task.goal}</b>
              <span>
                {projectName(workspaces, s.task.workspace_path)} · {OUTCOME_LABEL[s.outcome]}{" "}
                {clockTime(s.when)}
              </span>
            </span>
            {s.outcome === "rolled_back" || s.outcome === "aborted" ? (
              <span className="s-diff none">{s.outcome === "aborted" ? "stopped" : "discarded"}</span>
            ) : (
              <DiffStat changes={s.changes} className="s-diff" />
            )}
          </button>
        ))}
      </section>
    </div>
  );
}
