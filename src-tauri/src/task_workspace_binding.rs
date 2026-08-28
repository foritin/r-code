//! Task 运行工作区的唯一解析边界。
//!
//! 持久化的 `Task.workspace_path` 表示用户注册的项目，`Task.worktree_path` 表示该
//! Task 实际执行所在的 linked worktree。调用方不得自行在两者之间选择：一旦任务
//! 声明了 worktree，本模块会验证它仍属于注册项目；任何不一致都 fail closed，绝不
//! 回退到原项目目录。

use std::path::{Path, PathBuf};

use r_code_core::dto::{ProjectAccessMode, Task};
use r_code_core::error::{ProductError, ProductResult};
use r_code_core::UserFacingError;
use r_code_store::{Database, GitService, WorkspaceService};

/// 未使用 linked worktree 的任务绑定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWorkspaceBinding {
    pub task_id: String,
    pub root: PathBuf,
    /// 注册项目的物理根；纯聊天使用用户主目录执行，但没有注册项目。
    pub registered_workspace_root: Option<PathBuf>,
    pub access_mode: ProjectAccessMode,
}

/// 已通过 Git 拓扑验证的 linked worktree 绑定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedWorktreeBinding {
    pub task_id: String,
    /// Task 实际执行根目录。
    pub root: PathBuf,
    /// 用户最初注册的项目目录。
    pub registered_workspace_root: PathBuf,
    /// 注册项目所在工作树的 Git 根目录。
    pub repo_root: PathBuf,
    /// 注册项目与 linked worktree 共同使用的 Git 元数据目录。
    pub common_dir: PathBuf,
    pub access_mode: ProjectAccessMode,
}

/// Task 的权威运行目录。所有 Agent、终端、文件、Git 与 Review 消费者都应使用它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskWorkspaceBinding {
    Local(LocalWorkspaceBinding),
    ManagedWorktree(ManagedWorktreeBinding),
}

impl TaskWorkspaceBinding {
    pub fn root(&self) -> &Path {
        match self {
            Self::Local(binding) => &binding.root,
            Self::ManagedWorktree(binding) => &binding.root,
        }
    }

    pub fn access_mode(&self) -> ProjectAccessMode {
        match self {
            Self::Local(binding) => binding.access_mode,
            Self::ManagedWorktree(binding) => binding.access_mode,
        }
    }

    pub fn repo_root(&self) -> Option<&Path> {
        match self {
            Self::Local(_) => None,
            Self::ManagedWorktree(binding) => Some(&binding.repo_root),
        }
    }

    /// 适配现有 Agent contract。纯聊天沿用既有语义，把用户主目录作为受审批保护的根。
    pub fn into_runtime_parts(self) -> (Option<String>, ProjectAccessMode) {
        let access_mode = self.access_mode();
        (Some(self.root().display().to_string()), access_mode)
    }
}

/// 只依赖数据库和 Git 的可复用 Task 工作区解析服务。
pub struct TaskWorkspaceBindingService<'a> {
    db: &'a Database,
}

