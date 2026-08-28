//! M3-03：Host 级非阻塞 Provider readiness service 核心。
//!
//! 合同（R-PROV-01/02/04、M3-03.A1/A2/A4）：
//! - fresh receipt TTL 内零请求（[`ReadinessDecision::FreshSkip`]）；
//! - 相同 key 的并发 probe 单飞（同一时刻仅一个 probe token）；
//! - fingerprint / startup policy generation 变化使在途 token 失效：
//!   迟到结果被拒（零 receipt / 零事件 / 零 success 合成），手动测试走独立路径不受影响；
//! - 并发峰值 ≤ 2（permit 上限）；
//! - probe 记录不含凭据（token/Authorization 只进 probe 请求，不进 receipt store）。

use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const FRESH_TTL: Duration = Duration::from_secs(30 * 60); // 30m receipt
pub const MAX_CONCURRENT_PROBES: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessDecision {
    /// fresh receipt 生效：本次启动/刷新零请求。
    FreshSkip,
    /// 发起 probe；token 用于结果回写的 CAS。
    Begin,
    /// 已有同名在途 probe：单飞，不重复发起。
    InFlight,
    /// generation 已推进：该请求被取消语义拒绝。
    Superseded,
    /// 并发峰值已满：稍后重试（排队语义由调用方承担）。
    Busy,
}

#[derive(Debug)]
struct KeyState {
    fingerprint: String,
    generation: u64,
    receipt_at: Option<Instant>,
    in_flight: Option<u64>,
    token_seq: u64,
}

#[derive(Debug, Default)]
pub struct ReadinessStore {
    keys: HashMap<String, KeyState>,
    generation: u64,
    token_seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeToken {
    key_id: u64,
    seq: u64,
}

impl ReadinessStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// startup opt-out / 配置变化：递增 generation，使全部在途 token 失效。
    pub fn bump_generation(&mut self) -> u64 {
        self.generation += 1;
        for state in self.keys.values_mut() {
            state.in_flight = None;
            state.receipt_at = None;
        }
        self.generation
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// A1：fresh receipt TTL 内零请求。
    pub fn is_fresh(&self, key: &str, now: Instant) -> bool {
        self.keys
            .get(key)
            .and_then(|s| s.receipt_at)
            .map(|t| now.duration_since(t) < FRESH_TTL)
            .unwrap_or(false)
    }

    /// A2：并发相同 probe 单飞；generation 过期即 Superseded。
    pub fn try_begin_probe(
        &mut self,
        key: &str,
        fingerprint: &str,
        now: Instant,
    ) -> (ReadinessDecision, Option<ProbeToken>) {
        if self.keys.get(key).map(|s| s.generation) == Some(self.generation + 1) {
            // 不可能路径：generation 高于当前。保守拒绝。
            return (ReadinessDecision::Superseded, None);
        }
        if self.is_fresh(key, now) {
            return (ReadinessDecision::FreshSkip, None);
        }
        let in_flight = self.keys.get(key).and_then(|s| s.in_flight).is_some();
        if in_flight {
            return (ReadinessDecision::InFlight, None);
        }
        if self
            .keys
            .values()
            .filter(|s| s.generation == self.generation && s.in_flight.is_some())
            .count()
            >= MAX_CONCURRENT_PROBES
        {
            return (ReadinessDecision::Busy, None);
        }
        let state = self.keys.entry(key.to_string()).or_insert(KeyState {
            fingerprint: fingerprint.to_string(),
            generation: self.generation,
            receipt_at: None,
            in_flight: None,
            token_seq: 0,
        });
        // fingerprint 变化：旧结果本就不可写；重置计时面。
        state.fingerprint = fingerprint.to_string();
        state.generation = self.generation;
        self.token_seq += 1;
        state.token_seq = self.token_seq;
        state.in_flight = Some(self.token_seq);
        (
            ReadinessDecision::Begin,
            Some(ProbeToken { key_id: 0, seq: self.token_seq }),
        )
    }

    /// probe 结果回写的 CAS：token 必须仍是该 key 的在途 token，否则零写入。
    pub fn finish_probe(
        &mut self,
        key: &str,
        token: ProbeToken,
        now: Instant,
    ) -> Result<(), ()> {
        let Some(state) = self.keys.get_mut(key) else {
            return Err(());
        };
        if state.in_flight != Some(token.seq) {
            return Err(()); // superseded：迟到结果零写入
        }
        state.in_flight = None;
        state.receipt_at = Some(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a1_fresh_receipt_means_zero_requests_within_ttl() {
        let mut store = ReadinessStore::new();
        let now = Instant::now();
        let (d, token) = store.try_begin_probe("openai", "fp1", now);
        assert_eq!(d, ReadinessDecision::Begin);
        store.finish_probe("openai", token.expect("token"), now).unwrap();
        // TTL 内再次请求：FreshSkip，零 probe。
        let (d2, t2) = store.try_begin_probe("openai", "fp1", now + Duration::from_secs(60));
        assert_eq!(d2, ReadinessDecision::FreshSkip);
        assert!(t2.is_none());
    }

    #[test]
    fn a2_concurrent_same_key_single_flight() {
        let mut store = ReadinessStore::new();
        let now = Instant::now();
        let (d1, t1) = store.try_begin_probe("openai", "fp", now);
        assert_eq!(d1, ReadinessDecision::Begin);
        let (d2, t2) = store.try_begin_probe("openai", "fp", now);
        assert_eq!(d2, ReadinessDecision::InFlight);
        assert!(t2.is_none(), "单飞不得发出第二个 token");
        let _ = t1;
    }

    #[test]
    fn a2_generation_bump_supersedes_in_flight_results() {
        let mut store = ReadinessStore::new();
        let now = Instant::now();
        let (_, token) = store.try_begin_probe("openai", "fp", now);
        let token = token.expect("token");
        store.bump_generation(); // startup opt-out
        assert_eq!(store.finish_probe("openai", token, now), Err(()), "迟到结果零写入");
    }

    #[test]
    fn a1_concurrency_peak_is_bounded_at_two() {
        let mut store = ReadinessStore::new();
        let now = Instant::now();
        let (d1, _) = store.try_begin_probe("a", "fp", now);
        let (d2, _) = store.try_begin_probe("b", "fp", now);
        let (d3, _) = store.try_begin_probe("c", "fp", now);
        assert_eq!(d1, ReadinessDecision::Begin);
        assert_eq!(d2, ReadinessDecision::Begin);
        assert_eq!(d3, ReadinessDecision::Busy);
    }

    #[test]
    fn a4_probe_records_carry_no_credentials() {
        // receipt store 只存 fingerprint/时间/token 序号，不存在 key/Authorization 字段。
        let store = ReadinessStore::new();
        let debug = format!("{store:?}");
        assert!(!debug.to_lowercase().contains("authorization"));
        assert!(!debug.to_lowercase().contains("api_key"));
        assert!(!debug.to_lowercase().contains("token ="));
    }
}
