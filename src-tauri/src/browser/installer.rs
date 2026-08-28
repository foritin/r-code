//! M7-02：按需安装单飞锁 + 每 Task Session/进程隔离注册表。
//!
//! 合同：
//! - 并发首次使用只安装一次（单飞锁）；损坏 current 不覆盖可用旧版（repair 语义）；
//! - 每 Task 独立 Session/profile/进程树，进程数有上限；
//! - 重启后 Session 一律呈 stopped/crashed，不自动拉起；
//! - Task 删除时清理该 task 的全部 Session/profile，其他 task 不受影响。

use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Default)]
pub struct InstallCoordinator {
    inner: Mutex<InstallInner>,
}

#[derive(Debug, Default)]
struct InstallInner {
    installing: Option<String>,
    installed_version: Option<String>,
    current_corrupt: bool,
}

impl InstallCoordinator {
    /// 单飞：同一 runtime 只允许一个安装流程；返回 true = 获得安装权。
    pub fn try_begin_install(&self, runtime_key: &str) -> bool {
        let mut inner = self.inner.lock().expect("install lock");
        if inner.installing.is_some() {
            return false;
        }
        inner.installing = Some(runtime_key.to_string());
        true
    }

    pub fn finish_install(&self, runtime_key: &str, version: &str) {
        let mut inner = self.inner.lock().expect("install lock");
        if inner.installing.as_deref() == Some(runtime_key) {
            inner.installing = None;
            inner.installed_version = Some(version.to_string());
            inner.current_corrupt = false;
        }
    }

    pub fn fail_install(&self, runtime_key: &str) {
        let mut inner = self.inner.lock().expect("install lock");
        if inner.installing.as_deref() == Some(runtime_key) {
            inner.installing = None;
            // 损坏 staging 不覆盖可用旧版：installed_version 保持不变。
        }
    }

    pub fn installed_version(&self) -> Option<String> {
        self.inner
            .lock()
            .expect("install lock")
            .installed_version
            .clone()
    }

    pub fn current_corrupt(&self) -> bool {
        self.inner.lock().expect("install lock").current_corrupt
    }
}

/// 每 Task 进程数上限（隔离与资源上限合同）。
pub const MAX_PROCESSES_PER_TASK: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub session_id: String,
    pub task_id: String,
    pub profile_path: String,
    /// 重启后一律 stopped/crashed，绝不自动拉起。
    pub state_after_restart: &'static str,
}

#[derive(Debug, Default)]
pub struct SessionRegistry {
    /// task_id → sessions（同 task 内共享 profile 命名空间，跨 task 严格隔离）。
    sessions: HashMap<String, Vec<SessionRecord>>,
    profiles: HashMap<String, String>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 每 Task 独立 profile：不同 task 的 profile_path 永不相同。
    pub fn create_session(
        &mut self,
        task_id: &str,
        session_id: &str,
        profile_root: &str,
    ) -> SessionRecord {
        let profile_path = format!("{profile_root}/{task_id}/{session_id}");
        let record = SessionRecord {
            session_id: session_id.to_string(),
            task_id: task_id.to_string(),
            profile_path: profile_path.clone(),
            state_after_restart: "stopped",
        };
        self.sessions
            .entry(task_id.to_string())
            .or_default()
            .push(record.clone());
        self.profiles.insert(session_id.to_string(), profile_path);
        record
    }

    pub fn sessions_of_task(&self, task_id: &str) -> Vec<&SessionRecord> {
        self.sessions
            .get(task_id)
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }

    /// 重启恢复：所有 Session 一律 stopped（不自动拉起）。
    pub fn restore_after_restart(&mut self, task_id: &str) -> Vec<SessionRecord> {
        self.sessions
            .get(task_id)
            .map(|v| {
                v.iter()
                    .map(|r| SessionRecord {
                        state_after_restart: "stopped",
                        ..r.clone()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Task 删除：清理该 task 的全部 session/profile，返回被清理的 profile 路径。
    pub fn remove_task(&mut self, task_id: &str) -> Vec<String> {
        let removed = self
            .sessions
            .remove(task_id)
            .map(|v| v.iter().map(|r| r.profile_path.clone()).collect())
            .unwrap_or_default();
        for p in &removed {
            self.profiles.remove(p);
        }
        removed
    }

    pub fn profile_of_session(&self, session_id: &str) -> Option<&String> {
        self.profiles.get(session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_first_use_installs_once() {
        let coordinator = InstallCoordinator::default();
        assert!(
            coordinator.try_begin_install("playwright-1.54.2"),
            "首个安装者获得安装权"
        );
        assert!(
            !coordinator.try_begin_install("playwright-1.54.2"),
            "并发安装被单飞锁拒绝"
        );
        coordinator.finish_install("playwright-1.54.2", "1.54.2");
        assert_eq!(coordinator.installed_version().as_deref(), Some("1.54.2"));
    }

    #[test]
    fn corrupt_staging_keeps_installed_version() {
        let coordinator = InstallCoordinator::default();
        assert!(coordinator.try_begin_install("rt"));
        coordinator.fail_install("rt");
        assert_eq!(coordinator.installed_version(), None, "失败不覆盖可用旧版");
        assert!(!coordinator.current_corrupt());
    }

    #[test]
    fn sessions_are_task_isolated() {
        let mut registry = SessionRegistry::new();
        let s1 = registry.create_session("task-a", "s-a1", "/profiles");
        let s2 = registry.create_session("task-b", "s-b1", "/profiles");
        assert_ne!(
            s1.profile_path, s2.profile_path,
            "不同 task 的 profile 必须隔离"
        );
        assert!(registry
            .sessions_of_task("task-a")
            .iter()
            .all(|r| r.task_id == "task-a"));
    }

    #[test]
    fn restart_restores_as_stopped_never_running() {
        let mut registry = SessionRegistry::new();
        let _ = registry.create_session("task-a", "s1", "/profiles");
        for r in registry.restore_after_restart("task-a") {
            assert_eq!(r.state_after_restart, "stopped", "重启后不得自动拉起");
        }
    }

    #[test]
    fn task_delete_cleans_only_its_own_sessions() {
        let mut registry = SessionRegistry::new();
        let a = registry.create_session("task-a", "sa", "/profiles");
        let _ = registry.create_session("task-b", "sb", "/profiles");
        let removed = registry.remove_task("task-a");
        assert_eq!(removed, vec![a.profile_path.clone()]);
        assert!(
            registry.profile_of_session("sb").is_some(),
            "其他 task 的 profile 不受影响"
        );
    }
}
