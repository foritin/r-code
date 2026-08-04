use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use r_code_core::dto::{AgentRun, ProjectAccessMode, RiskLevel, Task, TaskMode};
use r_code_core::error::ProductError;
use r_code_gateway::{Tool, ToolExecutionContext, ToolExecutionResult};
use r_code_host::plan_review_tools::{PlanReviewServices, TrackedWriteTool};
use r_code_store::{
    AgentRunRepository, Database, OsPlanReviewFileSystem, PathCoordinator, PlanReviewFileSystem,
    TaskRepository,
};
use rusqlite::params;
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    workspace: PathBuf,
    db: Arc<Database>,
    task: Task,
    run: AgentRun,
}

impl Fixture {
    fn new(with_active_feature: bool) -> Self {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let db = Arc::new(Database::open_in_memory().unwrap());
        let task = Task::new(
            Some(workspace.to_string_lossy().into_owned()),
            "tracked write",
            "test Plan ownership",
            TaskMode::Edit,
        );
        TaskRepository::new(&db).create(&task).unwrap();
        let run = AgentRun::new(&task.id, "test-model");
        AgentRunRepository::new(&db).create(&run).unwrap();
        if with_active_feature {
            let now = Utc::now().to_rfc3339();
            let conn = db.conn().unwrap();
            conn.execute(
                "INSERT INTO plans (
                     id, task_id, revision, state, approved_revision, created_at, updated_at,
                     approved_at
                 ) VALUES ('plan', ?1, 1, 'executing', 1, ?2, ?2, ?2)",
                params![task.id, now],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO plan_items (
                     id, plan_id, revision, ordinal, title, description, state,
                     created_at, updated_at
                 ) VALUES ('feature', 'plan', 1, 0, 'Feature', 'Description', 'in_progress',
                           ?1, ?1)",
                params![now],
            )
            .unwrap();
        }
        Self {
            _temp: temp,
            workspace,
            db,
            task,
            run,
        }
    }

    fn context(&self, tool_call_id: &str) -> ToolExecutionContext {
        ToolExecutionContext {
            task_id: self.task.id.clone(),
            run_id: self.run.id.clone(),
            tool_call_id: tool_call_id.to_string(),
            caller: Some("main-agent".into()),
            access_mode: ProjectAccessMode::FullAccess,
        }
    }

    fn services(&self, file_system: Arc<dyn PlanReviewFileSystem>) -> PlanReviewServices {
        let blobs = self._temp.path().join("blobs");
        std::fs::create_dir_all(&blobs).unwrap();
        PlanReviewServices::with_dependencies(
            self.db.clone(),
            blobs,
            PathCoordinator::default(),
            file_system,
        )
    }
}

struct FakeWriteTool {
    fail_after_write: bool,
    observed_context: Arc<Mutex<Option<ToolExecutionContext>>>,
}

impl FakeWriteTool {
    fn new(fail_after_write: bool) -> (Self, Arc<Mutex<Option<ToolExecutionContext>>>) {
        let observed_context = Arc::new(Mutex::new(None));
        (
            Self {
                fail_after_write,
                observed_context: observed_context.clone(),
            },
            observed_context,
        )
    }

    fn write(&self, input: &serde_json::Value) -> Result<(), ProductError> {
        let path = input
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ProductError::Other("missing path".into()))?;
        let content = input
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("changed");
        std::fs::write(path, content)?;
        Ok(())
    }
}

#[async_trait]
impl Tool for FakeWriteTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "test write"
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R2
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        self.write(&input)?;
        if self.fail_after_write {
            Err(ProductError::Other(
                "inner failure after partial write".into(),
            ))
        } else {
            Ok("written".into())
        }
    }

    async fn execute_with_context(
        &self,
        input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolExecutionResult, ProductError> {
        *self.observed_context.lock().unwrap() = Some(context.clone());
        self.execute(input).await.map(ToolExecutionResult::success)
    }
}

struct CountingFileSystem {
    reads: AtomicUsize,
    fail_on_read: Option<usize>,
    os: OsPlanReviewFileSystem,
}

