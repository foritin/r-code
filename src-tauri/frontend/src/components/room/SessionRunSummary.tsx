import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { FileChange, PlanView } from "../../lib/types";
import { useAppStore } from "../../store/app";
import { IconCheck, IconChevronDown, IconFile } from "../icons";
import { AnchoredSurface } from "../ui/AnchoredSurface";
import { compactInstruction, type PlanStep } from "./model";
import { changeTypeLabel, pathParts, useRunChangeStats } from "./run-change-stats";
import {
  currentStepNumber,
  sessionSummarySteps,
  type SessionSummaryStepState,
} from "./session-run-summary-model";

interface Props {
  taskId: string;
  runId: string;
  running: boolean;
  liveSteps: readonly PlanStep[];
  planView: PlanView | null;
  changes: readonly FileChange[];
  /** 当前运行的可观察阶段文案（无计划步骤/文件变更时作为权威状态锚点）。 */
  activityLabel?: string | null;
  /** 当前仍在活动的子代理数量。 */
  activeSubagents?: number;
  /** 当前运行轮的发起指令；hover 时以 ≤10 字 + 省略号展示。 */
  requestText?: string | null;
}

type OpenPanel = "steps" | "changes" | null;

function stepStateLabel(state: SessionSummaryStepState): string {
  switch (state) {
    case "completed": return "已完成";
    case "current": return "进行中";
    case "blocked": return "受阻";
    case "failed": return "失败";
    case "cancelled": return "已取消";
    case "pending": return "待处理";
  }
}

