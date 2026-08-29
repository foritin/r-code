//! agent-event 前端出口的批内合并与借用序列化（F-perf-03）。
//!
//! drain 循环以 40ms 一批排空事件，但批内每个文本 delta 仍各自走一次 IPC
//! emit（每条付：serde Value 树分配 → 序列化 → WebView 解析）。前端虽有
//! 100ms 渲染 coalescer 兜底，IPC 通道与 JSON.parse 成本照付。
//!
//! 两层修复：
//! - [`DeltaCoalescer`]：同一 task 的连续 `Message { delta: true }` 在批内
//!   合并为一条再 emit（文本拼接，顺序与逐条发送完全一致）；任何其它事件
//!   到达时先冲刷挂起帧，保证跨类型顺序不变。
//! - [`AgentEventEnvelope`]：借用型信封直发，去掉逐事件的 `json!` 中间树。

use r_code_core::dto::AgentEvent;

/// 前端 `agent-event` 信封：字段名与历史上的 `json!({ "task_id", "event" })`
/// 完全一致，WebView 契约不变；借用避免把 event 文本再拷进 Value 树。
#[derive(serde::Serialize)]
pub struct AgentEventEnvelope<'a> {
    pub task_id: &'a str,
    pub event: &'a AgentEvent,
}

/// 批内文本 delta 合并器。纯逻辑、无 IO，行为由单元测试钉住。
#[derive(Debug, Default)]
pub struct DeltaCoalescer {
    /// 挂起的合并帧：(task_id, 已并入的 delta 文本)。
    pending: Option<(String, String)>,
}

/// 一个输入事件的分步结果：可能无需 emit（已并入挂起帧），
/// 也可能需要先冲刷挂起帧再 emit 当前事件。
#[derive(Debug)]
pub enum CoalesceStep<'e> {
    /// 当前 delta 已并入挂起帧，本步不 emit。
    Merged,
    /// 直接 emit 当前事件（借用）。
    Emit {
        task_id: &'e str,
        event: &'e AgentEvent,
    },
    /// 先 emit 冲刷出来的合并帧，再 emit 当前事件（借用）。
    /// 冲刷帧装箱：AgentEvent 较大，避免枚举整体随最大变体膨胀。
    FlushThenEmit {
        flushed_task_id: String,
        flushed_event: Box<AgentEvent>,
        task_id: &'e str,
        event: &'e AgentEvent,
    },
}

impl DeltaCoalescer {
    pub fn step<'e>(&mut self, task_id: &'e str, event: &'e AgentEvent) -> CoalesceStep<'e> {
        if let AgentEvent::Message { text, delta: true } = event {
            return match self.pending.take() {
                Some((pending_task, mut merged)) if pending_task == task_id => {
                    merged.push_str(text);
                    self.pending = Some((pending_task, merged));
                    CoalesceStep::Merged
                }
                Some((pending_task, merged)) => {
                    // 换 task：先冲刷旧 task 的挂起帧，再为本 task 开新帧。
                    self.pending = Some((task_id.to_string(), text.clone()));
                    CoalesceStep::FlushThenEmit {
                        flushed_task_id: pending_task,
                        flushed_event: Box::new(AgentEvent::Message {
                            text: merged,
                            delta: true,
                        }),
                        task_id,
                        event,
                    }
                }
                None => {
                    self.pending = Some((task_id.to_string(), text.clone()));
                    CoalesceStep::Merged
                }
            };
        }
        // 非增量事件（封口帧/工具/状态/……）：先冲刷挂起帧再透传，顺序不变。
        match self.pending.take() {
            Some((pending_task, merged)) => CoalesceStep::FlushThenEmit {
                flushed_task_id: pending_task,
                flushed_event: Box::new(AgentEvent::Message {
                    text: merged,
                    delta: true,
                }),
                task_id,
                event,
            },
            None => CoalesceStep::Emit { task_id, event },
        }
    }

    /// 终局冲刷（sink 卸载/退出）。无挂起帧时返回 None。
    pub fn flush(&mut self) -> Option<(String, AgentEvent)> {
        self.pending.take().map(|(task_id, merged)| {
            (
                task_id,
                AgentEvent::Message {
                    text: merged,
                    delta: true,
                },
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(text: &str) -> AgentEvent {
        AgentEvent::Message {
            text: text.to_string(),
            delta: true,
        }
    }

    fn seal(text: &str) -> AgentEvent {
        AgentEvent::Message {
            text: text.to_string(),
            delta: false,
        }
    }

    #[test]
    fn consecutive_deltas_for_the_same_task_merge_into_one_frame() {
        let mut coalescer = DeltaCoalescer::default();
        assert!(matches!(
            coalescer.step("t1", &delta("Hel")),
            CoalesceStep::Merged
        ));
        assert!(matches!(
            coalescer.step("t1", &delta("lo ")),
            CoalesceStep::Merged
        ));
        assert!(matches!(
            coalescer.step("t1", &delta("world")),
            CoalesceStep::Merged
        ));
        let flushed = coalescer.flush().expect("merged frame pending");
        assert_eq!(flushed.0, "t1");
        assert_eq!(
            flushed.1,
            AgentEvent::Message {
                text: "Hello world".to_string(),
                delta: true,
            }
        );
    }

    #[test]
    fn non_delta_events_flush_the_pending_frame_first_in_order() {
        let mut coalescer = DeltaCoalescer::default();
        assert!(matches!(
            coalescer.step("t1", &delta("part-")),
            CoalesceStep::Merged
        ));
        match coalescer.step("t1", &seal("final")) {
            CoalesceStep::FlushThenEmit {
                flushed_event,
                event,
                ..
            } => {
                assert_eq!(
                    *flushed_event,
                    AgentEvent::Message {
                        text: "part-".to_string(),
                        delta: true,
                    }
                );
                assert_eq!(event, &seal("final"));
            }
            other => panic!("expected flush-then-emit, got {other:?}"),
        }
        assert!(coalescer.flush().is_none(), "nothing left pending");
    }

    #[test]
    fn switching_tasks_flushes_the_previous_task_frame() {
        let mut coalescer = DeltaCoalescer::default();
        assert!(matches!(
            coalescer.step("t1", &delta("a")),
            CoalesceStep::Merged
        ));
        match coalescer.step("t2", &delta("b")) {
            CoalesceStep::FlushThenEmit {
                flushed_task_id,
                flushed_event,
                task_id,
                ..
            } => {
                assert_eq!(flushed_task_id, "t1");
                assert_eq!(*flushed_event, delta("a"));
                assert_eq!(task_id, "t2");
            }
            other => panic!("expected flush-then-emit, got {other:?}"),
        }
        let flushed = coalescer.flush().expect("t2 frame pending");
        assert_eq!((flushed.0.as_str(), flushed.1), ("t2", delta("b")));
    }

    #[test]
    fn non_delta_events_without_pending_pass_through_borrowed() {
        let mut coalescer = DeltaCoalescer::default();
        let event = AgentEvent::State {
            state: r_code_core::dto::TaskState::Idle,
        };
        match coalescer.step("t1", &event) {
            CoalesceStep::Emit { task_id, event } => {
                assert_eq!(task_id, "t1");
                assert_eq!(
                    event,
                    &AgentEvent::State {
                        state: r_code_core::dto::TaskState::Idle,
                    }
                );
            }
            other => panic!("expected emit, got {other:?}"),
        }
    }
}
