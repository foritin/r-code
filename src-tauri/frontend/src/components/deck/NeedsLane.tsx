/**
 * Needs You 通道 —— 权限待批卡 + review-ready 卡（整卡 needsPulse 呼吸）。
 * 结构照 fusion-obsidian.html:272-301, 750-782。
 * cards 密度下 g/d 裁决最早等待的权限（键帽标注在首张权限卡按钮上）。
 */
import { useEffect, useRef, useState } from "react";
import { permissionApprove, rollbackTask } from "../../lib/ipc";
import { elapsedSince, permissionAttribution, permissionRiskLabel } from "../../lib/format";
import { isTypingTarget } from "../../lib/keys";
import { useAppStore } from "../../store/app";
import { useTasksStore, type NeedsYouItem } from "../../store/tasks";
import { baseName, latestVerification, projectName } from "../../lib/deck";
import type { Task, Workspace } from "../../lib/types";
import { DiffStat } from "./shared";

interface NeedsLaneProps {
  items: NeedsYouItem[];
  onRefresh: (taskId: string) => Promise<void>;
  onError: (message: string) => void;
}

export function NeedsLane({ items, onRefresh, onError }: NeedsLaneProps) {
  const density = useAppStore((s) => s.deckDensity);
  const [busy, setBusy] = useState(false);

  const decide = async (item: NeedsYouItem, decision: "allow" | "deny") => {
    if (!item.permission || busy) return;
    setBusy(true);
    try {
      await permissionApprove(item.permission.id, decision);
      await onRefresh(item.task.id);
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  // cards 密度：g/d 裁决最早等待的那条权限
  const firstPerm = items.find((i) => i.kind === "permission");
  useEffect(() => {
    if (density !== "cards" || !firstPerm) return;
    const onKey = (e: KeyboardEvent) => {
      if (isTypingTarget(e.target) || e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.key === "g") {
        e.preventDefault();
        void decide(firstPerm, "allow");
      } else if (e.key === "d") {
        e.preventDefault();
        void decide(firstPerm, "deny");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  if (items.length === 0) return null;

  return (
    <>
      <div className="zone-head nh-needs">
        <span>Needs you</span>
        <span className="n">{items.length}</span>
      </div>
      <section className="needs-lane">
        {items.map((item) =>
          item.kind === "permission" && item.permission ? (
            <PermissionCard
              key={item.permission.id}
              item={item}
              busy={busy}
              hot={firstPerm === item}
              onDecide={(d) => void decide(item, d)}
            />
          ) : (
            <ReviewCard
              key={`rev-${item.task.id}`}
              item={item}
              onRefresh={onRefresh}
              onError={onError}
            />
          ),
        )}
      </section>
    </>
  );
}

/** 项目 · worktree 行（ncard 的 .proj）。 */
function projLine(task: Task, workspaces: Workspace[]): string {
  const base = projectName(workspaces, task.workspace_path);
  return task.worktree_path ? `${base} · ⎇ ${baseName(task.worktree_path)}` : base;
}

function PermissionCard({
  item,
  busy,
  hot,
  onDecide,
}: {
  item: NeedsYouItem;
  busy: boolean;
  hot: boolean;
  onDecide: (decision: "allow" | "deny") => void;
}) {
  const workspaces = useTasksStore((s) => s.workspaces);
  const runs = useTasksStore((s) => s.details[item.task.id]?.runs ?? []);
  const p = item.permission;
  if (!p) return null;
  const attribution = permissionAttribution(p, runs);
  return (
    <article className="ncard">
      <div className="tag">
        <i />
        权限待批 · {p.risk_level}（{permissionRiskLabel(p.risk_level)}）· 等待 {elapsedSince(item.since)}
      </div>
      <h3>{item.task.title || item.task.goal}</h3>
      <div className="proj">{projLine(item.task, workspaces)}</div>
      <div className="body">
        <span className="ntool">{p.tool_name}</span>
        <span className={`perm-owner owner-${attribution.kind}`}>{attribution.label}</span>
        <span className="nsummary">{p.input_summary || "（无摘要）"}</span>
      </div>
      <div className="acts">
        <button className="btn primary" disabled={busy} onClick={() => onDecide("allow")}>
          Grant once {hot && <span className="bkey">g</span>}
        </button>
        <button className="btn" disabled={busy} onClick={() => onDecide("deny")}>
          Deny {hot && <span className="bkey">d</span>}
        </button>
      </div>
    </article>
  );
}

function ReviewCard({
  item,
  onRefresh,
  onError,
}: {
  item: NeedsYouItem;
  onRefresh: (taskId: string) => Promise<void>;
  onError: (message: string) => void;
}) {
  const openRoom = useAppStore((s) => s.openRoom);
  const workspaces = useTasksStore((s) => s.workspaces);
  const detail = useTasksStore((s) => s.details[item.task.id]);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    [],
  );

  const arm = () => {
    setConfirming(true);
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => setConfirming(false), 3500);
  };

  const rollback = async () => {
    if (busy) return;
    setBusy(true);
    try {
      await rollbackTask(item.task.id);
      await onRefresh(item.task.id);
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
      setConfirming(false);
    }
  };

  const verifs = [...(detail?.verifications ?? [])]
    .sort((a, b) => b.started_at.localeCompare(a.started_at))
    .slice(0, 3);
  const latest = latestVerification(detail);
  const tagTail = !latest
    ? "no checks yet"
    : latest.status === "passed"
      ? "all checks passed"
      : latest.status === "running"
        ? "verifying…"
        : latest.status === "failed" || latest.status === "timeout"
          ? "checks failed"
          : latest.status;

  return (
    <article className="ncard">
      <div className="tag">
        <i />
        Review ready · {tagTail}
      </div>
      <h3>{item.task.title || item.task.goal}</h3>
      <div className="proj">{projLine(item.task, workspaces)}</div>
      <DiffStat changes={detail?.changes ?? []} />
      <div className="checkrow">
        {verifs.length === 0 && <span>尚未运行验证</span>}
        {verifs.map((v) => (
          <span
            key={v.id}
            className={
              v.status === "passed"
                ? "ok"
                : v.status === "running"
                  ? "run"
                  : v.status === "failed" || v.status === "timeout"
                    ? "bad"
                    : ""
            }
          >
            {v.command}
          </span>
        ))}
      </div>
      <div className="acts">
        <button
          className="btn primary"
          onClick={() => {
            openRoom(item.task.id, "review");
          }}
        >
          Open review
        </button>
        <button
          className="btn"
          onClick={() => {
            openRoom(item.task.id, "changes");
          }}
        >
          Peek changes
        </button>
        {confirming ? (
          <button className="btn danger" disabled={busy} onClick={() => void rollback()}>
            确认回滚？
          </button>
        ) : (
          <button className="btn ghost" onClick={arm}>
            Rollback
          </button>
        )}
      </div>
    </article>
  );
}
