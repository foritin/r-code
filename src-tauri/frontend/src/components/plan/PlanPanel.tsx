import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  planAnswer,
  planApprove,
  planCancel,
  planCreate,
  planGet,
  planRepairProjection,
  planRetryContinuation,
  planRetryImplementation,
} from "../../lib/ipc";
import { usePoll } from "../../lib/poll";
import type {
  PlanItem,
  PlanQuestionAnswerInput,
  PlanQuestionSet,
  PlanState,
  PlanView,
  Task,
} from "../../lib/types";
import {
  IconCheck,
  IconChevronDown,
  IconHelp,
  IconRefresh,
} from "../icons";
import { StatusBar } from "../ui/StatusBar";

type AnswerDraft =
  | { kind: "option"; optionId: string }
  | { kind: "text"; text: string };

interface Props {
  task: Task;
  running: boolean;
  onTaskChanged?: () => Promise<void> | void;
}

const PLAN_STATE_LABEL: Record<PlanState, string> = {
  draft: "草拟中",
  awaiting_input: "需要你确认",
  ready: "等待确认实施",
  approved: "已确认",
  executing: "实施中",
  completed: "已完成",
  cancelled: "已取消",
};

const ITEM_STATE_LABEL: Record<PlanItem["state"], string> = {
  proposed: "待确认",
  pending: "等待依赖",
  in_progress: "进行中",
  blocked: "已阻塞",
  completed: "已完成",
  failed: "失败",
  cancelled: "已取消",
};

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function newIdempotencyKey(): string {
  return globalThis.crypto?.randomUUID?.()
    ?? `plan-answer-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

function featureProgress(items: readonly PlanItem[]): { completed: number; total: number } {
  return {
    completed: items.filter((item) => item.state === "completed").length,
    total: items.length,
  };
}

export function PlanPanel({ task, running, onTaskChanged }: Props) {
  const [view, setView] = useState<PlanView | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [expanded, setExpanded] = useState(true);
  const [busy, setBusy] = useState<
    "create" | "answer" | "skip" | "retry" | "approve" | "repair" | "retryImplementation" | "cancel" | null
  >(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [cancelArmedRevision, setCancelArmedRevision] = useState<number | null>(null);
  const [answers, setAnswers] = useState<Record<string, AnswerDraft>>({});
  const [retryQuestionSetId, setRetryQuestionSetId] = useState<string | null>(null);
  const currentQuestionSetId = view?.pending_question_set?.id ?? null;
  const answerKeys = useRef(new Map<string, string>());

  const refresh = useCallback(async () => {
    try {
      const next = await planGet(task.id);
      setView(next);
      setLoaded(true);
      return next;
    } catch (cause) {
      setLoaded(true);
      setError(`读取计划失败：${errorText(cause)}`);
      throw cause;
    }
  }, [task.id]);

  usePoll(async () => { await refresh(); }, 1800, task.mode === "plan" || view != null, "计划状态");

  useEffect(() => {
    setView(null);
    setLoaded(false);
    setAnswers({});
    setRetryQuestionSetId(null);
    setCancelArmedRevision(null);
    setNotice(null);
    setError(null);
  }, [task.id]);

  useEffect(() => {
    if (!currentQuestionSetId) return;
    setExpanded(true);
    setAnswers({});
    setNotice(null);
  }, [currentQuestionSetId]);

  const progress = useMemo(() => featureProgress(view?.items ?? []), [view?.items]);
  const itemTitles = useMemo(
    () => new Map((view?.items ?? []).map((item) => [item.id, item.title])),
    [view?.items],
  );
  const visible = task.mode === "plan" || view != null;
  const cancelArmed = view != null && cancelArmedRevision === view.plan.revision;
  if (!visible) return null;

  const initialize = async () => {
    if (busy) return;
    setBusy("create");
    setError(null);
    try {
      const next = await planCreate(task.id);
      setView(next);
      setExpanded(true);
    } catch (cause) {
      setError(`初始化计划失败：${errorText(cause)}`);
    } finally {
      setBusy(null);
    }
  };

  const answerSet = async (questionSet: PlanQuestionSet, skipAll: boolean) => {
    if (busy || !view) return;
    let payload: PlanQuestionAnswerInput[] = [];
    if (!skipAll) {
      const missing = questionSet.questions.filter((question) => {
        const draft = answers[question.id];
        return !draft || (draft.kind === "text" && !draft.text.trim());
      });
      if (missing.length > 0) {
        setError(`请逐项回答：${missing.map((question) => question.header).join("、")}；也可以跳过整组。`);
        return;
      }
      payload = questionSet.questions.map((question) => {
        const draft = answers[question.id]!;
        return draft.kind === "option"
          ? { kind: "option", question_id: question.id, option_id: draft.optionId }
          : { kind: "text", question_id: question.id, text: draft.text.trim() };
      });
    }

    const operation = skipAll ? "skip" : "answer";
    setBusy(operation);
    setError(null);
    setNotice(skipAll ? "正在跳过这组问题…" : "正在提交回答…");
    const key = answerKeys.current.get(questionSet.id) ?? newIdempotencyKey();
    answerKeys.current.set(questionSet.id, key);
    try {
      const next = await planAnswer(task.id, {
        question_set_id: questionSet.id,
        expected_revision: questionSet.revision,
        idempotency_key: key,
        skip_all: skipAll,
        answers: payload,
      });
      setView(next);
      setRetryQuestionSetId(null);
      setNotice(next.continuation_question_set
        ? "回答已接纳，Agent 正在续接同一份计划。"
        : "回答已接纳，计划已继续更新。");
      await onTaskChanged?.();
    } catch (cause) {
      const message = errorText(cause);
      setError(`提交计划回答失败：${message}`);
      setNotice(null);
      if (/续接|continuation|dispatch/i.test(message)) setRetryQuestionSetId(questionSet.id);
    } finally {
      setBusy(null);
    }
  };

  const retryContinuation = async (questionSetId: string) => {
    if (busy) return;
    setBusy("retry");
    setError(null);
    try {
      setView(await planRetryContinuation(task.id, questionSetId));
      setRetryQuestionSetId(null);
      setNotice("已重新请求续接计划。");
    } catch (cause) {
      setError(`重试计划续接失败：${errorText(cause)}`);
    } finally {
      setBusy(null);
    }
  };

  const approve = async () => {
    if (!view || busy) return;
    setBusy("approve");
    setError(null);
    try {
      const next = await planApprove(task.id, view.plan.id, view.plan.revision);
      setView(next);
      setNotice("计划已确认，功能事项将按依赖顺序进入实施。");
      await onTaskChanged?.();
    } catch (cause) {
      setError(`确认实施失败：${errorText(cause)}`);
      await refresh().catch(() => undefined);
    } finally {
      setBusy(null);
    }
  };

  const repairProjection = async () => {
    if (!view || busy) return;
    setBusy("repair");
    setError(null);
    try {
      setView(await planRepairProjection(task.id, view.plan.id));
      setNotice("计划文档已重新同步。");
    } catch (cause) {
      setError(`修复计划文档失败：${errorText(cause)}`);
    } finally {
      setBusy(null);
    }
  };

  const retryImplementation = async () => {
    if (!view || busy) return;
    setBusy("retryImplementation");
    setError(null);
    try {
      const next = await planRetryImplementation(task.id, view.plan.id);
      setView(next);
      setNotice("实施任务已重新加入可靠队列。即使应用重启，也会从这里继续。");
      await onTaskChanged?.();
    } catch (cause) {
      setError(`重试实施派发失败：${errorText(cause)}`);
      await refresh().catch(() => undefined);
    } finally {
      setBusy(null);
    }
  };

  const cancel = async () => {
    if (!view || busy || running) return;
    if (!cancelArmed) {
      setBusy("cancel");
      setError(null);
      try {
        // Refresh before arming the destructive action. Plan item/review activity may have
        // advanced the revision since the last poll; confirming against that stale snapshot
        // would otherwise force the user through an avoidable conflict round-trip.
        const latest = await refresh();
        if (!latest) throw new Error("当前计划已不存在");
        setCancelArmedRevision(latest.plan.revision);
        setNotice("再次点击“确认取消”才会取消计划；工作区中的文件不会被回滚。");
      } catch {
        setNotice(null);
      } finally {
        setBusy(null);
      }
      return;
    }
    setBusy("cancel");
    setError(null);
    try {
      const next = await planCancel(task.id, view.plan.id, view.plan.revision);
      setView(next);
      setCancelArmedRevision(null);
      setNotice("计划已取消。现有工作区文件保持不变，你可以从“添加”重新建立计划。");
      await onTaskChanged?.();
    } catch (cause) {
      setError(`取消计划失败：${errorText(cause)}`);
      await refresh().catch(() => undefined);
      setNotice("计划在确认期间发生了变化，请检查最新状态后重新取消。");
    } finally {
      setBusy(null);
    }
  };

  if (!loaded && !view) {
    return <div className="plan-panel plan-panel-loading" role="status">正在读取计划…</div>;
  }

  if (!view) {
    return (
      <section className="plan-panel plan-panel-empty" aria-label="计划模式">
        <IconHelp width={16} height={16} />
        <span>计划模式已开启，但尚未建立计划文档。</span>
        <button className="quiet-link" type="button" disabled={busy === "create"} onClick={() => void initialize()}>
          {busy === "create" ? "初始化中…" : "初始化计划"}
        </button>
        {error && <StatusBar kind="error" compact>{error}</StatusBar>}
      </section>
    );
  }

  const questionSet = view.pending_question_set;
  const continuationSet = view.continuation_question_set;
  const implementationReady = view.plan.state === "ready" && view.items.length > 0;
  const cancellable = !["completed", "cancelled"].includes(view.plan.state);
  const progressPercent = progress.total > 0 ? Math.round(progress.completed / progress.total * 100) : 0;

  return (
    <section className={`plan-panel state-${view.plan.state}`} aria-label="当前计划">
      <button
        className="plan-panel-summary"
        type="button"
        onClick={() => setExpanded((value) => !value)}
        aria-expanded={expanded}
      >
        <span className="plan-state-diamond" aria-hidden="true" />
        <span className="plan-summary-copy">
          <strong>计划</strong>
          <small>{PLAN_STATE_LABEL[view.plan.state]} · 修订 {view.plan.revision}</small>
        </span>
        {progress.total > 0 && (
          <span className="plan-summary-progress">{progress.completed}/{progress.total} 功能</span>
        )}
        {running && <span className="plan-runtime-state">Agent 运行中</span>}
        <IconChevronDown className={expanded ? "is-open" : ""} width={14} height={14} />
      </button>

      {expanded && (
        <div className="plan-panel-body">
          <div className="plan-metadata">
            <span title={view.plan.projection_path ?? "计划文档尚未生成"}>
              文档 · {view.plan.projection_path ?? "准备中"}
            </span>
            <span>同步修订 · {view.plan.projection_revision ?? "—"}</span>
            <button type="button" className="iconbtn" aria-label="刷新计划" title="刷新计划" onClick={() => void refresh()}>
              <IconRefresh width={13} height={13} />
            </button>
          </div>

          {view.goal.goal && (
            <p className="plan-goal"><span>目标</span>{view.goal.goal}</p>
          )}

          {view.plan.projection_error && (
            <StatusBar
              kind="warn"
              compact
              action={{ label: busy === "repair" ? "同步中…" : "重新同步", onClick: () => void repairProjection(), disabled: busy === "repair" }}
            >
              计划正文同步失败：{view.plan.projection_error}
            </StatusBar>
          )}
          {error && <StatusBar kind="error" compact onDismiss={() => setError(null)}>{error}</StatusBar>}
          {notice && <StatusBar kind="info" compact onDismiss={() => setNotice(null)}>{notice}</StatusBar>}

          {questionSet && (
            <div className="plan-hitl" role="group" aria-label="计划需要你的回答">
              <header>
                <IconHelp width={16} height={16} />
                <div>
                  <strong>需要你确认 {questionSet.questions.length} 个问题</strong>
                  <p>每个问题单独作答；选项和自定义回答不会串到其他问题。</p>
                </div>
              </header>
              <div className="plan-question-list">
                {questionSet.questions.map((question, questionIndex) => {
                  const draft = answers[question.id];
                  return (
                    <fieldset className="plan-question" key={question.id}>
                      <legend>
                        <span>{questionIndex + 1}</span>
                        <span><strong>{question.header}</strong><small>{question.question}</small></span>
                      </legend>
                      <div className="plan-question-options">
                        {question.options.map((option) => {
                          const checked = draft?.kind === "option" && draft.optionId === option.id;
                          return (
                            <label className={checked ? "is-selected" : ""} key={option.id}>
                              <input
                                type="radio"
                                name={`plan-question-${question.id}`}
                                checked={checked}
                                onChange={() => setAnswers((current) => ({
                                  ...current,
                                  [question.id]: { kind: "option", optionId: option.id },
                                }))}
                              />
                              <span><strong>{option.label}</strong><small>{option.description}</small></span>
                            </label>
                          );
                        })}
                        <label className={`plan-question-custom${draft?.kind === "text" ? " is-selected" : ""}`}>
                          <input
                            type="radio"
                            name={`plan-question-${question.id}`}
                            checked={draft?.kind === "text"}
                            onChange={() => setAnswers((current) => {
                              const existing = current[question.id];
                              return {
                                ...current,
                                [question.id]: { kind: "text", text: existing?.kind === "text" ? existing.text : "" },
                              };
                            })}
                          />
                          <input
                            type="text"
                            aria-label={`${question.header}的自定义回答`}
                            value={draft?.kind === "text" ? draft.text : ""}
                            placeholder="自定义回答…"
                            onFocus={() => setAnswers((current) => {
                              const existing = current[question.id];
                              return {
                                ...current,
                                [question.id]: { kind: "text", text: existing?.kind === "text" ? existing.text : "" },
                              };
                            })}
                            onChange={(event) => setAnswers((current) => ({
                              ...current,
                              [question.id]: { kind: "text", text: event.target.value },
                            }))}
                          />
                        </label>
                      </div>
                    </fieldset>
                  );
                })}
              </div>
              <footer>
                <button type="button" className="quiet-link" disabled={busy != null} onClick={() => void answerSet(questionSet, true)}>
                  {busy === "skip" ? "跳过中…" : "跳过整组"}
                </button>
                <button type="button" className="btn accent" disabled={busy != null} onClick={() => void answerSet(questionSet, false)}>
                  {busy === "answer" ? "提交中…" : "提交回答"}
                </button>
              </footer>
            </div>
          )}

          {continuationSet && ["pending", "dispatching"].includes(continuationSet.continuation_state) && (
            <StatusBar kind="info" compact>
              已收到你的回答，正在续接同一份计划；可以继续查看对话，无需重复提交。
            </StatusBar>
          )}

          {continuationSet?.continuation_state === "failed" && (
            <StatusBar
              kind="error"
              compact
              action={{
                label: busy === "retry" ? "重试中…" : "重试续接",
                onClick: () => void retryContinuation(continuationSet.id),
                disabled: busy != null,
              }}
            >
              计划续接失败：{continuationSet.continuation_error ?? "运行未能继续，但你的回答已经安全保存。"}
            </StatusBar>
          )}

          {["pending", "dispatching"].includes(view.plan.implementation_dispatch_state) && (
            <StatusBar kind="info" compact>
              已确认计划，正在把实施事项写入可靠队列。完成前不会重复启动同一份计划。
            </StatusBar>
          )}

          {view.plan.implementation_dispatch_state === "failed" && (
            <StatusBar
              kind="error"
              compact
              action={{
                label: busy === "retryImplementation" ? "重试中…" : "重试实施",
                onClick: () => void retryImplementation(),
                disabled: busy != null || running,
              }}
            >
              实施任务尚未启动：{view.plan.implementation_dispatch_error ?? "可靠队列派发失败，计划内容仍已保存。"}
            </StatusBar>
          )}

          {retryQuestionSetId && continuationSet?.continuation_state !== "failed" && (
            <button className="plan-retry" type="button" disabled={busy != null} onClick={() => void retryContinuation(retryQuestionSetId)}>
              <IconRefresh width={13} height={13} />
              {busy === "retry" ? "正在重试续接…" : "重试计划续接"}
            </button>
          )}

          {view.items.length > 0 && (
            <div className="plan-feature-section">
              <div className="plan-feature-head">
                <div>
                  <strong>功能事项</strong>
                  <small>按可独立验收的功能拆分，不按文件拆分</small>
                </div>
                <span>{progress.completed}/{progress.total}</span>
              </div>
              <div className="plan-progress" role="progressbar" aria-valuemin={0} aria-valuemax={progress.total} aria-valuenow={progress.completed}>
                <span style={{ width: `${progressPercent}%` }} />
              </div>
              <ol className="plan-feature-list">
                {view.items.map((item, index) => (
                  <li className={`state-${item.state}`} key={item.id}>
                    <span className="plan-feature-marker" aria-hidden="true" />
                    <span className="plan-feature-copy">
                      <span><b>{index + 1}</b><strong>{item.title}</strong><em>{ITEM_STATE_LABEL[item.state]}</em></span>
                      <small>{item.description}</small>
                      {item.depends_on.length > 0 && (
                        <small className="plan-feature-dependencies">
                          依赖：{item.depends_on.map((id) => itemTitles.get(id) ?? id).join("、")}
                        </small>
                      )}
                    </span>
                    {item.state === "completed" && <IconCheck width={14} height={14} aria-label="已完成" />}
                  </li>
                ))}
              </ol>
            </div>
          )}

          {implementationReady && (
            <div className="plan-approval">
              <span>{running
                ? "计划已准备好，等待本轮规划运行安全结束后即可确认。"
                : "计划已准备好。确认后，事项才会进入实施与增强审核。"}</span>
              <button
                className="btn accent"
                type="button"
                disabled={busy != null || running}
                title={running ? "请等待当前 Plan 运行结束" : "确认并开始实施"}
                onClick={() => void approve()}
              >
                {busy === "approve" ? "确认中…" : "确认实施"}
              </button>
            </div>
          )}


          {cancellable && (
            <div className="plan-cancel-row">
              <span>取消只终止计划、待办和后续派发，不撤销已经写入工作区的文件。</span>
              <button
                className={`quiet-link danger-link${cancelArmed ? " is-armed" : ""}`}
                type="button"
                disabled={busy != null || running}
                title={running ? "请先停止或等待当前运行结束" : "取消当前计划"}
                onClick={() => void cancel()}
              >
                {busy === "cancel" ? "取消中…" : cancelArmed ? "确认取消" : "取消计划"}
              </button>
            </div>
          )}
        </div>
      )}
    </section>
  );
}
