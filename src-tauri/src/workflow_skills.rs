use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use r_code_core::dto::RiskLevel;
use r_code_core::error::ProductError;
use r_code_gateway::{PathBinding, Tool};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAX_NAME_CHARS: usize = 64;
const MAX_DESCRIPTION_CHARS: usize = 800;
const MAX_INSTRUCTIONS_CHARS: usize = 20_000;
// Keep custom Skills from becoming invisible behind a built-in slash command or alias.
// The frontend applies the same rule when it composes the completion catalog.
const RESERVED_SLASH_COMMANDS: &[&str] = &[
    "clear",
    "new",
    "reset",
    "resume",
    "tasks",
    "history",
    "compact",
    "fork",
    "rename",
    "context",
    "status",
    "usage",
    "copy",
    "export",
    "stop",
    "model",
    "search",
    "pending",
    "inbox",
    "activity",
    "projects",
    "workspaces",
    "permissions",
    "permission",
    "agents",
    "subagents",
    "agent",
    "diff",
    "changes",
    "undo",
    "rewind",
    "files",
    "terminal",
    "review",
    "verify",
    "test",
    "run",
    "memory",
    "instructions",
    "theme",
    "settings",
    "plan",
    "doctor",
    "debug",
    "fix",
    "explain",
    "init",
    "code-review",
    "security-review",
    "simplify",
    "refactor",
    "docs",
    "document",
    "research",
    "qa",
    "codex",
    "mcp",
    "skills",
    "plugins",
    "help",
    "commands",
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowSkillSource {
    Builtin,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub source: WorkflowSkillSource,
    pub enabled: bool,
    pub overridden: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowSkillDraft {
    pub id: Option<String>,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub source: WorkflowSkillSource,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct WorkflowSkillCatalog {
    root: PathBuf,
}

impl WorkflowSkillCatalog {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn list(&self) -> Result<Vec<WorkflowSkill>, ProductError> {
        let mut skills = builtins();
        for skill in &mut skills {
            let path = self.builtin_override_path(&skill.id)?;
            if path.exists() {
                let mut overridden = read_skill(&path)?;
                overridden.id = skill.id.clone();
                overridden.source = WorkflowSkillSource::Builtin;
                overridden.overridden = true;
                validate_skill(&overridden)?;
                *skill = overridden;
            }
        }
        let custom_dir = self.custom_dir();
        if custom_dir.exists() {
            for entry in std::fs::read_dir(custom_dir)? {
                let path = entry?.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let mut skill = read_skill(&path)?;
                skill.source = WorkflowSkillSource::Custom;
                skill.overridden = false;
                validate_skill(&skill)?;
                skills.push(skill);
            }
        }
        ensure_unique_names(&skills, None)?;
        skills.sort_by(|left, right| {
            source_rank(left.source)
                .cmp(&source_rank(right.source))
                .then_with(|| left.name.cmp(&right.name))
        });
        Ok(skills)
    }

    pub fn save(&self, draft: WorkflowSkillDraft) -> Result<WorkflowSkill, ProductError> {
        let id = match draft.source {
            WorkflowSkillSource::Builtin => draft.id.clone().ok_or_else(|| {
                ProductError::ConfigError("built-in skill save requires an id".into())
            })?,
            WorkflowSkillSource::Custom => draft
                .id
                .clone()
                .filter(|id| id.starts_with("custom:"))
                .unwrap_or_else(|| format!("custom:{}", Uuid::new_v4())),
        };
        let skill = WorkflowSkill {
            id,
            name: draft.name,
            description: draft.description,
            instructions: draft.instructions,
            source: draft.source,
            enabled: draft.enabled,
            overridden: draft.source == WorkflowSkillSource::Builtin,
        };
        validate_skill(&skill)?;
        let existing = self.list()?;
        ensure_unique_names(&existing, Some(&skill))?;
        let path = match skill.source {
            WorkflowSkillSource::Builtin => {
                let default = builtins()
                    .into_iter()
                    .find(|item| item.id == skill.id)
                    .ok_or_else(|| {
                        ProductError::ConfigError(format!("unknown built-in skill: {}", skill.id))
                    })?;
                if skill.name != default.name {
                    return Err(ProductError::ConfigError(
                        "built-in skill invocation names cannot be changed".into(),
                    ));
                }
                self.builtin_override_path(&skill.id)?
            }
            WorkflowSkillSource::Custom => {
                if RESERVED_SLASH_COMMANDS.contains(&skill.name.as_str()) {
                    return Err(ProductError::ConfigError(format!(
                        "skill name is reserved by a built-in slash command: {}",
                        skill.name
                    )));
                }
                self.custom_path(&skill.id)?
            }
        };
        atomic_write_json(&path, &skill)?;
        Ok(skill)
    }

    pub fn save_custom_from_tool(
        &self,
        name: &str,
        description: &str,
        instructions: &str,
        enabled: bool,
    ) -> Result<WorkflowSkill, ProductError> {
        let current = self
            .list()?
            .into_iter()
            .find(|skill| skill.source == WorkflowSkillSource::Custom && skill.name == name);
        self.save(WorkflowSkillDraft {
            id: current.map(|skill| skill.id),
            name: name.to_string(),
            description: description.to_string(),
            instructions: instructions.to_string(),
            source: WorkflowSkillSource::Custom,
            enabled,
        })
    }

    pub fn reset_builtin(&self, id: &str) -> Result<WorkflowSkill, ProductError> {
        let default = builtins()
            .into_iter()
            .find(|skill| skill.id == id)
            .ok_or_else(|| ProductError::ConfigError(format!("unknown built-in skill: {id}")))?;
        let path = self.builtin_override_path(id)?;
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(default)
    }

    pub fn delete_custom(&self, id: &str) -> Result<(), ProductError> {
        if !id.starts_with("custom:") {
            return Err(ProductError::ConfigError(
                "built-in skills cannot be deleted; reset them instead".into(),
            ));
        }
        let path = self.custom_path(id)?;
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    fn builtin_dir(&self) -> PathBuf {
        self.root.join("builtin-overrides")
    }

    fn custom_dir(&self) -> PathBuf {
        self.root.join("custom")
    }

    fn builtin_override_path(&self, id: &str) -> Result<PathBuf, ProductError> {
        let name = id
            .strip_prefix("builtin:")
            .ok_or_else(|| ProductError::ConfigError(format!("invalid built-in skill id: {id}")))?;
        validate_name(name)?;
        Ok(self.builtin_dir().join(format!("{name}.json")))
    }

    fn custom_path(&self, id: &str) -> Result<PathBuf, ProductError> {
        let value = id
            .strip_prefix("custom:")
            .ok_or_else(|| ProductError::ConfigError(format!("invalid custom skill id: {id}")))?;
        let uuid = Uuid::parse_str(value)
            .map_err(|_| ProductError::ConfigError(format!("invalid custom skill id: {id}")))?;
        Ok(self.custom_dir().join(format!("{uuid}.json")))
    }
}

#[derive(Debug, Clone)]
pub struct SaveWorkflowSkillTool {
    catalog: WorkflowSkillCatalog,
}

impl SaveWorkflowSkillTool {
    pub fn new(catalog: WorkflowSkillCatalog) -> Self {
        Self { catalog }
    }
}

#[async_trait]
impl Tool for SaveWorkflowSkillTool {
    fn name(&self) -> &str {
        "save_skill"
    }

    fn description(&self) -> &str {
        "Save or update a user workflow skill in R-Code's AppData catalog. Use this only after the user asks to create a reusable skill. The result is immediately available to slash completion and never writes into the project workspace."
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R2
    }

    fn path_bindings(&self) -> &'static [PathBinding] {
        &[]
    }

    fn requires_existing_path(&self) -> bool {
        false
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Lowercase hyphenated invocation name, without a leading slash." },
                "description": { "type": "string", "description": "When and why this skill should be used." },
                "instructions": { "type": "string", "description": "Complete reusable instructions for the model." },
                "enabled": { "type": "boolean", "description": "Whether slash completion should expose it. Defaults to true." }
            },
            "required": ["name", "description", "instructions"]
        })
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        let field = |name: &str| {
            input
                .get(name)
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| ProductError::Other(format!("missing '{name}' parameter")))
        };
        let skill = self.catalog.save_custom_from_tool(
            field("name")?,
            field("description")?,
            field("instructions")?,
            input
                .get("enabled")
                .and_then(|value| value.as_bool())
                .unwrap_or(true),
        )?;
        serde_json::to_string(&serde_json::json!({
            "saved": true,
            "id": skill.id,
            "invocation": format!("/{}", skill.name),
            "source": "custom"
        }))
        .map_err(|error| ProductError::Other(format!("serialize save_skill result: {error}")))
    }
}

