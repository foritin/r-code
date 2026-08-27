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
