//! Persistent questions and continuation.
//!
//! Questions persist *before* the run suspends; answers resume exactly
//! once — a repeated answer for the same question is a replayed no-op, and
//! an answer for a different (or expired) question is refused.

use r_code_harness_protocol::services::{QuestionsAskReply, QuestionsAskRequest};
use r_code_harness_protocol::OperationKey;
use std::collections::HashMap;

/// Errors from question operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QuestionError {
    #[error("question {0} is not open")]
    NotOpen(String),
    #[error("question {0} expired")]
    Expired(String),
    #[error("question {0} not found")]
    Unknown(String),
    #[error("answer already recorded for {0}")]
    AlreadyAnswered(String),
}

/// One persisted question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionRecord {
    pub question_id: String,
    pub task_id: String,
    pub run_id: String,
    pub text: String,
    pub options: Vec<String>,
    pub blocking: bool,
    pub state: QuestionState,
    pub answer: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionState {
    Open,
    Answered,
    Expired,
}

/// The question service (host-owned persistence + resume-once).
#[derive(Default)]
pub struct QuestionBoard {
    questions: HashMap<String, QuestionRecord>,
    /// Continuation receipts: answer operation key → question id.
    receipts: HashMap<String, String>,
}

impl QuestionBoard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Persist a question (before suspension) and return its id.
    pub fn ask(
        &mut self,
        task_id: &str,
        run_id: &str,
        request: &QuestionsAskRequest,
    ) -> QuestionsAskReply {
        let sequence = self.questions.len() + 1;
        let question_id = format!("q-{task_id}-{sequence}");
        self.questions.insert(
            question_id.clone(),
            QuestionRecord {
                question_id: question_id.clone(),
                task_id: task_id.to_string(),
                run_id: run_id.to_string(),
                text: request.text.clone(),
                options: request.options.clone(),
                blocking: request.blocking,
                state: QuestionState::Open,
                answer: None,
            },
        );
        QuestionsAskReply { question_id }
    }

    /// Answer a question. The first answer resumes the run; repeats replay
    /// the original outcome without re-suspending or re-resuming.
    pub fn answer(
        &mut self,
        question_id: &str,
        answer: &str,
        operation_key: Option<OperationKey>,
    ) -> Result<AnswerOutcome, QuestionError> {
        let key = operation_key.map(|key| key.0).unwrap_or_default();
        if !key.is_empty() {
            if let Some(prior_question) = self.receipts.get(&key) {
                return if prior_question == question_id {
                    Ok(AnswerOutcome::Replayed)
                } else {
                    Err(QuestionError::Unknown(question_id.to_string()))
                };
            }
        }
        let record = self
            .questions
            .get_mut(question_id)
            .ok_or_else(|| QuestionError::Unknown(question_id.to_string()))?;
        match record.state {
            QuestionState::Open => {
                record.state = QuestionState::Answered;
                record.answer = Some(answer.to_string());
                if !key.is_empty() {
                    self.receipts.insert(key, question_id.to_string());
                }
                Ok(AnswerOutcome::Resumed)
            }
            QuestionState::Answered => Err(QuestionError::AlreadyAnswered(question_id.to_string())),
            QuestionState::Expired => Err(QuestionError::Expired(question_id.to_string())),
        }
    }

    /// Expire an unanswered question (declined/timeout).
    pub fn expire(&mut self, question_id: &str) -> Result<(), QuestionError> {
        let record = self
            .questions
            .get_mut(question_id)
            .ok_or_else(|| QuestionError::Unknown(question_id.to_string()))?;
        if record.state != QuestionState::Open {
            return Err(QuestionError::NotOpen(question_id.to_string()));
        }
        record.state = QuestionState::Expired;
        Ok(())
    }

    pub fn get(&self, question_id: &str) -> Option<&QuestionRecord> {
        self.questions.get(question_id)
    }

    pub fn open_blocking(&self, task_id: &str) -> Option<&QuestionRecord> {
        self.questions.values().find(|record| {
            record.task_id == task_id && record.blocking && record.state == QuestionState::Open
        })
    }
}

/// Result of answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerOutcome {
    /// First answer: the run resumes.
    Resumed,
    /// Same operation key: replayed, no second resume.
    Replayed,
}