fn builtins() -> Vec<WorkflowSkill> {
    vec![
        builtin(
            "skill-creator",
            "创建可复用的 R-Code 自定义 Skill，并在用户确认后注册到自定义 Skill 系统。",
            "当用户希望把一套工作方式固化为 Skill 时使用。先确认触发场景、边界、输入、输出与安全约束；编写完整且可独立理解的 instructions。不得在项目目录创建 Skill 文件。完成草案后必须调用 save_skill(name, description, instructions, enabled=true)，由 R-Code 写入用户 AppData 的自定义 Skill 目录；只有工具返回 saved=true 才能宣告创建成功。",
        ),
        builtin(
            "mcp-creator",
            "创建或封装一个 MCP Server，完成源码与离线验证后保存为待用户审核的禁用草稿。",
            "当用户要求创建 MCP Server、把 API/CLI/数据库封装成 MCP，或修改既有 MCP 实现时使用。先根据当前项目和用户目标确定最小工具集、输入输出 Schema、只读/写入边界、传输方式与凭据环境变量；优先使用官方 MCP SDK，默认只读和最小权限，不把任何密钥写入源码、参数、日志或草稿。把实现放在当前项目内，补充错误处理、超时、输出限额与单元测试。可以编译、静态检查和运行不启动服务的单元测试；不得启动服务、执行 MCP 握手、tools/list、注册、启用或持久运行进程。验证成功后调用 mcp_create_draft，传入当前项目内已存在的 source_path、唯一 server_id、显示信息和精确启动配置；该工具只会创建关闭状态的草稿。不得调用 mcp_prepare_enable，也不得声称服务已经运行。只有工具返回 status=draft_created 才能宣告草稿创建成功；最后明确告知用户服务尚未启用，并引导用户前往“设置 → 工具与连接”审核启动方案、配置凭据并亲自打开滑钮。更新已有服务时不得覆盖现有 ID，改用新的草稿 ID 交由用户审核。",
        ),
        builtin(
            "review-changes",
            "审核本任务产生的文件差异，并安全地接受单行、单文件或全部任务变更。",
            "只审核 R-Code 为当前任务归集、且未被 .gitignore 或生成物规则排除的路径。先检查冲突、任务开始前已有脏改动和验证结果；逐项解释关键差异。按用户选择在应用审核账本中接受单行、单个文件或全部任务路径；接受不等于 Git 暂存，不得在审核阶段执行 git add。拒绝文件时只在当前内容仍匹配任务产物时恢复任务前快照。遇到 preexisting_dirty 或 conflict 必须停下并说明。",
        ),
        builtin(
            "git-commit-push",
            "为已接受的任务变更生成提交信息、提交，并在明确确认后推送已有 upstream。",
            "先确认审核账本没有未决项，再由用户显式执行“暂存已接受文件”；只暂存本次审核中已接受的路径，禁止 git add -A，并拒绝混入任务外暂存内容。生成简洁、可编辑的提交信息建议；只有用户执行提交操作后才 commit。推送前展示 branch、upstream、ahead/behind，并要求显式确认；只执行普通 git push，不设置 upstream、不修改远端、不使用 force。",
        ),
    ]
}

