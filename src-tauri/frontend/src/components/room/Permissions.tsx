/**
 * 待批权限门 —— 运行中被权限阻塞的 tool call 在这里批复。
 * 数据源:task_detail 轮询(store.tasks);批复后刷新 detail。
 */
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { permissionApprove } from "../../lib/ipc";
import {
  canPersistPermissionGrant,
  permissionAttribution,
  permissionRiskLabel,
} from "../../lib/format";
import { useTasksStore } from "../../store/tasks";
import type { AgentRun, PermissionDecision, PermissionRequest } from "../../lib/types";

const EMPTY_RUNS: AgentRun[] = [];

export function PendingPermissions({ taskId }: { taskId: string }) {
  const { t } = useTranslation();
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
      <p className="sr-only" aria-live="assertive" aria-atomic="true">
        {pending[0]
          ? t(canPersistPermissionGrant(pending[0].risk_level)
            ? "approvals.waitingAnnouncementWithActions"
            : "approvals.waitingAnnouncementWithSingleUseActions", {
              risk: permissionRiskLabel(pending[0].risk_level),
              tool: pending[0].tool_name,
            })
          : ""}
      </p>
      {pending.map((p) => (
        <PermissionCard
          key={p.id}
          permission={p}
          runs={runs ?? EMPTY_RUNS}
          busy={busyId === p.id}
          onDecide={decide}
        />
      ))}
      {error && <div className="perm-error" role="alert">{t("approvals.error", { error })}</div>}
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
  const { t } = useTranslation();
  const attribution = permissionAttribution(permission, runs);
  const headingId = `permission-${permission.id}-heading`;
  const scopeId = `permission-${permission.id}-scope`;
  const target = permission.target?.trim()
    || permission.input_summary.trim()
    || t("approvals.currentAction");
  const canPersist = canPersistPermissionGrant(permission.risk_level);
  return (
    <section className="perm-card" role="region" aria-labelledby={headingId} aria-describedby={scopeId}>
      <div className="perm-head">
        <span className="chip risk" title={permissionRiskLabel(permission.risk_level)}>
          {permission.risk_level} · {permissionRiskLabel(permission.risk_level)}
        </span>
        <span className="perm-tool" id={headingId}>{permission.tool_name}</span>
        <span className={"perm-owner owner-" + attribution.kind}>{attribution.label}</span>
        <span className="perm-hint">{t("approvals.waitingLabel")}</span>
      </div>
      <div className="perm-summary" title={permission.input_summary}>
        {permission.input_summary}
      </div>
      <div className="perm-scope" id={scopeId}>
        {canPersist
          ? t("approvals.persistentScope", {
              tool: permission.tool_name,
              target,
              risk: permission.risk_level,
            })
          : t("approvals.singleUseScope", { risk: permission.risk_level })}
      </div>
      <div className="perm-actions">
        <button type="button" className="btn accent sm" disabled={busy} onClick={() => void onDecide(permission.id, "allow")}>
          {t("approvals.allowOnce")}
        </button>
        {canPersist && (
          <button type="button" className="btn sm" disabled={busy} onClick={() => void onDecide(permission.id, "allow_always")}>
            {t("approvals.allowAlways")}
          </button>
        )}
        <button type="button" className="btn danger sm" disabled={busy} onClick={() => void onDecide(permission.id, "deny")}>
          {t("approvals.deny")}
        </button>
      </div>
    </section>
  );
}
