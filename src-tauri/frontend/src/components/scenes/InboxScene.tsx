import { useEffect, useMemo, useState } from "react";
import { acceptTask, changeRequest, permissionApprove, rollbackTask } from "../../lib/ipc";
import { elapsedSince, permissionAttribution, permissionRiskLabel } from "../../lib/format";
import { taskTitle, workspaceName } from "../../lib/presentation";
import { usePoll } from "../../lib/poll";
import { selectNeedsYou, useTasksStore, type NeedsYouItem } from "../../store/tasks";
import { useAppStore } from "../../store/app";
import type { PermissionDecision } from "../../lib/types";
import { IconArrowRight, IconChevronLeft, IconCheck, IconClose, IconFile, IconInbox, IconShield } from "../icons";

type InspectorKind = "permission" | "review_ready";

const itemKey = (item: NeedsYouItem) => item.kind === "permission" ? `permission:${item.permission!.id}` : `review:${item.task.id}`;

/**
 * 跨项目待处理页。右侧是当前条目的详情（权限详情 / 审核摘要），不是项目动态。
 * 点击关闭会将当前详情收成一条可展开的 rail，展开后恢复同一张可关闭详情卡。
 */
export function InboxScene() {
  const items = useTasksStore(selectNeedsYou);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const refreshDetails = useTasksStore((s) => s.refreshDetails);
  const workspaces = useTasksStore((s) => s.workspaces);
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [inspectorCollapsed, setInspectorCollapsed] = useState(false);
  const [error, setError] = useState<string | null>(null);

  usePoll(async () => {
    await refreshTasks();
    const ids = useTasksStore.getState().tasks.filter((task) => task.state !== "idle" && task.state !== "archived").map((task) => task.id);
    if (ids.length) await refreshDetails(ids);
  }, 2000);

  useEffect(() => {
    if (items.length === 0) {
      setSelectedKey(null);
      setInspectorCollapsed(false);
      return;
    }
    if (!selectedKey || !items.some((item) => itemKey(item) === selectedKey)) setSelectedKey(itemKey(items[0]));
  }, [items, selectedKey]);

  const selected = useMemo(() => items.find((item) => itemKey(item) === selectedKey) ?? null, [items, selectedKey]);
  const kind: InspectorKind = selected?.kind ?? "permission";
  const choose = (key: string) => {
    setSelectedKey(key);
    setInspectorCollapsed(false);
  };

  return (
    <div className={`scene scene-inbox${selected ? " has-inspector" : ""}${inspectorCollapsed ? " inspector-collapsed" : ""}`}>
      <div className="inbox-main">
        <div className="inbox-scroll">
          <header className="inbox-header">
            <div>
              <p className="page-kicker">NEEDS YOU</p>
              <h1>待处理</h1>
              <p>需要你授权、审核或决定的事项集中在这里。</p>
            </div>
            <span className="inbox-count">{items.length} 项</span>
          </header>

          {error && <div className="inbox-error" role="alert">{error}</div>}

          {items.length === 0 ? (
            <div className="inbox-empty"><IconCheck width={24} height={24} /><h2>暂时没有待处理事项</h2><p>权限请求和待审核变更会在出现时显示在这里。</p></div>
          ) : (
            <section className="inbox-list" aria-label="待处理事项">
              {items.map((item) => (
                <InboxRow
                  key={itemKey(item)}
                  item={item}
                  selected={itemKey(item) === selectedKey}
                  projectName={workspaceName(item.task.workspace_path, workspaces)}
                  onSelect={() => choose(itemKey(item))}
                />
              ))}
            </section>
          )}
        </div>
      </div>

      {selected && (
        <aside className="inbox-inspector" aria-label={kind === "permission" ? "权限详情" : "审核摘要"}>
          {inspectorCollapsed ? (
            <button className="inspector-rail-button" onClick={() => setInspectorCollapsed(false)} title={`展开${kind === "permission" ? "权限详情" : "审核摘要"}`}>
              <span>{kind === "permission" ? "权限详情" : "审核摘要"}</span><IconChevronLeft width={16} height={16} />
            </button>
          ) : kind === "permission" ? (
            <PermissionInspector item={selected} onError={setError} onCollapse={() => setInspectorCollapsed(true)} />
          ) : (
            <ReviewInspector item={selected} onError={setError} onCollapse={() => setInspectorCollapsed(true)} />
          )}
        </aside>
      )}
    </div>
  );
}

