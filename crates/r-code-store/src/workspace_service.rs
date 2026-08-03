//! Workspace 服务层：管理 workspace 的打开/关闭/最近列表/忘记/权限模式。
//!
//! 封装 [`WorkspaceRepository`]，提供更高层的工作流语义：
//! - `open`：upsert + touch，并保留既有项目权限模式
//! - `close`：仅 touch `last_opened_at`
//! - `list_recent` / `forget` / `set_access_mode` / `get`
//!
//! [doc-06 §3.8] [doc-13]

use chrono::Utc;
use r_code_core::dto::{ProjectAccessMode, Workspace, WorkspaceMemoryMode};
use r_code_core::error::ProductError;
use std::path::Path;
use uuid::Uuid;

use crate::repositories::WorkspaceRepository;
use crate::Database;

/// Workspace 服务 -- 管理 workspace open/close/recent/access mode。
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
    /// 若 workspace 已存在，保留其既有项目权限模式；否则默认 `RequestApproval`。
    /// `display_name` 以本次传入为准。`last_opened_at` 更新为当前时间。
    pub fn open(
        &self,
        canonical_path: &str,
        display_name: &str,
    ) -> Result<Workspace, ProductError> {
        let access_mode = self
            .repo
            .get(canonical_path)?
            .map(|w| w.access_mode)
            .unwrap_or_default();
        let candidate_id = Uuid::new_v4().simple().to_string();
        self.repo.upsert_and_get(
            &candidate_id,
            canonical_path,
            display_name,
            access_mode,
            Utc::now(),
        )
    }

    /// 关闭 workspace（仅 touch `last_opened_at`）。
    pub fn close(&self, canonical_path: &str) -> Result<(), ProductError> {
        self.repo.touch(canonical_path)
    }

    /// 列出最近打开的 workspace（按 `last_opened_at` 降序）。
    pub fn list_recent(&self, limit: u32) -> Result<Vec<Workspace>, ProductError> {
        self.repo.list_recent(limit)
    }

    /// 忘记 workspace，清除 R-Code 内部的项目、会话与关联审计记录。
    ///
    /// 此调用只操作产品数据库，不会读取、修改或删除 `canonical_path` 指向的任何
    /// 磁盘内容。返回 `(是否清除了项目, 清除的会话数)`。
    pub fn forget(
        &self,
        canonical_path: &str,
        blobs_dir: &Path,
        projection_root: &Path,
    ) -> Result<(bool, usize), ProductError> {
        self.repo.remove(canonical_path, blobs_dir, projection_root)
    }

    /// 设置项目级 Agent 权限模式。
    pub fn set_access_mode(
        &self,
        canonical_path: &str,
        access_mode: ProjectAccessMode,
    ) -> Result<(), ProductError> {
        self.repo.update_access_mode(canonical_path, access_mode)
    }

    /// 按 canonical path 获取 workspace。
    pub fn get(&self, canonical_path: &str) -> Result<Option<Workspace>, ProductError> {
        self.repo.get(canonical_path)
    }

    /// 按稳定 owner id 获取 workspace。
    pub fn get_by_id(&self, id: &str) -> Result<Option<Workspace>, ProductError> {
        self.repo.get_by_id(id)
    }

    /// 以 generation CAS 更新项目记忆模式。
    pub fn set_memory_mode(
        &self,
        id: &str,
        expected_generation: u64,
        memory_mode: WorkspaceMemoryMode,
    ) -> Result<u64, ProductError> {
        self.repo
            .update_memory_mode(id, expected_generation, memory_mode)
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_core::dto::{ProjectAccessMode, WorkspaceMemoryMode};
    use std::sync::{Arc, Barrier};
    use std::thread;
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
        assert_eq!(ws.access_mode, ProjectAccessMode::RequestApproval);

        // 已持久化
        let fetched = svc.get("/home/user/proj").unwrap().unwrap();
        assert_eq!(fetched.canonical_path, "/home/user/proj");
        assert_eq!(fetched.display_name, "My Project");
        assert_eq!(fetched.access_mode, ProjectAccessMode::RequestApproval);
    }

    #[test]
    fn test_open_preserves_access_mode() {
        let db = setup();
        let svc = WorkspaceService::new(&db);

        svc.open("/proj", "P").unwrap();
        svc.set_access_mode("/proj", ProjectAccessMode::FullAccess)
            .unwrap();

        // 重新打开 -> 项目权限模式应保留
        let ws = svc.open("/proj", "P").unwrap();
        assert_eq!(ws.access_mode, ProjectAccessMode::FullAccess);

        let fetched = svc.get("/proj").unwrap().unwrap();
        assert_eq!(fetched.access_mode, ProjectAccessMode::FullAccess);
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
    fn test_reopen_and_close_preserve_memory_owner_and_generation() {
        let db = setup();
        let svc = WorkspaceService::new(&db);

        let opened = svc.open("/memory-owner", "Original").unwrap();
        assert_eq!(
            svc.set_memory_mode(&opened.id, 1, WorkspaceMemoryMode::ReadOnly)
                .unwrap(),
            2
        );
        svc.close("/memory-owner").unwrap();
        let reopened = svc.open("/memory-owner", "Renamed").unwrap();

        assert_eq!(reopened.id, opened.id);
        assert_eq!(reopened.display_name, "Renamed");
        assert_eq!(reopened.memory_mode, WorkspaceMemoryMode::ReadOnly);
        assert_eq!(reopened.memory_generation, 2);
        assert_eq!(svc.get_by_id(&opened.id).unwrap(), Some(reopened));
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
    fn test_set_access_mode() {
        let db = setup();
        let svc = WorkspaceService::new(&db);

        svc.open("/proj", "P").unwrap();
        assert_eq!(
            svc.get("/proj").unwrap().unwrap().access_mode,
            ProjectAccessMode::RequestApproval
        );

        svc.set_access_mode("/proj", ProjectAccessMode::RiskBased)
            .unwrap();
        assert_eq!(
            svc.get("/proj").unwrap().unwrap().access_mode,
            ProjectAccessMode::RiskBased
        );

        svc.set_access_mode("/proj", ProjectAccessMode::FullAccess)
            .unwrap();
        assert_eq!(
            svc.get("/proj").unwrap().unwrap().access_mode,
            ProjectAccessMode::FullAccess
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
    fn test_canonical_path_case_is_not_normalized_by_storage() {
        let db = setup();
        let svc = WorkspaceService::new(&db);

        let upper = svc.open("/Repo/Case", "Upper").unwrap();
        let lower = svc.open("/repo/case", "Lower").unwrap();

        assert_ne!(upper.id, lower.id);
        assert_eq!(svc.list_recent(10).unwrap().len(), 2);
        assert_eq!(svc.get("/Repo/Case").unwrap().unwrap().id, upper.id);
        assert_eq!(svc.get("/repo/case").unwrap().unwrap().id, lower.id);
    }

    #[test]
    fn test_two_services_concurrently_open_one_persisted_owner() {
        let directory = TempDir::new().unwrap();
        let database_path = directory.path().join("concurrent-workspace.sqlite");
        let first_database = Database::open(&database_path).unwrap();
        let second_database = Database::open(&database_path).unwrap();
        let barrier = Arc::new(Barrier::new(2));

        let first_barrier = Arc::clone(&barrier);
        let first = thread::spawn(move || {
            let service = WorkspaceService::new(&first_database);
            first_barrier.wait();
            service.open("/shared/project", "First").unwrap()
        });
        let second_barrier = Arc::clone(&barrier);
        let second = thread::spawn(move || {
            let service = WorkspaceService::new(&second_database);
            second_barrier.wait();
            service.open("/shared/project", "Second").unwrap()
        });

        let first = first.join().unwrap();
        let second = second.join().unwrap();
        assert_eq!(first.id, second.id);

        let verification_database = Database::open(&database_path).unwrap();
        let service = WorkspaceService::new(&verification_database);
        let persisted = service.get("/shared/project").unwrap().unwrap();
        assert_eq!(persisted.id, first.id);
        assert_eq!(service.list_recent(10).unwrap().len(), 1);
    }

    #[test]
    fn test_forget_removes_recent_workspace_record() {
        let db = setup();
        let svc = WorkspaceService::new(&db);
        let app_data = TempDir::new().unwrap();
        let blobs = app_data.path().join("blobs");
        let plans = app_data.path().join("plans");

        svc.open("/proj", "P").unwrap();
        assert_eq!(svc.forget("/proj", &blobs, &plans).unwrap(), (true, 0));
        assert!(svc.get("/proj").unwrap().is_none());
        assert!(svc.list_recent(10).unwrap().is_empty());
        assert_eq!(svc.forget("/proj", &blobs, &plans).unwrap(), (false, 0));
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

        svc.set_access_mode(&canonical, ProjectAccessMode::RiskBased)
            .unwrap();
        let fetched = svc.get(&canonical).unwrap().unwrap();
        assert_eq!(fetched.access_mode, ProjectAccessMode::RiskBased);
        assert_eq!(fetched.display_name, "Tmp Workspace");
    }
}
