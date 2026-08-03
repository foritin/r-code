/**
 * 待批权限门 —— 运行中被权限阻塞的 tool call 在这里批复。
 * 数据源:task_detail 轮询(store.tasks);批复后刷新 detail。
 */
import { useMemo, useState } from "react";
import { permissionApprove } from "../../lib/ipc";
import { permissionAttribution, permissionRiskLabel } from "../../lib/format";
import { useTasksStore } from "../../store/tasks";
import type { AgentRun, PermissionDecision, PermissionRequest } from "../../lib/types";

const EMPTY_RUNS: AgentRun[] = [];

export function PendingPermissions({ taskId }: { taskId: string }) {
  const permissions = useTasksStore((s) => s.details[taskId]?.permissions);
  const runs = useTasksStore((s) => s.details[taskId]?.runs);
  const pending = useMemo(
    () => permissions?.filter((permission) => permission.decision === "pending") ?? [],
    [permissions],
  );
  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const [error, setError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

  if (pending.length === 0 && !error) return null;

  const decide = async (id: string, decision: Exclude<PermissionDecision, "pending">) => {
    setBusyId(id);
    setError(null);
    try {
      await permissionApprove(id, decision);
      await refreshDetail(taskId);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyId(null);
    }
  };

  return (
    <div className="perm-stack">
      {pending.map((p) => (
        <PermissionCard
          key={p.id}
          permission={p}
          runs={runs ?? EMPTY_RUNS}
          busy={busyId === p.id}
          onDecide={decide}
        />
      ))}
      {error && <div className="perm-error">批复失败:{error}</div>}
    </div>
  );
}

function PermissionCard({
  permission,
  runs,
  busy,
  onDecide,
}: {
  permission: PermissionRequest;
  runs: AgentRun[];
  busy: boolean;
  onDecide: (id: string, decision: Exclude<PermissionDecision, "pending">) => Promise<void>;
}) {
  const attribution = permissionAttribution(permission, runs);
  return (
    <div className="perm-card">
      <div className="perm-head">
        <span className="chip risk" title={permissionRiskLabel(permission.risk_level)}>
          {permission.risk_level} · {permissionRiskLabel(permission.risk_level)}
        </span>
        <span className="perm-tool">{permission.tool_name}</span>
        <span className={"perm-owner owner-" + attribution.kind}>{attribution.label}</span>
        <span className="perm-hint">等待批准</span>
      </div>
      <div className="perm-summary" title={permission.input_summary}>
        {permission.input_summary}
      </div>
      <div className="perm-actions">
        <button className="btn accent sm" disabled={busy} onClick={() => void onDecide(permission.id, "allow")}>
          允许一次
        </button>
        <button className="btn sm" disabled={busy} onClick={() => void onDecide(permission.id, "allow_always")}>
          总是允许
        </button>
        <button className="btn danger sm" disabled={busy} onClick={() => void onDecide(permission.id, "deny")}>
          拒绝
        </button>
      </div>
    </div>
  );
}