function InboxRow({ item, selected, projectName, onSelect }: { item: NeedsYouItem; selected: boolean; projectName: string; onSelect: () => void }) {
  const label = item.kind === "permission" ? "权限请求" : "等待审核";
  const description = item.kind === "permission" ? item.permission!.tool_name : "变更已准备好验收";
  return (
    <button className={`inbox-row${selected ? " selected" : ""}`} onClick={onSelect}>
      <span className={`inbox-row-icon ${item.kind}`}>{item.kind === "permission" ? <IconShield width={17} height={17} /> : <IconFile width={17} height={17} />}</span>
      <span className="inbox-row-copy"><small>{label}</small><strong>{taskTitle(item.task)}</strong><em>{description}</em></span>
      <span className="inbox-row-project">{projectName}</span>
      <time>等待 {elapsedSince(item.since)}</time>
      <IconArrowRight className="inbox-row-arrow" width={16} height={16} />
    </button>
  );
}

function InspectorHead({ title, subtitle, onCollapse }: { title: string; subtitle: string; onCollapse: () => void }) {
  return (
    <header className="inspector-head"><div><p className="section-kicker">DECISION DETAIL</p><h2>{title}</h2><span>{subtitle}</span></div><button className="inspector-close" onClick={onCollapse} aria-label={`收起${title}`} title={`收起${title}`}><IconClose width={13} height={13} /></button></header>
  );
}