impl<'a> TaskWorkspaceBindingService<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn resolve(&self, task: &Task) -> ProductResult<TaskWorkspaceBinding> {
        let Some(workspace_path) = task.workspace_path.as_deref() else {
            if task.worktree_path.is_some() {
                return Err(invalid_worktree(
                    task,
                    "task has a worktree but no registered workspace",
                ));
            }
            return resolve_pure_chat(task);
        };

        let workspace = WorkspaceService::new(self.db)
            .get(workspace_path)?
            .ok_or_else(|| {
                invalid_registered_workspace(task, "workspace is no longer registered")
            })?;
        let workspace_root = canonical_directory(
            Path::new(&workspace.canonical_path),
            "workspace is no longer accessible",
        )
        .map_err(|error| {
            if task.worktree_path.is_some() {
                invalid_worktree(task, error.to_string())
            } else {
                invalid_registered_workspace(task, error.to_string())
            }
        })?;

        let Some(worktree_path) = task.worktree_path.as_deref() else {
            return Ok(TaskWorkspaceBinding::Local(LocalWorkspaceBinding {
                task_id: task.id.clone(),
                root: workspace_root.clone(),
                registered_workspace_root: Some(workspace_root),
                access_mode: workspace.access_mode,
            }));
        };

        self.resolve_worktree(task, &workspace_root, worktree_path, workspace.access_mode)
    }

    fn resolve_worktree(
        &self,
        task: &Task,
        workspace_root: &Path,
        worktree_path: &str,
        access_mode: ProjectAccessMode,
    ) -> ProductResult<TaskWorkspaceBinding> {
        if worktree_path.trim().is_empty() {
            return Err(invalid_worktree(task, "worktree path is empty"));
        }
        let worktree_root = canonical_worktree_directory(task, Path::new(worktree_path))?;
        if !worktree_root.join(".git").is_file() {
            return Err(invalid_worktree(
                task,
                "managed worktree is not a linked Git worktree",
            ));
        }
        let repository = GitService::new(workspace_root.to_path_buf());
        let repo_root = canonical_git_path(
            task,
            repository.repo_root(),
            "registered workspace is not in a Git repository",
        )?;
        if worktree_root == repo_root {
            return Err(invalid_worktree(
                task,
                "managed worktree points at the registered repository root",
            ));
        }

        let common_dir = canonical_git_path(
            task,
            repository.common_dir(),
            "cannot resolve registered repository common dir",
        )?;
        let listed = repository.worktree_paths().map_err(|error| {
            invalid_worktree(task, format!("cannot list registered worktrees: {error}"))
        })?;
        let is_registered = listed.iter().any(|path| {
            path.canonicalize()
                .is_ok_and(|candidate| candidate == worktree_root)
        });
        if !is_registered {
            return Err(invalid_worktree(
                task,
                "worktree is not registered by the task repository",
            ));
        }

        let worktree_repository = GitService::new(worktree_root.clone());
        let reported_worktree_root = canonical_git_path(
            task,
            worktree_repository.repo_root(),
            "cannot resolve worktree repository root",
        )?;
        if reported_worktree_root != worktree_root {
            return Err(invalid_worktree(
                task,
                "worktree path is not the Git worktree root",
            ));
        }
        let worktree_common_dir = canonical_git_path(
            task,
            worktree_repository.common_dir(),
            "cannot resolve worktree common dir",
        )?;
        if worktree_common_dir != common_dir {
            return Err(invalid_worktree(
                task,
                "worktree belongs to a different Git repository",
            ));
        }

        Ok(TaskWorkspaceBinding::ManagedWorktree(
            ManagedWorktreeBinding {
                task_id: task.id.clone(),
                root: worktree_root,
                registered_workspace_root: workspace_root.to_path_buf(),
                repo_root,
                common_dir,
                access_mode,
            },
        ))
    }
}

pub fn resolve_task_workspace_binding(
    db: &Database,
    task: &Task,
) -> ProductResult<TaskWorkspaceBinding> {
    TaskWorkspaceBindingService::new(db).resolve(task)
}

fn resolve_pure_chat(task: &Task) -> ProductResult<TaskWorkspaceBinding> {
    let home = dirs::home_dir()
        .filter(|path| path.is_dir())
        .ok_or_else(|| {
            ProductError::from(
                UserFacingError::new("workspace.home_unavailable")
                    .with_arg("task_id", task.id.clone())
                    .with_debug_detail(
                        "the operating system did not expose an accessible home directory",
                    ),
            )
        })?;
    let root = canonical_directory(&home, "home directory is inaccessible").map_err(|error| {
        ProductError::from(
            UserFacingError::new("workspace.home_unavailable")
                .with_arg("task_id", task.id.clone())
                .with_debug_detail(error.to_string()),
        )
    })?;
    Ok(TaskWorkspaceBinding::Local(LocalWorkspaceBinding {
        task_id: task.id.clone(),
        root,
        registered_workspace_root: None,
        access_mode: ProjectAccessMode::RequestApproval,
    }))
}

fn canonical_directory(path: &Path, context: &str) -> ProductResult<PathBuf> {
    if !path.is_dir() {
        return Err(ProductError::Other(format!(
            "{context}: path is not a directory"
        )));
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| ProductError::Other(format!("{context}: {error}")))?;
    if !canonical.is_dir() {
        return Err(ProductError::Other(format!(
            "{context}: path is not a directory"
        )));
    }
    Ok(canonical)
}

fn canonical_worktree_directory(task: &Task, path: &Path) -> ProductResult<PathBuf> {
    if !path.is_dir() {
        return Err(invalid_worktree(
            task,
            "worktree path does not exist or is not a directory",
        ));
    }
    let canonical = path.canonicalize().map_err(|error| {
        invalid_worktree(task, format!("cannot canonicalize worktree path: {error}"))
    })?;
    if !canonical.is_dir() {
        return Err(invalid_worktree(task, "worktree path is not a directory"));
    }
    Ok(canonical)
}

fn canonical_git_path(
    task: &Task,
    result: Result<PathBuf, ProductError>,
    context: &str,
) -> ProductResult<PathBuf> {
    let path = result.map_err(|error| invalid_worktree(task, format!("{context}: {error}")))?;
    path.canonicalize()
        .map_err(|error| invalid_worktree(task, format!("{context}: {error}")))
}

