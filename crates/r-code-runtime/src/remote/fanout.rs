//! Event fan-out (R05, F8): authenticated connections subscribe with an
//! `after_seq` cursor and receive the *same* EventEnvelope projection the
//! local `task.events` method serves — seq-monotonic, no loss, no
//! duplication. Publishing happens only after persistence (the publisher
//! trails the journal cursor), and a slow consumer is disconnected instead
//! of ever stalling the journal.

use r_code_harness_protocol::EventEnvelope;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Per-connection channel bound: the backpressure threshold. Exceeding it
/// means the consumer cannot keep up — it is dropped, never the journal.
const SUBSCRIBER_CHANNEL_CAPACITY: usize = 1000;

/// Subscription registry.
pub struct FanoutHub {
    subscribers: Mutex<HashMap<u64, mpsc::Sender<EventEnvelope>>>,
    next_id: AtomicU64,
}

impl Default for FanoutHub {
    fn default() -> Self {
        Self {
            subscribers: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }
}

impl FanoutHub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Register a live subscription.
    pub fn subscribe(&self) -> (u64, mpsc::Receiver<EventEnvelope>) {
        let (sender, receiver) = mpsc::channel(SUBSCRIBER_CHANNEL_CAPACITY);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.subscribers.lock().expect("fanout").insert(id, sender);
        (id, receiver)
    }

    /// Unregister (disconnect or drop).
    pub fn unsubscribe(&self, id: u64) {
        self.subscribers.lock().expect("fanout").remove(&id);
    }

    /// Publish persisted events to every subscriber. Slow consumers (full
    /// channel) are unsubscribed on the spot; the others are unaffected.
    pub fn publish(&self, events: &[EventEnvelope]) {
        if events.is_empty() {
            return;
        }
        let mut slow = Vec::new();
        {
            let subscribers = self.subscribers.lock().expect("fanout");
            for (id, sender) in subscribers.iter() {
                for event in events {
                    match sender.try_send(event.clone()) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            slow.push(*id);
                            break;
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            slow.push(*id);
                            break;
                        }
                    }
                }
            }
        }
        for id in slow {
            self.unsubscribe(id);
        }
    }

    /// Live subscriber count (diagnostics/tests).
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.lock().expect("fanout").len()
    }
}

/// The journal-cursor publisher: polls durable events past `cursor` and
/// fans them out. Because it only ever publishes what `read_events`
/// returns, ordering (seq ascending) and the persisted-before-published
/// invariant fall out of the store itself (F8).
pub struct CursorPublisher {
    store: Arc<r_code_store::v1::V1Store>,
    hub: Arc<FanoutHub>,
    cursor: u64,
}

impl CursorPublisher {
    pub fn new(store: Arc<r_code_store::v1::V1Store>, hub: Arc<FanoutHub>) -> Self {
        Self {
            store,
            hub,
            cursor: 0,
        }
    }

    /// One poll step (exposed for tests); returns the events published.
    pub async fn poll_once(&mut self) -> Vec<EventEnvelope> {
        use r_code_kernel::ports::JournalStore as _;
        let events = self.store.read_events(self.cursor, 200).await;
        if events.is_empty() {
            return Vec::new();
        }
        self.cursor = events.last().map(|event| event.seq).unwrap_or(self.cursor);
        let envelopes: Vec<EventEnvelope> = events
            .into_iter()
            .map(crate::run_manager::envelope_of)
            .collect();
        self.hub.publish(&envelopes);
        envelopes
    }

    /// Run forever at the poll cadence.
    pub async fn run(mut self, interval: std::time::Duration) {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            self.poll_once().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(seq: u64) -> EventEnvelope {
        EventEnvelope {
            seq,
            task_id: "t1".into(),
            run_id: "run-t1".into(),
            kind: r_code_harness_protocol::EventKind::Progress,
            source: r_code_harness_protocol::Provenance::Host,
            payload: serde_json::json!({"seq": seq}),
        }
    }

    #[tokio::test]
    async fn r05_a1_subscribers_receive_events_in_seq_order() {
        let hub = FanoutHub::new();
        let (_id, mut rx) = hub.subscribe();
        hub.publish(&[envelope(1), envelope(2), envelope(3)]);
        let mut seen = Vec::new();
        for _ in 0..3 {
            seen.push(rx.recv().await.expect("event").seq);
        }
        assert_eq!(seen, vec![1, 2, 3]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn r05_a3_slow_consumers_are_dropped_without_touching_others() {
        let hub = FanoutHub::new();
        // A subscriber that never drains (its receiver lives but fills up).
        let (_slow_id, _slow_rx) = hub.subscribe();
        // A fast one that we drain inline, slice by slice — 不再依赖 spawned
        // drainer 的调度时机（重负载 runner 上它可能整个 burst 都没被调度，
        // fast 通道同样溢出被误丢，计数归零）。
        let (fast_id, mut fast_rx) = hub.subscribe();

        let burst_total = SUBSCRIBER_CHANNEL_CAPACITY as u64 + 5;
        for chunk_start in (0..burst_total).step_by(64) {
            let chunk: Vec<EventEnvelope> = (chunk_start..(chunk_start + 64).min(burst_total))
                .map(envelope)
                .collect();
            hub.publish(&chunk);
            while fast_rx.try_recv().is_ok() {}
        }

        // The slow one is gone; the fast one still receives fresh events.
        assert_eq!(hub.subscriber_count(), 1, "slow consumer dropped");
        hub.publish(&[envelope(9_999)]);
        while fast_rx.try_recv().is_ok() {}
        assert_eq!(hub.subscriber_count(), 1, "fast consumer stays live");
        hub.unsubscribe(fast_id);
    }
}
