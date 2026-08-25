/**
 * Codex requestUserInput 问题卡（M3-02，§5.2）。
 *
 * 合同要点：
 * - 不写 Plan store：这是 pending Codex 反向请求的专用卡，不复用 Plan 状态机；
 * - secret 输入用密码框，绝不回显原值，answered 后只显示“已安全提交”；
 * - 提交中锁定（原子 claim 由后端保证，重复提交返回 rejected）；
 * - 终态（answered/cancelled/expired/resolved）只读；
 * - 键盘可完整完成选择/输入/提交/取消（原生表单控件，无 autofocus）；
 * - 状态变化经 aria-live 播报，错误与问题语义关联（aria-describedby）。
 */

import { useMemo, useState } from "react";

import type { CodexUserQuestion } from "../../lib/types";

export interface CodexQuestionCardProps {
  questions: CodexUserQuestion[];
  state: string;
  answerSummary: string[];
  onSubmit: (answers: Record<string, string[]> | null) => Promise<"delivered" | "rejected">;
}

/** 每题草稿：选中的选项 label 集合 + 自由文本（isOther）/secret 值。 */
type Draft = { optionLabels: string[]; text: string };

function draftHasAnswer(draft: Draft | undefined, question: CodexUserQuestion): boolean {
  if (!draft) return false;
  if (question.is_secret) return draft.text.trim().length > 0;
  return draft.optionLabels.length > 0 || draft.text.trim().length > 0;
}

/** 草稿 → 协议答案编码（§4.3：{qid: {answers: [string]}}）。 */
export function encodeUserInputAnswers(
  questions: CodexUserQuestion[],
  drafts: Record<string, Draft>
): Record<string, string[]> {
  const answers: Record<string, string[]> = {};
  for (const question of questions) {
    const draft = drafts[question.id];
    if (!draft || !draftHasAnswer(draft, question)) continue;
    const values = [...draft.optionLabels];
    const text = draft.text.trim();
    if (text && !values.includes(text)) values.push(text);
    answers[question.id] = values;
  }
  return answers;
}

/** answered 后的非敏感摘要：secret 只保留“已安全提交”。 */
export function summarizeAnswers(
  questions: CodexUserQuestion[],
  answers: Record<string, string[]>
): string[] {
  const summary: string[] = [];
  for (const question of questions) {
    const values = answers[question.id];
    if (!values || values.length === 0) continue;
    summary.push(
      question.is_secret
        ? `${question.header}：已安全提交`
        : `${question.header}：${values.join("、")}`
    );
  }
  return summary;
}

const STATE_LABELS: Record<string, string> = {
  pending: "等待回答",
  submitting: "提交中",
  answered: "已回答",
  cancelled: "已取消",
  expired: "已过期",
  resolved: "已在别处处理",
};

export function CodexQuestionCard({ questions, state, answerSummary, onSubmit }: CodexQuestionCardProps) {
  const [drafts, setDrafts] = useState<Record<string, Draft>>({});
  const [error, setError] = useState<string | null>(null);
  const readonly = state !== "pending";
  const submitting = state === "submitting";

  const allAnswered = useMemo(
    () => questions.every((question) => draftHasAnswer(drafts[question.id], question)),
    [drafts, questions]
  );

  const updateDraft = (id: string, patch: Partial<Draft>) => {
    setDrafts((current) => ({
      ...current,
      [id]: { optionLabels: current[id]?.optionLabels ?? [], text: current[id]?.text ?? "", ...patch },
    }));
  };

  const submit = async (answers: Record<string, string[]> | null) => {
    setError(null);
    const outcome = await onSubmit(answers);
    if (outcome !== "delivered") {
      setError("提交被拒绝：请求可能已过期或已被回答。");
    }
  };

  const statusText = STATE_LABELS[state] ?? state;

  return (
    <section
      className={`codex-question-card state-${state}`}
      aria-label={`Codex 提问：${statusText}`}
    >
      <header className="codex-question-head">
        <strong>Codex 需要你的输入</strong>
        <span className="codex-question-state" role="status" aria-live="polite">
          {statusText}
        </span>
      </header>
      <div className="codex-question-list">
        {questions.map((question, index) => {
          const draft = drafts[question.id];
          const describedBy = `codex-question-error-${index}`;
          return (
            <fieldset className="codex-question" key={question.id} disabled={readonly}>
              <legend>
                <span className="codex-question-index">{index + 1}</span>
                <span>
                  <strong>{question.header}</strong>
                  <small>{question.question}</small>
                </span>
              </legend>
              {question.is_secret ? (
                <label className="codex-question-secret">
                  <span className="codex-question-input-label">安全输入（不回显）</span>
                  <input
                    type="password"
                    autoComplete="off"
                    aria-label={`${question.header}的安全回答`}
                    value={draft?.text ?? ""}
                    onChange={(event) => updateDraft(question.id, { text: event.target.value })}
                  />
                </label>
              ) : (
                <>
                  <div className="codex-question-options" role="group" aria-label={question.header}>
                    {question.options.map((option) => {
                      const checked = draft?.optionLabels.includes(option.label) ?? false;
                      return (
                        <label className={checked ? "is-selected" : ""} key={option.label}>
                          <input
                            type="checkbox"
                            checked={checked}
                            onChange={(event) =>
                              updateDraft(question.id, {
                                optionLabels: event.target.checked
                                  ? [...(draft?.optionLabels ?? []), option.label]
                                  : (draft?.optionLabels ?? []).filter((label) => label !== option.label),
                              })
                            }
                          />
                          <span>
                            <strong>{option.label}</strong>
                            {option.description && <small>{option.description}</small>}
                          </span>
                        </label>
                      );
                    })}
                  </div>
                  {(question.is_other || question.options.length === 0) && (
                    <label className="codex-question-custom">
                      <span className="codex-question-input-label">自定义回答</span>
                      <input
                        type="text"
                        aria-label={`${question.header}的自定义回答`}
                        value={draft?.text ?? ""}
                        placeholder="输入其他答案…"
                        onChange={(event) => updateDraft(question.id, { text: event.target.value })}
                      />
                    </label>
                  )}
                </>
              )}
            </fieldset>
          );
        })}
      </div>
      {state === "answered" && answerSummary.length > 0 && (
        <ul className="codex-question-summary">
          {answerSummary.map((line) => (
            <li key={line}>{line}</li>
          ))}
        </ul>
      )}
      {error && (
        <p className="codex-question-error" id="codex-question-error-0" role="alert">
          {error}
        </p>
      )}
      {!readonly && (
        <div className="codex-question-actions">
          <button
            type="button"
            className="codex-question-submit"
            disabled={!allAnswered || submitting}
            aria-busy={submitting}
            onClick={() => void submit(encodeUserInputAnswers(questions, drafts))}
          >
            {submitting ? "提交中…" : "提交回答"}
          </button>
          <button
            type="button"
            className="codex-question-cancel"
            disabled={submitting}
            onClick={() => void submit(null)}
          >
            取消
          </button>
          {!allAnswered && (
            <small className="codex-question-hint">每个问题都需要至少一个答案</small>
          )}
        </div>
      )}
    </section>
  );
}
