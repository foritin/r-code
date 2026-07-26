import { useState } from "react";
import { useAppStore } from "../../store/app";
import {
  useTasksStore,
  selectNeedsYou,
  type NeedsYouItem,
} from "../../store/tasks";
import { usePoll } from "../../lib/poll";
import { permissionApprove, rollbackTask } from "../../lib/ipc";
import type { PermissionDecision } from "../../lib/types";
import { elapsedSince, permissionAttribution, permissionRiskLabel } from "../../lib/format";

/**
 * Inbox — 跨项目 Needs-you 聚合（selectNeedsYou 派生）。
 * 权限待批卡：Grant once / Deny 就地裁决；review-ready 卡：Open review / Peek changes / Rollback。
 * 操作全部就地完成，随后刷新派生数据；2s 轮询。
 */
export function InboxScene() {
  const items = useTasksStore(selectNeedsYou);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const refreshDetails = useTasksStore((s) => s.refreshDetails);
  const workspaces = useTasksStore((s) => s.workspaces);

  const [err, setErr] = useState<string | null>(null);

  usePoll(async () => {
    await refreshTasks();
    const active = useTasksStore
      .getState()
      .tasks.filter((t) => t.state !== "idle" && t.state !== "archived")
      .map((t) => t.id);
    await refreshDetails(active);
  }, 2000);

  const workspaceName = (path: string | null) => {
    if (!path) return "聊天";
    return workspaces.find((w) => w.canonical_path === path)?.display_name ?? path.split(/[\\/]/).pop() ?? path;
  };

  return (
    <div className="scene">
      <div className="scene-scroll">
        <div className="page-head">
          <h1>Inbox</h1>
          <span className="meta">NEEDS YOU · {items.length}</span>
        </div>

        {err && <div className="errbar">{err}</div>}

        {items.length === 0 ? (
          <div className="empty">
            没有待决事项。
            <br />
            所有任务都在正常运转，去喝杯茶吧。
          </div>
        ) : (
          <div className="needs-list">
            {items.map((item) =>
              item.kind === "permission" ? (
                <PermissionCard
                  key={item.permission!.id}
                  item={item}
                  projectName={workspaceName(item.task.workspace_path)}
                  onError={setErr}
                />
              ) : (
                <ReviewCard
                  key={item.task.id}
                  item={item}
                  projectName={workspaceName(item.task.workspace_path)}
                  onError={setErr}
                />
              ),
            )}
          </div>
        )}
      </div>
    </div>
  );
}

/** 权限待批卡：tool / summary / 等待时长 + 就地裁决。 */
function PermissionCard({
  item,
  projectName,
  onError,
}: {
  item: NeedsYouItem;
  projectName: string;
  onError: (e: string | null) => void;
}) {
  const openRoom = useAppStore((s) => s.openRoom);
  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const runs = useTasksStore((s) => s.details[item.task.id]?.runs ?? []);
  const [busy, setBusy] = useState(false);
  const p = item.permission!;
  const riskHi = p.risk_level === "R3" || p.risk_level === "R4";
  const attribution = permissionAttribution(p, runs);

  const decide = async (decision: Exclude<PermissionDecision, "pending">) => {
    setBusy(true);
    onError(null);
    try {
      await permissionApprove(p.id, decision);
      await refreshDetail(item.task.id);
    } catch (e) {
      onError(`权限裁决失败：${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <article className="ncard perm">
      <div className="tag">
        <span className="lamp attn" />
        权限待批 · 等待 {elapsedSince(item.since)}
        <span className={`chip risk${riskHi ? " hi" : ""}`} title={permissionRiskLabel(p.risk_level)}>
          {p.risk_level} · {permissionRiskLabel(p.risk_level)}
        </span>
      </div>
      <h3>{item.task.title || item.task.goal.slice(0, 60)}</h3>
      <div className="proj">
        {projectName} · <span className="chip">{p.tool_name}</span> ·{" "}
        <span className={`perm-owner owner-${attribution.kind}`}>{attribution.label}</span>
      </div>
      {p.input_summary && <div className="body">{p.input_summary}</div>}
      <div className="acts">
        <button className="btn primary" disabled={busy} onClick={() => void decide("allow")}>
          Grant once
        </button>
        <button className="btn" disabled={busy} onClick={() => void decide("deny")}>
          Deny
        </button>
        <button className="btn ghost" disabled={busy} onClick={() => openRoom(item.task.id)}>
          查看任务
        </button>
      </div>
    </article>
  );
}

/** Review-ready 卡：diffstat + Open review / Peek changes / Rollback。 */
function ReviewCard({
  item,
  projectName,
  onError,
}: {
  item: NeedsYouItem;
  projectName: string;
  onError: (e: string | null) => void;
}) {
  const openRoom = useAppStore((s) => s.openRoom);
  const setCanvasTab = useAppStore((s) => s.setCanvasTab);
  const changes = useTasksStore((s) => s.details[item.task.id]?.changes);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const [busy, setBusy] = useState(false);

  const open = (tab: "review" | "changes") => {
    setCanvasTab(tab);
    openRoom(item.task.id);
  };

  const rollback = async () => {
    setBusy(true);
    onError(null);
    try {
      await rollbackTask(item.task.id);
      await refreshTasks();
    } catch (e) {
      onError(`回滚失败：${String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const stat = { create: 0, modify: 0, delete: 0, rename: 0 };
  for (const c of changes ?? []) stat[c.change_type] += 1;
  const files = changes?.length ?? 0;

  return (
    <article className="ncard review">
      <div className="tag">
        <span className="lamp attn" />
        Review ready · {elapsedSince(item.since)}
      </div>
      <h3>{item.task.title || item.task.goal.slice(0, 60)}</h3>
      <div className="proj">{projectName}</div>
      <div className="diffstat">
        {stat.create > 0 && <span className="add">+{stat.create}</span>}{" "}
        {stat.delete > 0 && <span className="del">−{stat.delete}</span>}{" "}
        {stat.modify + stat.rename > 0 && <span className="dim">~{stat.modify + stat.rename}</span>}{" "}
        <span className="dim">· {files} 个文件</span>
      </div>
      <div className="acts">
        <button className="btn primary" disabled={busy} onClick={() => open("review")}>
          Open review
        </button>
        <button className="btn" disabled={busy} onClick={() => open("changes")}>
          Peek changes
        </button>
        <button className="btn ghost" disabled={busy} onClick={() => void rollback()}>
          Rollback
        </button>
      </div>
    </article>
  );
}
