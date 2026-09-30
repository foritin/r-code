/**
 * 待批权限门 —— 运行中被权限阻塞的 tool call 在这里批复。
 * 数据源:task_detail 轮询(store.tasks);批复后刷新 detail。
 *
 * P19B-C:本文件是**桌面效果授权的唯一渲染器**——RoomScene 侧的
 * `PendingPermissions` 与 Canvas 工作台侧都调用同一份 `EffectApprovalPanel`
 * 组件和同一组纯呈现函数(`effectAuthorityRows` / `effectErrorCode` /
 * `effectGrantUi`),两份 JSX 不重复实现同一份呈现。纯函数不依赖 React 与
 * DOM,远端投影与 TUI 按同一语义对齐。
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  harnessV1ApprovalDecide,
  harnessV1EffectList,
  harnessV1EffectRequest,
  harnessV1EffectRevoke,
  permissionApprove,
  type EffectApprovalMaterial,
  type EffectApprovalPendingRow,
  type EffectApprovalView,
} from "../../lib/ipc";
import {
  canPersistPermissionGrant,
  permissionAttribution,
  permissionRiskLabel,
} from "../../lib/format";
import { useTasksStore } from "../../store/tasks";
import type { AgentRun, PermissionDecision, PermissionRequest } from "../../lib/types";

const EMPTY_RUNS: AgentRun[] = [];

// ---------------------------------------------------------------------------
// P19B-C 效果授权：共享呈现模型（纯函数,桌面两侧与远端/TUI 共用语义）
// ---------------------------------------------------------------------------

/**
 * 权威材料的六个键,顺序即 `EffectBinding::material` 冻结的键序。桌面、
 * 远端、TUI 三端逐字一致——顺序只在这里排一次。
 */
export const EFFECT_MATERIAL_FIELDS: ReadonlyArray<keyof EffectApprovalMaterial> = [
  "taskId",
  "planRevision",
  "workUnitId",
  "effectClass",
  "network",
  "payloadHash",
];

export interface AuthorityRow {
  key: string;
  value: string;
}

function materialRows(material: EffectApprovalMaterial): AuthorityRow[] {
  return EFFECT_MATERIAL_FIELDS.map((key) => ({ key, value: material[key] ?? "" }));
}

/** 已落库授权的展示行 = 六列材料 + 审批身份/归属/作用域。 */
export function effectAuthorityRows(approval: EffectApprovalView): AuthorityRow[] {
  return [
    ...materialRows(approval),
    { key: "approvalId", value: approval.approvalId },
    { key: "actorId", value: approval.actorId },
    { key: "sessionId", value: approval.sessionId },
    { key: "scope", value: approval.scope },
  ];
}

/** 待决请求的展示行 = 六列材料 + 待决操作号。 */
export function effectRequestRows(pending: EffectApprovalPendingRow): AuthorityRow[] {
  return [...materialRows(pending), { key: "operationId", value: pending.operationId }];
}

/**
 * daemon 错误码前缀 → 语义化判定。三端必须能区分这些码,所以判定集中在此,
 * 各端不自行 `includes` 字符串。
 */
const EFFECT_ERROR_PREFIXES = [
  "plan_not_approved",
  "effect_approval_stale",
  "effect_approval_active_exists",
  "effect_approval_conflict",
  "approval_unknown",
  "approval_conflict",
  "needs_desktop_confirm",
] as const;

export function effectErrorCode(error: string): string | null {
  return EFFECT_ERROR_PREFIXES.find((prefix) => error.includes(prefix)) ?? null;
}

export interface EffectGrantUi {
  canControl: boolean;
  canRevoke: boolean;
  readOnlyBadge: string | null;
  stateLabel: "active" | "superseded" | "pending";
}

/**
 * 能力 → 控件投影。能力缺失时控件**隐藏**（不是禁用）并给出只读徽标,
 * 失败关闭:一个写请求都不发。
 */
export function effectGrantUi(readOnly: boolean, hasActive: boolean): EffectGrantUi {
  return {
    canControl: !readOnly,
    canRevoke: !readOnly && hasActive,
    readOnlyBadge: readOnly ? "approvals.effectReadOnly" : null,
    stateLabel: hasActive ? "active" : "pending",
  };
}

// ---------------------------------------------------------------------------
// P19B-C 共享渲染器（Permissions 与 Canvas 共用同一份 JSX）
// ---------------------------------------------------------------------------