fn builtin(name: &str, description: &str, instructions: &str) -> WorkflowSkill {
    WorkflowSkill {
        id: format!("builtin:{name}"),
        name: name.to_string(),
        description: description.to_string(),
        instructions: instructions.to_string(),
        source: WorkflowSkillSource::Builtin,
        enabled: true,
        overridden: false,
    }
}

fn validate_skill(skill: &WorkflowSkill) -> Result<(), ProductError> {
    validate_name(&skill.name)?;
    validate_text("description", &skill.description, MAX_DESCRIPTION_CHARS)?;
    validate_text("instructions", &skill.instructions, MAX_INSTRUCTIONS_CHARS)?;
    match skill.source {
        WorkflowSkillSource::Builtin if !skill.id.starts_with("builtin:") => Err(
            ProductError::ConfigError("built-in skill id must start with 'builtin:'".into()),
        ),
        WorkflowSkillSource::Custom if !skill.id.starts_with("custom:") => Err(
            ProductError::ConfigError("custom skill id must start with 'custom:'".into()),
        ),
        _ => Ok(()),
    }
}

fn validate_name(name: &str) -> Result<(), ProductError> {
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("--")
    {
        return Err(ProductError::ConfigError(
            "skill name must use lowercase letters, digits and single hyphens".into(),
        ));
    }
    Ok(())
}

fn validate_text(label: &str, value: &str, max: usize) -> Result<(), ProductError> {
    if value.trim().is_empty() || value.contains('\0') || value.chars().count() > max {
        return Err(ProductError::ConfigError(format!(
            "skill {label} must be non-empty, contain no NUL, and stay within {max} characters"
        )));
    }
    Ok(())
}

