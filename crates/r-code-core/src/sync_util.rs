//! std 同步原语的降级恢复工具。
//!
//! 产品约定（F-robust-07 / F-corr-02..04）：运行主路径上的 std Mutex 一律
//! 不因锁中毒而 panic——中毒只说明**某个持锁线程**曾 panic，不代表受保护
//! 数据不可用。可恢复的守卫（VecDeque、Option、HashMap、计数器等无跨语句
//! 不变量的容器）走 `into_inner` 恢复并记录 warn；只有跨语句不变量的结构
//! 才允许在中毒时显式失败。

/// 从锁中毒中恢复守卫（记录 warn 后 `into_inner`）。
///
/// 用法：`lock().unwrap_or_else(recover_poisoned_guard)`。
#[inline]
pub fn recover_poisoned_guard<T>(poisoned: std::sync::PoisonError<T>) -> T {
    tracing::warn!("std mutex poisoned; recovering guard to keep the run responsive");
    poisoned.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn recovered_guard_content_survives_the_poisoning_thread() {
        let mutex: &'static Mutex<Vec<u32>> = Box::leak(Box::new(Mutex::new(vec![7])));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = mutex.lock().unwrap();
            panic!("poison the lock while held");
        }));
        assert!(mutex.is_poisoned());
        let mut guard = mutex.lock().unwrap_or_else(recover_poisoned_guard);
        guard.push(9);
        assert_eq!(&*guard, &[7, 9]);
    }
}
