/**
 * Plan 入口建议的客户决策弹窗（docs/plan-mode-dual-track-gate.md §6.1）。
 *
 * 合同要点：
 * - 只有一个通俗原因（宿主模板）+ 两个动作（直接继续 / 先制定计划）+ 一个低层级
 *   帮助入口；不显示 reason、signal、工具名、目录、profile、证据或“双轨”等内部词；
 * - 关闭与 Escape 等价于“直接继续”，不留语义不明的 pending 状态；
 * - 提交期间按钮 busy，并复用同一个幂等键（双击不产生两个决定）；
 * - GuideSheet 协调由宿主（Canvas）完成：本组件只发出 openGuide 请求，不自行
 *   打开第二个 modal；决策草稿（busy/error）保留在宿主。
 */
import { useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";

import { useFocusTrap } from "../../lib/hooks";
import type { PlanEntryOfferView } from "../../lib/types";

export type PlanEntryDecision = "accept" | "continue" | "close" | "escape";

interface PlanEntryDialogProps {
  offer: PlanEntryOfferView;
  busy: boolean;
  error: string | null;
  /** 从手册返回时为 "guide-link"：重新挂载后聚焦帮助入口（docs §12.5）。 */
  returnFocus?: "guide-link" | null;
  onDecide: (decision: PlanEntryDecision, idempotencyKey: string) => void;
  onRetry: () => void;
  onOpenGuide: () => void;
}

export function PlanEntryDialog({
  offer,
  busy,
  error,
  returnFocus,
  onDecide,
  onRetry,
  onOpenGuide,
}: PlanEntryDialogProps) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const acceptRef = useRef<HTMLButtonElement | null>(null);
  const guideLinkRef = useRef<HTMLButtonElement | null>(null);
  const titleId = useId();
  const bodyId = useId();
  // 同一 offer 的整段交互复用同一个幂等键；offer 变化（新建议）才生成新键。
  const idempotencyKeyRef = useRef(`plan-entry-${offer.id}`);
  const [returnFocusToken, setReturnFocusToken] = useState<"guide-link" | null>(
    returnFocus === "guide-link" ? "guide-link" : null,
  );

  useEffect(() => {
    idempotencyKeyRef.current = `plan-entry-${offer.id}-${offer.revision}`;
  }, [offer.id, offer.revision]);

  useFocusTrap(dialogRef, true);

  // 初始焦点落在推荐主动作“先制定计划”；从手册返回时聚焦帮助入口。
  useEffect(() => {
    const target = returnFocusToken === "guide-link" ? guideLinkRef.current : acceptRef.current;
    target?.focus({ preventScroll: true });
  }, [returnFocusToken]);

  // Escape 等价于“直接继续”（docs §6.1）；busy 期间不重复提交。
  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || busy) return;
      event.preventDefault();
      onDecide("escape", idempotencyKeyRef.current);
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [busy, onDecide]);

  const decide = (decision: PlanEntryDecision) => {
    if (busy) return;
    onDecide(decision, idempotencyKeyRef.current);
  };

  return createPortal(
    <div
      className="plan-entry-overlay"
      onPointerDown={(event) => {
        if (event.target !== event.currentTarget || busy) return;
        decide("close");
      }}
    >
      <div
        ref={dialogRef}
        className="plan-entry-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={bodyId}
      >
        <h2 id={titleId} className="plan-entry-title">
          这个任务适合先列个计划
        </h2>
        <p id={bodyId} className="plan-entry-body">
          {offer.customer_copy.lead}
          {offer.customer_copy.suffix}
        </p>
        <p className="plan-entry-note">{offer.customer_copy.quiet_note}</p>
        {offer.notice ? <p className="plan-entry-notice">{offer.notice}</p> : null}
        {error ? (
          <p className="plan-entry-error" role="alert">
            {error}
            <button type="button" className="btn" onClick={onRetry} disabled={busy}>
              重试
            </button>
          </p>
        ) : null}
        <div className="plan-entry-actions">
          <button
            type="button"
            className="btn"
            onClick={() => decide("continue")}
            disabled={busy}
          >
            直接继续
          </button>
          <button
            ref={acceptRef}
            type="button"
            className="btn accent"
            onClick={() => decide("accept")}
            disabled={busy}
          >
            {busy ? "正在处理…" : "先制定计划"}
          </button>
          <button
            ref={guideLinkRef}
            type="button"
            className="plan-entry-guide-link"
            aria-haspopup="dialog"
            onClick={onOpenGuide}
          >
            Plan 模式会做什么？
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
