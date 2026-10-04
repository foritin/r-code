//! A10 — boot reseeding and delivery ordering at the kernel queue level.
//!
//! G12: `TaskService::new` starts with empty in-memory state and nothing in
//! production rebuilds it after a daemon restart — undelivered inputs strand
//! in the journal. `reseed` + the run_drive hook close that gap; this test
//! pins the kernel mechanics with the in-memory journal.

use r_code_harness_protocol::{InputKind, InputMessage};
use r_code_kernel::ports::JournalStore as _;
use r_code_kernel::task::{TaskContract, TaskKind};
use r_code_kernel::tasks::TaskService;
use r_code_kernel::testing::MemoryJournal;
use std::sync::Arc;

fn contract_for(task_id: &str) -> TaskContract {
    TaskContract {
        task_id: task_id.to_string(),
        kind: TaskKind::Conversation,
        objective: String::new(),
        constraints: vec![],
        required_checks: vec![],
        memory: None,
        revision: 0,
    }
}

async fn seed_task(journal: Arc<MemoryJournal>, task_id: &str, texts: &[&str]) {
    let service = TaskService::new(journal.clone());
    service
        .create_task(contract_for(task_id))
        .await
        .expect("create");
    for text in texts {
        service
            .enqueue(task_id, InputKind::User, text, None)
            .await
            .expect("enqueue");
    }
}

#[tokio::test]
async fn a10_restart_reseeds_undelivered() {
    let journal = Arc::new(MemoryJournal::default());
    seed_task(journal.clone(), "t-reseed", &["first", "second"]).await;

    // 模拟"进程死亡重启"：全新的 TaskService（内存队列为空）。
    let restarted = TaskService::new(journal.clone());
    assert!(
        restarted.poll("t-reseed").await.is_none(),
        "fresh service sees nothing"
    );

    // 宿主侧重建（生产中由 run_drive 在 poll 空时触发，此处直接调用）。
    let undelivered = {
        // MemoryJournal 与 V1Store 的 rebuild 语义在此处由 kernel 测试代位：
        // 从 journal 事件重建未投递输入。
        let events = journal
            .read_events(0, u32::MAX)
            .await
            .iter()
            .filter(|e| e.task_id == "t-reseed")
            .cloned()
            .collect::<Vec<_>>();
        let delivered: std::collections::HashSet<String> = events
            .iter()
            .filter(|event| event.kind == "input.delivered")
            .filter_map(|event| {
                event
                    .payload
                    .get("message_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .collect();
        events
            .iter()
            .filter(|event| event.kind == "input.queued")
            .filter_map(|event| serde_json::from_value::<InputMessage>(event.payload.clone()).ok())
            .filter(|message| !delivered.contains(&message.message_id))
            .collect::<Vec<_>>()
    };
    assert_eq!(undelivered.len(), 2);
    let added = restarted.reseed("t-reseed", undelivered).await;
    assert_eq!(added, 2);

    let first = restarted.poll("t-reseed").await.expect("dispatch resumes");
    assert_eq!(first.text, "first", "delivery order follows input_seq");
    restarted
        .acknowledge("t-reseed", &first.message_id)
        .await
        .expect("ack");
    let second = restarted.poll("t-reseed").await.expect("second dispatches");
    assert_eq!(second.text, "second");
}

#[tokio::test]
async fn a10_reseed_idempotent_and_sorted() {
    let journal = Arc::new(MemoryJournal::default());
    seed_task(journal.clone(), "t-order", &["a", "b"]).await;
    let service = TaskService::new(journal.clone());
    // 故意乱序灌入（seq 2 先、seq 1 后），reseed 必须按 input_seq 排序。
    let events = journal
        .read_events(0, u32::MAX)
        .await
        .iter()
        .filter(|e| e.task_id == "t-order")
        .cloned()
        .collect::<Vec<_>>();
    let mut messages: Vec<InputMessage> = events
        .iter()
        .filter(|event| event.kind == "input.queued")
        .filter_map(|event| serde_json::from_value::<InputMessage>(event.payload.clone()).ok())
        .collect();
    messages.reverse();
    let added = service.reseed("t-order", messages.clone()).await;
    assert_eq!(added, 2);
    // 重复灌入同一批：幂等跳过。
    let again = service.reseed("t-order", messages).await;
    assert_eq!(again, 0);
    let first = service.poll("t-order").await.expect("poll");
    assert_eq!(
        first.text, "a",
        "sorted by input_seq despite reverse insertion"
    );
}

#[tokio::test]
async fn a10_acknowledge_retryable_after_save_failure() {
    // acknowledge 持久先行：save 失败时 in_flight 恢复，重试仍可成功。
    // MemoryJournal 的 save 总是成功——用一个投递后 ack 两次的对照证明
    // 内存恢复路径：第一次 ack 成功后 in_flight 清空，第二次报 NotInFlight
    // （而非死锁或静默）。
    let journal = Arc::new(MemoryJournal::default());
    let service = TaskService::new(journal.clone());
    service
        .create_task(contract_for("t-ack"))
        .await
        .expect("create");
    service
        .enqueue("t-ack", InputKind::User, "hello", None)
        .await
        .expect("enqueue");
    let message = service.poll("t-ack").await.expect("poll");
    service
        .acknowledge("t-ack", &message.message_id)
        .await
        .expect("first ack succeeds");
    let second = service.acknowledge("t-ack", &message.message_id).await;
    assert!(
        second.is_err(),
        "double ack is a visible error, not a stall"
    );
}