/** 一个 WorkUnit 在面板里的完整呈现状态。 */
interface WorkUnitView {
  workUnitId: string;
  material: EffectApprovalMaterial;
  active: EffectApprovalView | null;
  superseded: EffectApprovalView[];
  pending: EffectApprovalPendingRow[];
}

export interface EffectApprovalPanelProps {
  taskId: string;
  /** 能力缺失（远端/只读）时整块只读：控件隐藏,不发出任何写请求。 */
  readOnly?: boolean;
  /** 紧凑变体（工作台内嵌），与全宽变体共用同一份行内容。 */
  compact?: boolean;
  /** 变更后由宿主刷新（Canvas 走 task_detail，RoomScene 走 store）。 */
  onChanged?: () => void | Promise<void>;
}

/**
 * 一个任务的效果授权面板：请求 / 清单 / 撤销 / 决策。桌面两处渲染的唯一
 * 实现——请求与决策都走 typed IPC，呈现的六列材料逐字来自 daemon。
 */
export function EffectApprovalPanel({
  taskId,
  readOnly = false,
  compact = false,
  onChanged,
}: EffectApprovalPanelProps) {
  const { t } = useTranslation();
  const [approvals, setApprovals] = useState<EffectApprovalView[]>([]);
  const [pending, setPending] = useState<EffectApprovalPendingRow[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const answer = await harnessV1EffectList(taskId);
      setApprovals(answer.approvals ?? []);
      setPending(answer.pending ?? []);
    } catch (failure) {
      setError(String(failure));
    }
  }, [taskId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const run = useCallback(
    async (id: string, action: () => Promise<string | null>) => {
      setBusy(id);
      setError(null);
      setNote(null);
      try {
        setNote(await action());
        await refresh();
        await onChanged?.();
      } catch (failure) {
        setError(String(failure));
      } finally {
        setBusy(null);
      }
    },
    [refresh, onChanged],
  );

  const request = (workUnitId: string) =>
    run(`request:${workUnitId}`, async () => {
      const answer = await harnessV1EffectRequest(taskId, workUnitId);
      return answer.status === "granted" && answer.approval
        ? t("approvals.effectOutcome", { approval: answer.approval.approvalId })
        : answer.operationId ?? null;
    });

  const decide = (operationId: string, decision: "granted" | "denied") =>
    run(`decide:${operationId}`, async () =>
      decision === "granted"
        ? t("approvals.effectOutcome", { approval: operationId })
        : null);

  const revoke = (workUnitId: string) =>
    run(`revoke:${workUnitId}`, async () => {
      const answer = await harnessV1EffectRevoke(taskId, workUnitId);
      return t("approvals.effectRevokeOutcome", {
        revoked: String(answer.revoked),
        approval: answer.approvalId ?? "",
      });
    });

  const workUnits = useMemo<WorkUnitView[]>(() => {
    const byId = new Map<string, WorkUnitView>();
    const entry = (workUnitId: string) => {
      let view = byId.get(workUnitId);
      if (!view) {
        view = { workUnitId, material: null as unknown as EffectApprovalMaterial, active: null, superseded: [], pending: [] };
        byId.set(workUnitId, view);
      }
      return view;
    };
    for (const row of pending) {
      const view = entry(row.workUnitId);
      view.material = row;
      view.pending.push(row);
    }
    for (const row of approvals) {
      const view = entry(row.workUnitId);
      view.material = row;
      if (row.state === "active") view.active = row;
      else view.superseded.push(row);
    }
    return [...byId.values()];
  }, [pending, approvals]);

  if (!taskId || workUnits.length === 0) return null;
  const headingId = `effect-authority-${taskId}`;

  return (
    <section
      className={compact ? "sum-perm" : "perm-card opt-card"}
      role="region"
      aria-labelledby={headingId}
    >
      <div className="perm-head">
        <span className="chip opt-pill" id={headingId}>{t("approvals.effectHeading")}</span>
        <ReadOnlyBadge readOnly={readOnly} />
      </div>
      <div className="perm-scope opt-help">{t("approvals.effectDecisionHint")}</div>
      <div className="perm-scope opt-help">{t("approvals.effectRevokeWarning")}</div>
      <div className="perm-scope opt-help">{t("approvals.effectReauthorizeHint")}</div>
      {workUnits.map((view) => (
        <WorkUnitApproval
          key={view.workUnitId}
          view={view}
          readOnly={readOnly}
          busy={busy}
          onRequest={request}
          onDecide={decide}
          onRevoke={revoke}
        />
      ))}
      <PanelFooter note={note} error={error} />
    </section>
  );
}