fn invalid_worktree(task: &Task, detail: impl Into<String>) -> ProductError {
    UserFacingError::new("worktree.binding_invalid")
        .with_arg("task_id", task.id.clone())
        .with_debug_detail(detail)
        .into()
}

fn invalid_registered_workspace(task: &Task, detail: impl Into<String>) -> ProductError {
    UserFacingError::new("workspace.binding_invalid")
        .with_arg("task_id", task.id.clone())
        .with_debug_detail(detail)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_core::dto::TaskMode;
    use std::process::Command as StdCommand;

    /// 建立独立临时目录（互不共享，cargo 并行安全）。
    fn temp_root(label: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("r-code-binding-{label}-"))
            .tempdir()
            .expect("create binding fixture temp directory")
    }

    fn git(repo: &Path, args: &[&str]) {
        let output = StdCommand::new("git")
            .arg("-C")
            .arg(repo)
            .args([
                "-c",
                "user.email=binding-test@example.invalid",
                "-c",
                "user.name=binding-test",
            ])
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// 带一个空提交的可用 Git 仓库。
    fn temp_repo(label: &str) -> (tempfile::TempDir, PathBuf) {
        let temp = temp_root(label);
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--quiet"]);
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "seed"]);
        (temp, repo)
    }

    fn task_for(workspace: Option<&str>, worktree: Option<&str>) -> Task {
        let mut task = Task::new(
            workspace.map(str::to_string),
            "binding-fixture",
            "fixture goal",
            TaskMode::Ask,
        );
        task.worktree_path = worktree.map(str::to_string);
        task
    }

    // ---- M1-02.A2：合法 Local/Worktree 解析一致，重开后读回一致 ----

    #[test]
    fn a2_local_binding_resolves_and_reopens_idempotently() {
        let temp = temp_root("a2-local");
        let workspace = temp.path().join("project");
        std::fs::create_dir_all(&workspace).unwrap();

        let db_path = temp.path().join("state.sqlite");
        let task = task_for(Some(workspace.to_str().unwrap()), None);
        let first = {
            let db = Database::open(&db_path).expect("open fresh database for local binding");
            WorkspaceService::new(&db)
                .open(workspace.to_str().unwrap(), "local-project")
                .expect("register workspace");
            resolve_task_workspace_binding(&db, &task).expect("resolve local binding")
        };

        let second = {
            let db = Database::open(&db_path).expect("reopen database");
            resolve_task_workspace_binding(&db, &task).expect("resolve local binding again")
        };
        assert_eq!(first, second, "binding 读取应幂等");
        match first {
            TaskWorkspaceBinding::Local(local) => {
                assert_eq!(local.root, workspace.canonicalize().unwrap());
                assert!(local.registered_workspace_root.is_some());
            }
            other => panic!("expected Local binding, got {other:?}"),
        }
    }

    #[test]
    fn a2_worktree_binding_resolves_and_reopens_idempotently() {
        let ws_temp = temp_root("a2-wt");
        let workspace = ws_temp.path().join("registered");

        // registered 目录本身必须是合法 Git 仓库（GitService 从它解析 repo/common）。
        std::fs::create_dir_all(&workspace).unwrap();
        git_init_at(&workspace);

        let worktree = ws_temp.path().join("wt-task42");
        git(&workspace, &[
            "worktree",
            "add",
            "-b",
            "r-code/task42",
            worktree.to_str().unwrap(),
            "HEAD",
        ]);

        let db_path = ws_temp.path().join("state.sqlite");
        let task = task_for(Some(workspace.to_str().unwrap()), Some(worktree.to_str().unwrap()));
        let resolve_once = || {
            let db = Database::open(&db_path).expect("open database");
            WorkspaceService::new(&db)
                .open(workspace.to_str().unwrap(), "wt-project")
                .expect("register workspace");
            resolve_task_workspace_binding(&db, &task).expect("resolve worktree binding")
        };
        let first = resolve_once();
        let second = resolve_once();
        assert_eq!(first, second);
        match first {
            TaskWorkspaceBinding::ManagedWorktree(managed) => {
                assert_eq!(managed.root, worktree.canonicalize().unwrap());
                assert_eq!(
                    managed.registered_workspace_root,
                    workspace.canonicalize().unwrap()
                );
            }
            other => panic!("expected ManagedWorktree binding, got {other:?}"),
        }
    }

    fn git_init_at(dir: &Path) {
        git(dir, &["init", "--quiet"]);
        git(dir, &["commit", "--quiet", "--allow-empty", "-m", "seed"]);
    }

    // ---- M1-02.A3：越界/缺失/mismatch/symlink 全部拒绝，绝不回退 Local ----

    #[test]
    fn a3_missing_or_unreachable_registration_is_rejected() {
        let temp = temp_root("a3-missing");
        let db = Database::open_in_memory().expect("in-memory db");

        let ghost = temp.path().join("never-registered");
        std::fs::create_dir_all(&ghost).unwrap();
        let err = resolve_task_workspace_binding(&db, &task_for(Some(ghost.to_str().unwrap()), None))
            .expect_err("unregistered workspace must fail closed");
        assert!(err.to_string().contains("workspace"), "{err}");

        let vanished = temp.path().join("vanishes-later");
        std::fs::create_dir_all(&vanished).unwrap();
        WorkspaceService::new(&db)
            .open(vanished.to_str().unwrap(), "later-removed")
            .expect("register then delete target");
        std::fs::remove_dir_all(&vanished).unwrap();
        let err = resolve_task_workspace_binding(&db, &task_for(Some(vanished.to_str().unwrap()), None))
            .expect_err("removed workspace dir must fail closed");
        assert!(err.to_string().contains("workspace"), "{err}");
    }

    #[test]
    fn a3_worktree_without_git_file_is_rejected() {
        let temp = temp_root("a3-nogit");
        let workspace = temp.path().join("registered");
        std::fs::create_dir_all(&workspace).unwrap();
        git_init_at(&workspace);

        // 普通子目录：既不是 linked worktree（.git 不是文件）也不存在逃逸可能。
        let plain_dir = temp.path().join("plain");
        std::fs::create_dir_all(&plain_dir).unwrap();

        let db = Database::open_in_memory().expect("in-memory db");
        WorkspaceService::new(&db)
            .open(workspace.to_str().unwrap(), "nogit")
            .unwrap();
        let task = task_for(Some(workspace.to_str().unwrap()), Some(plain_dir.to_str().unwrap()));
        let result = resolve_task_workspace_binding(&db, &task);
        assert!(result.is_err(), "plain directory must be rejected");
        assert!(!matches!(result, Ok(TaskWorkspaceBinding::Local(_))));
    }

    #[test]
    fn a3_symlink_into_foreign_repo_root_is_rejected() {
        let (_foreign_temp, foreign) = temp_repo("a3-symlink-target");
        let temp = temp_root("a3-symlink");
        let workspace = temp.path().join("registered");
        std::fs::create_dir_all(&workspace).unwrap();
        git_init_at(&workspace);

        // junction/symlink 逃逸模型：worktree 字段指向指向外部仓库根的软链。
        #[cfg(unix)]
        std::os::unix::fs::symlink(&foreign, temp.path().join("escape")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&foreign, temp.path().join("escape")).unwrap();

        let db = Database::open_in_memory().expect("in-memory db");
        WorkspaceService::new(&db)
            .open(workspace.to_str().unwrap(), "symlinked")
            .unwrap();
        let task = task_for(
            Some(workspace.to_str().unwrap()),
            Some(temp.path().join("escape").to_str().unwrap()),
        );
        let result = resolve_task_workspace_binding(&db, &task);
        assert!(result.is_err(), "symlink escape must fail closed");
        assert!(
            !matches!(result, Ok(TaskWorkspaceBinding::Local(_))),
            "never fall back to Local on rejection"
        );
    }

    #[test]
    fn a3_repo_mismatch_is_rejected() {
        let (_other_temp, other_repo) = temp_repo("a3-mismatch-other");
        let temp = temp_root("a3-mismatch");
        let workspace = temp.path().join("registered");
        std::fs::create_dir_all(&workspace).unwrap();
        git_init_at(&workspace);

        // worktree 属于另一个仓库 common dir；registered 只是普通 git 仓库目录。
        let foreign_worktree = temp.path().join("foreign-wt");
        git(&other_repo, &[
            "worktree",
            "add",
            "-b",
            "r-code/foreign",
            foreign_worktree.to_str().unwrap(),
            "HEAD",
        ]);
        // 该 worktree 的 .git 是文件 → 通过第一道检查，随后必须在
        // "belongs to a different Git repository" 处被拒。

        let db = Database::open_in_memory().expect("in-memory db");
        WorkspaceService::new(&db)
            .open(workspace.to_str().unwrap(), "mismatch-base")
            .unwrap();
        // resolver 以 registered workspace 为 Git 根；foreign worktree 的 common dir
        // 必然不同 → 拒绝。由于 registered 不是任何 worktree 的宿主，另一个更早的
        // 拒绝点同样成立；只断言“拒绝且非 Local”即可满足 fail-closed 合同。
        let task = task_for(
            Some(workspace.to_str().unwrap()),
            Some(foreign_worktree.to_str().unwrap()),
        );
        let result = resolve_task_workspace_binding(&db, &task);
        assert!(result.is_err());
        assert!(!matches!(result, Ok(TaskWorkspaceBinding::Local(_))));
    }
}