function PermissionInspector({ item, onError, onCollapse }: { item: NeedsYouItem; onError: (text: string | null) => void; onCollapse: () => void }) {
  const permission = item.permission!;
  const detail = useTasksStore((s) => s.details[item.task.id]);
  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const openRoom = useAppStore((s) => s.openRoom);
  const [busy, setBusy] = useState(false);
  const attribution = permissionAttribution(permission, detail?.runs ?? []);
  const decide = async (decision: Exclude<PermissionDecision, "pending">) => {
    if (busy) return;
    setBusy(true);
    onError(null);
    try {
      await permissionApprove(permission.id, decision);
      await refreshDetail(item.task.id);
    } catch (cause) {
      onError(`权限裁决失败：${String(cause)}`);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="inspector-card">
      <InspectorHead title="权限详情" subtitle={taskTitle(item.task)} onCollapse={onCollapse} />
      <div className="inspector-body">
        <div className="inspector-callout permission"><IconShield width={19} height={19} /><div><strong>{permission.tool_name}</strong><span>{permission.risk_level} · {permissionRiskLabel(permission.risk_level)}</span></div></div>
        <DetailLine label="发起者" value={attribution.label} />
        <DetailLine label="等待时间" value={elapsedSince(item.since)} />
        <div className="inspector-summary"><small>请求说明</small><p>{permission.input_summary || "没有补充说明。"}</p></div>
      </div>
      <footer className="inspector-actions">
        <button className="rc-button rc-button-primary" disabled={busy} onClick={() => void decide("allow")}>允许一次</button>
        <button className="rc-button" disabled={busy} onClick={() => void decide("deny")}>拒绝</button>
        <button className="rc-button rc-button-quiet" disabled={busy} onClick={() => void decide("allow_always")}>始终允许</button>
        <button className="text-link inspector-open-task" onClick={() => openRoom(item.task.id)}>打开任务 <IconArrowRight width={14} height={14} /></button>
      </footer>
    </div>
  );
}

function ReviewInspector({ item, onError, onCollapse }: { item: NeedsYouItem; onError: (text: string | null) => void; onCollapse: () => void }) {
  const detail = useTasksStore((s) => s.details[item.task.id]);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const refreshDetail = useTasksStore((s) => s.refreshDetail);
  const openRoom = useAppStore((s) => s.openRoom);
  const [busy, setBusy] = useState(false);
  const [requestingChanges, setRequestingChanges] = useState(false);
  const [feedback, setFeedback] = useState("");
  const changes = detail?.changes ?? [];
  const verify = detail?.verifications.slice(-1)[0];
  const open = (tab: "review" | "changes") => openRoom(item.task.id, tab);
  const run = async (action: "accept" | "rollback") => {
    if (busy) return;
    setBusy(true);
    onError(null);
    try {
      if (action === "accept") await acceptTask(item.task.id);
      else await rollbackTask(item.task.id);
      await refreshTasks();
    } catch (cause) {
      onError(`${action === "accept" ? "接受变更" : "回滚"}失败：${String(cause)}`);
    } finally {
      setBusy(false);
    }
  };
  const requestChanges = async () => {
    const message = feedback.trim();
    if (!message) {
      onError("请先说明希望修改的内容。");
      return;
    }
    if (busy) return;
    setBusy(true);
    onError(null);
    try {
      await changeRequest(item.task.id, message);
      await Promise.all([refreshTasks(), refreshDetail(item.task.id)]);
      setFeedback("");
      setRequestingChanges(false);
    } catch (cause) {
      onError(`请求修改失败：${String(cause)}`);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="inspector-card">
      <InspectorHead title="审核摘要" subtitle={taskTitle(item.task)} onCollapse={onCollapse} />
      <div className="inspector-body">
        <div className="inspector-callout review"><IconFile width={19} height={19} /><div><strong>{changes.length} 个文件变更</strong><span>{verify ? `${verify.command} · ${verify.status === "passed" ? "验证通过" : verify.status}` : "尚未记录验证"}</span></div></div>
        <div className="inspector-file-list">
          {changes.length === 0 ? <p>变更明细读取中，或当前没有可展示的文件。</p> : changes.slice(0, 5).map((change) => <span key={change.id}><IconFile width={14} height={14} />{change.path}<b>{change.change_type}</b></span>)}
        </div>
        {requestingChanges && (
          <div className="review-request-form">
            <label htmlFor={`change-request-${item.task.id}`}>修改说明</label>
            <textarea id={`change-request-${item.task.id}`} value={feedback} onChange={(event) => setFeedback(event.target.value)} placeholder="例如：请补充异常分支的测试，并说明 API 错误码的兼容策略。" disabled={busy} autoFocus />
            <div><button className="rc-button rc-button-primary" disabled={busy || !feedback.trim()} onClick={() => void requestChanges()}>发送修改请求</button><button className="rc-button rc-button-quiet" disabled={busy} onClick={() => setRequestingChanges(false)}>取消</button></div>
          </div>
        )}
      </div>
      <footer className="inspector-actions review-actions">
        <button className="rc-button rc-button-primary" disabled={busy} onClick={() => void run("accept")}>接受变更</button>
        <button className="rc-button" onClick={() => open("review")}>查看审核</button>
        <button className="rc-button" disabled={busy} onClick={() => setRequestingChanges((open) => !open)}>{requestingChanges ? "收起修改说明" : "请求修改"}</button>
        <button className="rc-button rc-button-quiet" disabled={busy} onClick={() => void run("rollback")}>回滚</button>
        <button className="text-link inspector-open-task" onClick={() => open("changes")}>查看变更 <IconArrowRight width={14} height={14} /></button>
      </footer>
    </div>
  );
}

function DetailLine({ label, value }: { label: string; value: string }) {
  return <div className="inspector-detail-line"><span>{label}</span><strong>{value}</strong></div>;
}