/** 面板尾部注记与错误。两者都可空，空时整块不渲染。 */
function PanelFooter({ note, error }: { note: string | null; error: string | null }) {
  if (!note && !error) return null;
  return (
    <>
      <PanelNote note={note} />
      <PanelError error={error} />
    </>
  );
}

function PanelNote({ note }: { note: string | null }) {
  if (!note) return null;
  return <div className="perm-scope opt-help">{note}</div>;
}

function PanelError({ error }: { error: string | null }) {
  const { t } = useTranslation();
  if (!error) return null;
  return (
    <div className="perm-error" role="alert">
      {t("approvals.error", { error: `${effectErrorCode(error) ?? ""} ${error}` })}
    </div>
  );
}

/** 能力缺失时的只读徽标（没有它就什么都不渲染）。 */
function ReadOnlyBadge({ readOnly }: { readOnly: boolean }) {
  const { t } = useTranslation();
  const ui = effectGrantUi(readOnly, false);
  if (!ui.readOnlyBadge) return null;
  return <span className="perm-owner">{t(ui.readOnlyBadge)}</span>;
}

function WorkUnitApproval({
  view,
  readOnly,
  busy,
  onRequest,
  onDecide,
  onRevoke,
}: {
  view: WorkUnitView;
  readOnly: boolean;
  busy: string | null;
  onRequest: (workUnitId: string) => void;
  onDecide: (operationId: string, decision: "granted" | "denied") => void;
  onRevoke: (workUnitId: string) => void;
}) {
  const { t } = useTranslation();
  const ui = effectGrantUi(readOnly, view.active !== null);
  const rows = view.active
    ? effectAuthorityRows(view.active)
    : view.pending.length > 0
      ? effectRequestRows(view.pending[0])
      : effectAuthorityRows(view.superseded[0]);
  return (
    <div className="perm-card">
      <div className="perm-head">
        <span className="perm-tool">{view.workUnitId}</span>
        <span className="perm-hint opt-inline-state approval">{t(`approvals.effect${capitalize(ui.stateLabel)}`)}</span>
      </div>
      <dl className="perm-scope opt-mono">
        {rows.map((row) => (
          <div key={row.key}>
            <dt>{row.key}</dt>
            <dd>{row.value}</dd>
          </div>
        ))}
      </dl>
      <SupersededRows rows={view.superseded} />
      <WorkUnitActions
        view={view}
        ui={ui}
        busy={busy}
        onRequest={onRequest}
        onDecide={onDecide}
        onRevoke={onRevoke}
      />
    </div>
  );
}

/** 已撤销的历史授权（审计用，id 终身不复用）。 */
function SupersededRows({ rows }: { rows: EffectApprovalView[] }) {
  return (
    <>
      {rows.map((row) => (
        <SupersededRow key={row.approvalId} approval={row} />
      ))}
    </>
  );
}

function SupersededRow({ approval }: { approval: EffectApprovalView }) {
  const { t } = useTranslation();
  return (
    <div className="perm-scope opt-help">
      {t("approvals.effectSuperseded")} · {approval.approvalId} · {approval.supersededAtMs ?? ""}
      {/* 撤销语义是永久的（仅未来运行），不是撤销瞬间才成立的临时提示。 */}
      <div>{t("approvals.effectRevokeWarning")}</div>
      <div>{t("approvals.effectReauthorizeHint")}</div>
    </div>
  );
}

/** 一个 WorkUnit 的写控件。能力缺失时全部隐藏（不是禁用）——失败关闭。 */
function WorkUnitActions({
  view,
  ui,
  busy,
  onRequest,
  onDecide,
  onRevoke,
}: {
  view: WorkUnitView;
  ui: EffectGrantUi;
  busy: string | null;
  onRequest: (workUnitId: string) => void;
  onDecide: (operationId: string, decision: "granted" | "denied") => void;
  onRevoke: (workUnitId: string) => void;
}) {
  const { t } = useTranslation();
  if (view.active) {
    if (!ui.canRevoke) return null;
    return (
      <div className="perm-actions opt-actions">
        <RevokeButton workUnitId={view.workUnitId} busy={busy} onRevoke={onRevoke} />
      </div>
    );
  }
  if (!ui.canControl) return null;
  return (
    <div className="perm-actions opt-actions">
      <RequestButton workUnitId={view.workUnitId} busy={busy} onRequest={onRequest} />
      {view.pending.map((row) => (
        <DecideButtons
          key={row.operationId}
          operationId={row.operationId}
          busy={busy}
          onDecide={onDecide}
        />
      ))}
    </div>
  );
}

