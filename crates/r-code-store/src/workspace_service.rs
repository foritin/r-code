//! Workspace 服务层：管理 workspace 的打开/关闭/最近列表/信任状态。
//!
//! 封装 [`WorkspaceRepository`]，提供更高层的工作流语义：
//! - `open`：upsert + touch，并保留既有信任状态
//! - `close`：仅 touch `last_opened_at`
//! - `list_recent` / `set_trust` / `get`
//!
//! [doc-06 §3.8] [doc-13]

use chrono::Utc;
use r_code_core::dto::{TrustState, Workspace};
use r_code_core::error::ProductError;

use crate::repositories::WorkspaceRepository;
use crate::Database;

/// Workspace 服务 -- 管理 workspace open/close/recent/trust。
pub struct WorkspaceService<'a> {
    repo: WorkspaceRepository<'a>,
}

impl<'a> WorkspaceService<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self {
            repo: WorkspaceRepository::new(db),
        }
    }

    /// 打开 workspace（upsert + touch）。
    ///
    /// 若 workspace 已存在，保留其既有信任状态；否则默认 `Untrusted`。
    /// `display_name` 以本次传入为准。`last_opened_at` 更新为当前时间。
    pub fn open(
        &self,
        canonical_path: &str,
        display_name: &str,
    ) -> Result<Workspace, ProductError> {
        let trust = self
            .repo
            .get(canonical_path)?
            .map(|w| w.trust_state)
            .unwrap_or_default();
        let ws = Workspace {
            canonical_path: canonical_path.to_string(),
            display_name: display_name.to_string(),
            trust_state: trust,
            last_opened_at: Utc::now(),
        };
        self.repo.upsert(&ws)?;
        // upsert 已写入 last_opened_at = now，touch 再次刷新以显式表达“open”语义
        self.repo.touch(canonical_path)?;
        Ok(ws)
    }

    /// 关闭 workspace（仅 touch `last_opened_at`）。
    pub fn close(&self, canonical_path: &str) -> Result<(), ProductError> {
        self.repo.touch(canonical_path)
    }

    /// 列出最近打开的 workspace（按 `last_opened_at` 降序）。
    pub fn list_recent(&self, limit: u32) -> Result<Vec<Workspace>, ProductError> {
        self.repo.list_recent(limit)
    }

    /// 设置信任状态。
    pub fn set_trust(&self, canonical_path: &str, trust: TrustState) -> Result<(), ProductError> {
        self.repo.update_trust(canonical_path, trust)
    }

    /// 按 canonical path 获取 workspace。
    pub fn get(&self, canonical_path: &str) -> Result<Option<Workspace>, ProductError> {
        self.repo.get(canonical_path)
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_core::dto::TrustState;
    use tempfile::TempDir;

    fn setup() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn test_open_new_workspace() {
        let db = setup();
        let svc = WorkspaceService::new(&db);

        let ws = svc.open("/home/user/proj", "My Project").unwrap();
        assert_eq!(ws.canonical_path, "/home/user/proj");
        assert_eq!(ws.display_name, "My Project");
        assert_eq!(ws.trust_state, TrustState::Untrusted);

        // 已持久化
        let fetched = svc.get("/home/user/proj").unwrap().unwrap();
        assert_eq!(fetched.canonical_path, "/home/user/proj");
        assert_eq!(fetched.display_name, "My Project");
        assert_eq!(fetched.trust_state, TrustState::Untrusted);
    }

    #[test]
    fn test_open_preserves_trust_state() {
        let db = setup();
        let svc = WorkspaceService::new(&db);

        svc.open("/proj", "P").unwrap();
        svc.set_trust("/proj", TrustState::Trusted).unwrap();

        // 重新打开 -> 信任状态应保留
        let ws = svc.open("/proj", "P").unwrap();
        assert_eq!(ws.trust_state, TrustState::Trusted);

        let fetched = svc.get("/proj").unwrap().unwrap();
        assert_eq!(fetched.trust_state, TrustState::Trusted);
    }

    #[test]
    fn test_open_updates_display_name() {
        let db = setup();
        let svc = WorkspaceService::new(&db);

        svc.open("/proj", "Old Name").unwrap();
        svc.open("/proj", "New Name").unwrap();

        let fetched = svc.get("/proj").unwrap().unwrap();
        assert_eq!(fetched.display_name, "New Name");
    }

    #[test]
    fn test_open_touches_last_opened_at() {
        let db = setup();
        let svc = WorkspaceService::new(&db);

        let first = svc.open("/proj", "P").unwrap();
        // 再次打开 -> last_opened_at 不应早于首次
        let second = svc.open("/proj", "P").unwrap();
        assert!(second.last_opened_at >= first.last_opened_at);

        let fetched = svc.get("/proj").unwrap().unwrap();
        assert!(fetched.last_opened_at >= first.last_opened_at);
    }

    #[test]
    fn test_close_touches_last_opened_at() {
        let db = setup();
        let svc = WorkspaceService::new(&db);

        let opened = svc.open("/proj", "P").unwrap();
        svc.close("/proj").unwrap();

        let fetched = svc.get("/proj").unwrap().unwrap();
        assert!(fetched.last_opened_at >= opened.last_opened_at);
    }

    #[test]
    fn test_close_unknown_workspace_no_error() {
        let db = setup();
        let svc = WorkspaceService::new(&db);
        // 关闭不存在的 workspace -> touch 影响 0 行，不报错
        svc.close("/never-opened").unwrap();
    }

    #[test]
    fn test_get_not_found() {
        let db = setup();
        let svc = WorkspaceService::new(&db);
        assert!(svc.get("/nope").unwrap().is_none());
    }

    #[test]
    fn test_set_trust() {
        let db = setup();
        let svc = WorkspaceService::new(&db);

        svc.open("/proj", "P").unwrap();
        assert_eq!(
            svc.get("/proj").unwrap().unwrap().trust_state,
            TrustState::Untrusted
        );

        svc.set_trust("/proj", TrustState::Trusted).unwrap();
        assert_eq!(
            svc.get("/proj").unwrap().unwrap().trust_state,
            TrustState::Trusted
        );

        svc.set_trust("/proj", TrustState::Untrusted).unwrap();
        assert_eq!(
            svc.get("/proj").unwrap().unwrap().trust_state,
            TrustState::Untrusted
        );
    }

    #[test]
    fn test_list_recent_order_and_limit() {
        let db = setup();
        let svc = WorkspaceService::new(&db);

        svc.open("/proj1", "P1").unwrap();
        svc.open("/proj2", "P2").unwrap();
        svc.open("/proj3", "P3").unwrap();

        let recent = svc.list_recent(10).unwrap();
        assert_eq!(recent.len(), 3);

        // 最后打开的 /proj3 应排在最前
        assert_eq!(recent[0].canonical_path, "/proj3");

        // limit 生效
        let limited = svc.list_recent(2).unwrap();
        assert_eq!(limited.len(), 2);
    }

    #[test]
    fn test_list_recent_reflects_close_touch() {
        let db = setup();
        let svc = WorkspaceService::new(&db);

        svc.open("/old", "Old").unwrap();
        svc.open("/new", "New").unwrap();
        // 关闭 /old 使其 last_opened_at 更新为最近
        svc.close("/old").unwrap();

        let recent = svc.list_recent(10).unwrap();
        assert_eq!(recent[0].canonical_path, "/old");
    }

    #[test]
    fn test_reopen_does_not_duplicate() {
        let db = setup();
        let svc = WorkspaceService::new(&db);

        svc.open("/proj", "P").unwrap();
        svc.open("/proj", "P").unwrap();
        svc.open("/proj", "P").unwrap();

        let recent = svc.list_recent(10).unwrap();
        assert_eq!(recent.len(), 1);
    }

    #[test]
    fn test_open_with_real_canonical_path() {
        // 使用真实临时目录路径，模拟实际 workspace 打开流程
        let tmp = TempDir::new().unwrap();
        let canonical = tmp.path().to_string_lossy().to_string();
        let db = setup();
        let svc = WorkspaceService::new(&db);

        let ws = svc.open(&canonical, "Tmp Workspace").unwrap();
        assert_eq!(ws.canonical_path, canonical);

        svc.set_trust(&canonical, TrustState::Trusted).unwrap();
        let fetched = svc.get(&canonical).unwrap().unwrap();
        assert_eq!(fetched.trust_state, TrustState::Trusted);
        assert_eq!(fetched.display_name, "Tmp Workspace");
    }
}