impl CountingFileSystem {
    fn new(fail_on_read: Option<usize>) -> Self {
        Self {
            reads: AtomicUsize::new(0),
            fail_on_read,
            os: OsPlanReviewFileSystem,
        }
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

impl PlanReviewFileSystem for CountingFileSystem {
    fn read_snapshot(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        let read = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_on_read == Some(read) {
            return Err(io::Error::other("injected capture read failure"));
        }
        self.os.read_snapshot(path)
    }

    fn write_snapshot(&self, path: &Path, content: Option<&[u8]>) -> io::Result<()> {
        self.os.write_snapshot(path, content)
    }
}

#[tokio::test]
async fn successful_write_is_captured_and_forwards_context() {
    let fixture = Fixture::new(true);
    let fs = Arc::new(CountingFileSystem::new(None));
    let services = fixture.services(fs.clone());
    let (inner, observed_context) = FakeWriteTool::new(false);
    let tracked = TrackedWriteTool::new(Box::new(inner), services.clone());
    let context = fixture.context("tool-success");
    let path = fixture.workspace.join("success.txt");

    let result = tracked
        .execute_with_context(
            serde_json::json!({ "path": path, "content": "captured" }),
            &context,
        )
        .await
        .unwrap();

    assert_eq!(result.content, "written");
    assert_eq!(*observed_context.lock().unwrap(), Some(context));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "captured");
    assert_eq!(fs.reads(), 2);
    let view = services
        .store()
        .list_current(&fixture.task.id)
        .unwrap()
        .unwrap();
    assert_eq!(view.groups.len(), 1);
    assert_eq!(
        view.groups[0].files[0].events[0].tool_call_id,
        "tool-success"
    );
}

#[tokio::test]
async fn failed_inner_write_still_captures_partial_mutation_and_returns_original_error() {
    let fixture = Fixture::new(true);
    let fs = Arc::new(CountingFileSystem::new(None));
    let services = fixture.services(fs);
    let (inner, _) = FakeWriteTool::new(true);
    let tracked = TrackedWriteTool::new(Box::new(inner), services.clone());
    let path = fixture.workspace.join("partial.txt");

    let error = tracked
        .execute_with_context(
            serde_json::json!({ "path": path, "content": "partial" }),
            &fixture.context("tool-partial"),
        )
        .await
        .unwrap_err();

    assert!(error
        .to_string()
        .contains("inner failure after partial write"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "partial");
    let view = services
        .store()
        .list_current(&fixture.task.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        view.groups[0].files[0].events[0].tool_call_id,
        "tool-partial"
    );
}

#[tokio::test]
async fn ordinary_write_uses_coordination_without_snapshot_or_event() {
    let fixture = Fixture::new(false);
    let fs = Arc::new(CountingFileSystem::new(Some(1)));
    let services = fixture.services(fs.clone());
    let (inner, _) = FakeWriteTool::new(false);
    let tracked = TrackedWriteTool::new(Box::new(inner), services.clone());
    let path = fixture.workspace.join("ordinary.txt");

    tracked
        .execute_with_context(
            serde_json::json!({ "path": path, "content": "ordinary" }),
            &fixture.context("tool-ordinary"),
        )
        .await
        .unwrap();

    assert_eq!(
        fs.reads(),
        0,
        "ordinary mode must not read review snapshots"
    );
    assert!(services
        .store()
        .list_current(&fixture.task.id)
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn paused_plan_rejects_direct_write_without_touching_the_file() {
    let fixture = Fixture::new(true);
    fixture
        .db
        .conn()
        .unwrap()
        .execute(
            "UPDATE plan_items SET state = 'blocked' WHERE plan_id = 'plan' AND id = 'feature'",
            [],
        )
        .unwrap();
    let fs = Arc::new(CountingFileSystem::new(None));
    let services = fixture.services(fs.clone());
    let (inner, observed_context) = FakeWriteTool::new(false);
    let tracked = TrackedWriteTool::new(Box::new(inner), services);
    let path = fixture.workspace.join("paused.txt");

    let error = tracked
        .execute_with_context(
            serde_json::json!({ "path": path, "content": "must not be written" }),
            &fixture.context("tool-paused"),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, ProductError::PermissionError(_)));
    assert!(error.to_string().contains("Plan execution is paused"));
    assert!(!path.exists());
    assert_eq!(fs.reads(), 0);
    assert_eq!(*observed_context.lock().unwrap(), None);
}

#[tokio::test]
async fn capture_failure_preserves_inner_error_and_surfaces_review_fallback() {
    let fixture = Fixture::new(true);
    let fs = Arc::new(CountingFileSystem::new(Some(2)));
    let services = fixture.services(fs);
    let (inner, _) = FakeWriteTool::new(true);
    let tracked = TrackedWriteTool::new(Box::new(inner), services);
    let path = fixture.workspace.join("capture-failed.txt");

    let error = tracked
        .execute_with_context(
            serde_json::json!({ "path": path, "content": "mutated" }),
            &fixture.context("tool-capture-failed"),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("inner failure after partial write"));
    assert!(error.contains("工作区可能已改变但增强归属失败，普通审核仍可用"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "mutated");
}