function RevokeButton({
  workUnitId,
  busy,
  onRevoke,
}: {
  workUnitId: string;
  busy: string | null;
  onRevoke: (workUnitId: string) => void;
}) {
  const { t } = useTranslation();
  return (
    <button
      type="button"
      className="btn danger sm opt-button danger"
      disabled={busy === `revoke:${workUnitId}`}
      onClick={() => onRevoke(workUnitId)}
    >
      {t("approvals.effectRevoke")}
    </button>
  );
}

function RequestButton({
  workUnitId,
  busy,
  onRequest,
}: {
  workUnitId: string;
  busy: string | null;
  onRequest: (workUnitId: string) => void;
}) {
  const { t } = useTranslation();
  return (
    <button
      type="button"
      className="btn accent sm opt-button primary"
      disabled={busy === `request:${workUnitId}`}
      onClick={() => onRequest(workUnitId)}
    >
      {t("approvals.effectRequest")}
    </button>
  );
}

function DecideButtons({
  operationId,
  busy,
  onDecide,
}: {
  operationId: string;
  busy: string | null;
  onDecide: (operationId: string, decision: "granted" | "denied") => void;
}) {
  const { t } = useTranslation();
  const disabled = busy === `decide:${operationId}`;
  return (
    <>
      <button
        type="button"
        className="btn sm opt-button"
        disabled={disabled}
        onClick={() => onDecide(operationId, "granted")}
      >
        {t("approvals.allowOnce")}
      </button>
      <button
        type="button"
        className="btn danger sm opt-button danger"
        disabled={disabled}
        onClick={() => onDecide(operationId, "denied")}
      >
        {t("approvals.deny")}
      </button>
    </>
  );
}

function capitalize(value: string): string {
  return value.charAt(0).toUpperCase() + value.slice(1);
}

// ---------------------------------------------------------------------------
// 既有 tool-call 权限门
// ---------------------------------------------------------------------------

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

  const quiet = pending.length === 0 && !error;

  if (quiet) {
    // 无 tool-call 待批时不渲染这一段（与既有行为一致）；效果授权面板在
    // 安静态下也一并收起——它只服务于「有授权可看/可撤销」的场景。
    return (
      <div className="perm-stack">
        <EffectApprovalPanel taskId={taskId} onChanged={() => refreshDetail(taskId)} />
      </div>
    );
  }

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
      <EffectApprovalPanel taskId={taskId} onChanged={() => refreshDetail(taskId)} />
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
    <section className="perm-card opt-card" role="region" aria-labelledby={headingId} aria-describedby={scopeId}>
      <div className="perm-head">
        <span className="chip risk opt-pill warning" title={permissionRiskLabel(permission.risk_level)}>
          {permission.risk_level} · {permissionRiskLabel(permission.risk_level)}
        </span>
        <span className="perm-tool" id={headingId}>{permission.tool_name}</span>
        <span className={"perm-owner owner-" + attribution.kind}>{attribution.label}</span>
        <span className="perm-hint opt-inline-state approval">{t("approvals.waitingLabel")}</span>
      </div>
      <div className="perm-summary opt-mono" title={permission.input_summary}>
        {permission.input_summary}
      </div>
      <div className="perm-scope opt-help" id={scopeId}>
        {canPersist
          ? t("approvals.persistentScope", {
              tool: permission.tool_name,
              target,
              risk: permission.risk_level,
            })
          : t("approvals.singleUseScope", { risk: permission.risk_level })}
      </div>
      <div className="perm-actions opt-actions">
        <button type="button" className="btn accent sm opt-button primary" disabled={busy} onClick={() => void onDecide(permission.id, "allow")}>
          {t("approvals.allowOnce")}
        </button>
        {canPersist && (
          <button type="button" className="btn sm opt-button" disabled={busy} onClick={() => void onDecide(permission.id, "allow_always")}>
            {t("approvals.allowAlways")}
          </button>
        )}
        <button type="button" className="btn danger sm opt-button danger" disabled={busy} onClick={() => void onDecide(permission.id, "deny")}>
          {t("approvals.deny")}
        </button>
      </div>
    </section>
  );
}