export function SessionRunSummary({
  taskId,
  runId,
  running,
  liveSteps,
  planView,
  changes,
  activityLabel = null,
  activeSubagents = 0,
  requestText = null,
}: Props) {
  const openWorkbenchFile = useAppStore((state) => state.openWorkbenchFile);
  const [openPanel, setOpenPanel] = useState<OpenPanel>(null);
  const stepsAnchor = useRef<HTMLButtonElement>(null);
  const changesAnchor = useRef<HTMLButtonElement>(null);
  const stepsSurface = useRef<HTMLDivElement>(null);
  const changesSurface = useRef<HTMLDivElement>(null);
  const requestHint = compactInstruction(requestText);

  const steps = useMemo(
    () => sessionSummarySteps(liveSteps, planView, running),
    [liveSteps, planView, running],
  );
  const {
    files,
    statsByPath,
    additions,
    deletions,
    statsPending,
    hasKnownStats,
  } = useRunChangeStats(taskId, runId, changes);

  useEffect(() => {
    setOpenPanel(null);
  }, [runId, taskId]);

  useEffect(() => {
    if (steps.length === 0 && openPanel === "steps") setOpenPanel(null);
    if (files.length === 0 && openPanel === "changes") setOpenPanel(null);
  }, [files.length, openPanel, steps.length]);

  const dismiss = useCallback(() => setOpenPanel(null), []);
  const currentStep = currentStepNumber(steps);
  const completedSteps = steps.filter((step) => step.state === "completed").length;
  const hasStatusAnchor = running && (Boolean(activityLabel?.trim()) || activeSubagents > 0);

  if (steps.length === 0 && files.length === 0 && !hasStatusAnchor) return null;

  return (
    <div className="session-run-summary-slot" data-testid="session-run-summary">
      <div className={`session-run-summary${running ? " is-running" : " is-settled"}`}>
        {steps.length === 0 && files.length === 0 && hasStatusAnchor && (
          <div
            className="session-run-summary-trigger status-trigger"
            role="status"
            aria-live="polite"
            title={requestHint ?? undefined}
          >
            <span className="session-run-state" aria-hidden="true"><i /></span>
            <span className="session-summary-long">
              {activityLabel?.trim() || "运行中"}
              {activeSubagents > 0 ? ` · ${activeSubagents} 个子代理` : ""}
            </span>
            <span className="session-summary-short" aria-hidden="true">
              {activityLabel?.trim() || "运行中"}
            </span>
          </div>
        )}

        {steps.length > 0 && (
          <button
            ref={stepsAnchor}
            type="button"
            className="session-run-summary-trigger step-trigger"
            aria-expanded={openPanel === "steps"}
            aria-controls="session-step-popover"
            title={requestHint ?? "查看当前任务步骤"}
            onClick={() => setOpenPanel((current) => current === "steps" ? null : "steps")}
          >
            <span className="session-run-state" aria-hidden="true"><i /></span>
            <span className="session-summary-long">
              {completedSteps === steps.length ? `${steps.length} / ${steps.length} 步已完成` : `第 ${currentStep} / ${steps.length} 步`}
            </span>
            <span className="session-summary-short" aria-hidden="true">{currentStep}/{steps.length} 步</span>
            <IconChevronDown className="session-summary-chevron" width={12} height={12} aria-hidden="true" />
          </button>
        )}

        {steps.length > 0 && files.length > 0 && <span className="session-run-summary-divider" aria-hidden="true" />}

        {files.length > 0 && (
          <button
            ref={changesAnchor}
            type="button"
            className="session-run-summary-trigger change-trigger"
            aria-expanded={openPanel === "changes"}
            aria-controls="session-change-popover"
            aria-busy={statsPending}
            title={requestHint ?? "查看当前对话产生的文件变更"}
            onClick={() => setOpenPanel((current) => current === "changes" ? null : "changes")}
          >
            {steps.length === 0 && <span className="session-run-state" aria-hidden="true"><i /></span>}
            <span className="session-summary-long"><strong>{files.length}</strong> 个文件已更改</span>
            <span className="session-summary-short" aria-hidden="true"><strong>{files.length}</strong> 文件</span>
            {hasKnownStats && (
              <span className="session-summary-diffstat" aria-label={`新增 ${additions} 行，删除 ${deletions} 行`}>
                <b className="add">+{additions}</b>
                <b className="del">−{deletions}</b>
              </span>
            )}
            {!hasKnownStats && statsPending && <span className="session-summary-stats-loading" aria-label="正在统计变更行数">···</span>}
            <IconChevronDown className="session-summary-chevron" width={12} height={12} aria-hidden="true" />
          </button>
        )}
      </div>

      {openPanel === "steps" && (
        <AnchoredSurface
          id="session-step-popover"
          anchorRef={stepsAnchor}
          surfaceRef={stepsSurface}
          className="session-summary-popover session-step-popover"
          role="dialog"
          label="当前任务步骤"
          placement="up"
          align="center"
          gap={8}
          onDismiss={dismiss}
          onKeyDown={(event) => {
            if (event.key !== "Escape") return;
            event.preventDefault();
            event.stopPropagation();
            setOpenPanel(null);
            requestAnimationFrame(() => stepsAnchor.current?.focus());
          }}
        >
          <header className="session-summary-popover-head">
            <span>当前任务步骤</span>
            <small>{completedSteps} / {steps.length} 已完成</small>
          </header>
          <ol className="session-step-list">
            {steps.map((step, index) => (
              <li
                className={`session-step-row state-${step.state}`}
                aria-current={step.state === "current" ? "step" : undefined}
                key={step.id}
                title={step.detail ?? step.label}
              >
                <span className="session-step-marker" aria-hidden="true">
                  {step.state === "completed" ? <IconCheck width={12} height={12} /> : <i />}
                </span>
                <span className="session-step-copy">
                  <strong>{step.label}</strong>
                  {step.detail && <small>{step.detail}</small>}
                </span>
                <span className="session-step-state">{stepStateLabel(step.state)}</span>
                <span className="sr-only">第 {index + 1} 步，{stepStateLabel(step.state)}</span>
              </li>
            ))}
          </ol>
        </AnchoredSurface>
      )}

      {openPanel === "changes" && (
        <AnchoredSurface
          id="session-change-popover"
          anchorRef={changesAnchor}
          surfaceRef={changesSurface}
          className="session-summary-popover session-change-popover"
          role="dialog"
          label="当前对话的文件变更"
          placement="up"
          align="right"
          gap={8}
          onDismiss={dismiss}
          onKeyDown={(event) => {
            if (event.key !== "Escape") return;
            event.preventDefault();
            event.stopPropagation();
            setOpenPanel(null);
            requestAnimationFrame(() => changesAnchor.current?.focus());
          }}
        >
          <header className="session-summary-popover-head">
            <span>当前对话的文件变更</span>
            <small>{files.length} 个文件</small>
          </header>
          <div className="session-change-list">
            {files.map((file) => {
              const parts = pathParts(file.path);
              const stat = statsByPath[file.path];
              return (
                <button
                  type="button"
                  className="session-change-row"
                  key={file.path}
                  title={`在文件工作台打开 ${file.path}`}
                  onClick={() => {
                    setOpenPanel(null);
                    openWorkbenchFile(taskId, file.path);
                  }}
                >
                  <IconFile width={14} height={14} aria-hidden="true" />
                  <span className="session-change-path">
                    <strong>{parts.name}</strong>
                    {parts.directory && <small>{parts.directory}</small>}
                  </span>
                  {stat?.available ? (
                    <span className="session-change-stat" aria-label={`新增 ${stat.additions ?? 0} 行，删除 ${stat.deletions ?? 0} 行`}>
                      <b className="add">+{stat.additions ?? 0}</b>
                      <b className="del">−{stat.deletions ?? 0}</b>
                    </span>
                  ) : stat ? (
                    <span className="session-change-kind">{changeTypeLabel(file)}</span>
                  ) : (
                    <span className="session-change-stat-pending" aria-label="正在统计">···</span>
                  )}
                </button>
              );
            })}
          </div>
        </AnchoredSurface>
      )}
    </div>
  );
}
