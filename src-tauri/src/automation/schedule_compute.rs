//! M8-01：Automation 调度核心（once/hourly/daily/weekdays/weekly）。
//!
//! 设计：调度以 UTC 计算并接受调用方提供的「当前 IANA 时区有效偏移（分钟）」；
//! DST 切换由 tz 数据提供方（前端 Intl / chrono-tz）负责，本层保持纯函数可测。
//! 幂等：相同 idempotency key 只产生一个 Run（lease 过期后可恢复）。

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recurrence {
    Once,
    Hourly,
    Daily,
    Weekdays,
    Weekly,
}

/// 墙钟字段（时区本地化后参与计算）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WallClock {
    pub minute: u32,
    pub hour: u32,
    pub weekday: u32, // 0=Sun..6=Sat
}

/// A1/A4：给定 UTC 起点（已加 tz 偏移的墙钟比较由调用方完成）、
/// 返回下一次触发的 UTC 分钟数；once 只触发一次（随后 Done）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleOutcome {
    RunAt(u64),
    Done,
}

pub fn next_run_minutes(
    recurrence: &Recurrence,
    from_minute: u64,
    at_minute_of_day: u32,
    at_weekday: u32,
) -> ScheduleOutcome {
    const DAY: u64 = 24 * 60;
    let target_of_day = (at_minute_of_day as u64) % DAY;
    match recurrence {
        Recurrence::Once => ScheduleOutcome::Done,
        Recurrence::Hourly => ScheduleOutcome::RunAt(from_minute + 60 - (from_minute % 60)),
        Recurrence::Daily => {
            let day_start = (from_minute / DAY) * DAY;
            let today = day_start + target_of_day;
            if today > from_minute {
                ScheduleOutcome::RunAt(today)
            } else {
                ScheduleOutcome::RunAt(today + DAY)
            }
        }
        Recurrence::Weekdays => {
            for m in (from_minute + 1)..=(from_minute + 7 * DAY) {
                let weekday = ((m / DAY) as u32 + at_weekday) % 7; // 相位由调用方锚定
                let is_weekend = matches!(weekday, 0 | 6);
                let at_time = m % DAY == target_of_day;
                if !is_weekend && at_time {
                    return ScheduleOutcome::RunAt(m);
                }
            }
            ScheduleOutcome::Done
        }
        Recurrence::Weekly => {
            for m in (from_minute + 1)..=(from_minute + 7 * DAY) {
                let weekday = ((m / DAY) as u32 + at_weekday) % 7;
                let at_time = m % DAY == target_of_day;
                if weekday == at_weekday && at_time {
                    return ScheduleOutcome::RunAt(m);
                }
            }
            ScheduleOutcome::Done
        }
    }
}

/// A2：幂等账本——相同 key 只产生一个 Run；lease 过期后可恢复。
#[derive(Debug, Default)]
pub struct IdempotencyLedger {
    runs: HashMap<String, String>,
    leases: HashMap<String, u64>, // key → lease_until(ms)
}

impl IdempotencyLedger {
    pub fn try_acquire_run(&mut self, key: &str, run_id: &str, now_ms: u64, lease_ms: u64) -> bool {
        if let Some(until) = self.leases.get(key) {
            if *until > now_ms {
                return false; // 未过期且已有 Run
            }
            // 租约过期：允许恢复性重跑（新 run_id 接替）。
        }
        self.runs.insert(key.to_string(), run_id.to_string());
        self.leases.insert(key.to_string(), now_ms + lease_ms);
        true
    }

    pub fn release_lease(&mut self, key: &str) {
        self.leases.remove(key);
    }

    pub fn run_id_of(&self, key: &str) -> Option<&String> {
        self.runs.get(key)
    }
}

/// A4：重启恢复只补最新一次遗漏，其余以 skipped 聚合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatchUp {
    pub run_missed_occurrence: Option<u64>,
    pub skipped_count: u32,
}

pub fn plan_recovery(missed: &[u64]) -> CatchUp {
    let mut sorted = missed.to_vec();
    sorted.sort_unstable();
    // 重启只补最新一次遗漏，其余以 skipped 聚合。
    CatchUp {
        run_missed_occurrence: sorted.last().copied(),
        skipped_count: (sorted.len().saturating_sub(1)) as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m8_01_a1_schedule_goldens() {
        // once：已过即 Done（不重复）
        assert_eq!(
            next_run_minutes(&Recurrence::Once, 500, 30, 1),
            ScheduleOutcome::Done
        );
        // hourly：对齐到下一整点
        assert_eq!(
            next_run_minutes(&Recurrence::Hourly, 500, 30, 1),
            ScheduleOutcome::RunAt(540)
        );
        // daily：今天未到取今天，已过取明天
        // 今天 0:30 已过（当前 500 = 8:20），下一次是明天 0:30
        assert_eq!(
            next_run_minutes(&Recurrence::Daily, 500, 30, 1),
            ScheduleOutcome::RunAt(1470)
        );
    }

    #[test]
    fn m8_01_a4_recovery_only_latest_runs_and_others_skipped() {
        let catch_up = plan_recovery(&[900, 950, 990]);
        assert_eq!(catch_up.run_missed_occurrence, Some(990));
        assert_eq!(catch_up.skipped_count, 2);
        assert_eq!(plan_recovery(&[]).run_missed_occurrence, None);
    }

    #[test]
    fn m8_02_a2_idempotency_key_single_run_and_lease_expiry_recovery() {
        let mut ledger = IdempotencyLedger::default();
        assert!(
            ledger.try_acquire_run("def-1", "run-1", 1000, 120_000),
            "首次获得租约"
        );
        assert!(
            !ledger.try_acquire_run("def-1", "run-2", 2000, 120_000),
            "租约未过期不得二次执行"
        );
        assert_eq!(ledger.run_id_of("def-1"), Some(&"run-1".to_string()));
        ledger.release_lease("def-1");
        assert!(
            ledger.try_acquire_run("def-1", "run-3", 2500, 120_000),
            "lease 释放后可恢复"
        );
    }
}