fn ensure_unique_names(
    skills: &[WorkflowSkill],
    candidate: Option<&WorkflowSkill>,
) -> Result<(), ProductError> {
    let mut names = HashSet::new();
    for skill in skills {
        if candidate.is_some_and(|candidate| candidate.id == skill.id) {
            continue;
        }
        if !names.insert(skill.name.as_str()) {
            return Err(ProductError::ConfigError(format!(
                "duplicate skill name: {}",
                skill.name
            )));
        }
    }
    if let Some(candidate) = candidate {
        if names.contains(candidate.name.as_str()) {
            return Err(ProductError::ConfigError(format!(
                "skill name already exists: {}",
                candidate.name
            )));
        }
    }
    Ok(())
}

fn read_skill(path: &Path) -> Result<WorkflowSkill, ProductError> {
    let content = std::fs::read(path)?;
    serde_json::from_slice(&content).map_err(|error| {
        ProductError::ConfigError(format!("parse workflow skill {}: {error}", path.display()))
    })
}

fn atomic_write_json(path: &Path, value: &WorkflowSkill) -> Result<(), ProductError> {
    let parent = path.parent().ok_or_else(|| {
        ProductError::ConfigError("workflow skill path has no parent directory".into())
    })?;
    std::fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| ProductError::ConfigError(format!("serialize workflow skill: {error}")))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    // tempfile's persist operation atomically replaces an existing target on supported
    // platforms; on failure the original target remains intact.
    temporary
        .persist(path)
        .map_err(|error| ProductError::from(error.error))?;
    Ok(())
}

fn source_rank(source: WorkflowSkillSource) -> u8 {
    match source {
        WorkflowSkillSource::Builtin => 0,
        WorkflowSkillSource::Custom => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_override_reset_and_custom_crud_stay_outside_workspaces() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("app-data/workflow-skills");
        let catalog = WorkflowSkillCatalog::new(root.clone());
        let defaults = catalog.list().unwrap();
        assert!(defaults.iter().any(|skill| skill.name == "skill-creator"));
        assert!(defaults.iter().any(|skill| skill.name == "mcp-creator"));
        assert!(defaults.iter().any(|skill| skill.name == "review-changes"));
        assert!(defaults.iter().any(|skill| skill.name == "git-commit-push"));

        let creator = defaults
            .into_iter()
            .find(|skill| skill.name == "skill-creator")
            .unwrap();
        let edited = catalog
            .save(WorkflowSkillDraft {
                id: Some(creator.id.clone()),
                name: creator.name.clone(),
                description: creator.description.clone(),
                instructions: "edited creator instructions".into(),
                source: WorkflowSkillSource::Builtin,
                enabled: false,
            })
            .unwrap();
        assert!(edited.overridden);
        assert!(!edited.enabled);
        assert!(!catalog.reset_builtin(&creator.id).unwrap().overridden);

        let custom = catalog
            .save_custom_from_tool("my-review", "review things", "do the review safely", true)
            .unwrap();
        assert_eq!(custom.source, WorkflowSkillSource::Custom);
        assert!(root.join("custom").exists());
        let updated = catalog
            .save_custom_from_tool(
                "my-review",
                "review things again",
                "use the updated safe review",
                true,
            )
            .unwrap();
        assert_eq!(updated.id, custom.id);
        let matching: Vec<_> = catalog
            .list()
            .unwrap()
            .into_iter()
            .filter(|skill| skill.name == "my-review")
            .collect();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].instructions, "use the updated safe review");
        catalog.delete_custom(&custom.id).unwrap();
        assert!(!catalog
            .list()
            .unwrap()
            .iter()
            .any(|skill| skill.id == custom.id));
    }

    #[tokio::test]
    async fn save_skill_tool_registers_and_validates_custom_skills() {
        let temp = tempfile::tempdir().unwrap();
        let catalog = WorkflowSkillCatalog::new(temp.path().join("workflow-skills"));
        let tool = SaveWorkflowSkillTool::new(catalog.clone());
        let result = tool
            .execute(serde_json::json!({
                "name": "safe-review",
                "description": "review safely",
                "instructions": "inspect task paths and never stage unrelated files"
            }))
            .await
            .unwrap();
        assert!(result.contains("/safe-review"));
        assert!(catalog
            .list()
            .unwrap()
            .iter()
            .any(|skill| skill.name == "safe-review"));
        assert!(tool
            .execute(serde_json::json!({
                "name": "Unsafe Name",
                "description": "bad",
                "instructions": "bad"
            }))
            .await
            .is_err());
        assert!(tool
            .execute(serde_json::json!({
                "name": "help",
                "description": "shadow help",
                "instructions": "this must not hide the built-in command"
            }))
            .await
            .is_err());
    }
}
