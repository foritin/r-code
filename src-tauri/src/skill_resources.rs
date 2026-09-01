//! Skill 资源扫描（docs/pi-alignment PRD §4.1 R-EXT-01 / M4-01）。
//!
//! Pi 风格的技能目录扫描：全局（AppData `data_dir/skills/`）+ 项目
//! （`<workspace>/.r-code/skills/`）两级，每个技能是一个目录 + `SKILL.md`
//! （YAML frontmatter：name/description）。统一现有 `.agents/skills` 语义：
//! frontmatter 结构与入口命名与其一致（构建期资产在仓库 `.agents/skills/`，
//! 运行时用户资产在这两级目录——同一解析器消费）。
//!
//! 发现规则（对齐 Pi）：坏文件静默跳过（不中断扫描、不报错——技能是增强
//! 不是依赖）；项目级同名覆盖全局级（就近优先）；返回按 name 排序稳定清单。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
/// 扫描产物：一个技能。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillResource {
    /// frontmatter name（缺省取目录名）。
    pub name: String,
    /// frontmatter description（一行；渐进披露的展示文本）。
    pub description: String,
    /// SKILL.md 绝对路径（read 工具按需取全文）。
    pub path: PathBuf,
    /// 来源层级：global（AppData）/ project（工作区）。
    pub source: SkillSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    Global,
    Project,
}

/// SKILL.md 的 frontmatter（与 `.agents/skills` 同一结构）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillFrontmatter {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// 解析 SKILL.md：`---` 围栏 frontmatter + 正文；无 frontmatter 时返回
/// None（坏文件跳过）。frontmatter 只需 `name:`/`description:` 键——与
/// `.agents/skills` 实际使用的子集一致（description 常用 YAML `|`/`>` 多行
/// 块），手写解析避免给宿主 crate 引入 YAML 依赖；带引号的值去引号。
pub fn parse_skill_md(content: &str, fallback_name: &str) -> Option<SkillFrontmatter> {
    let text = content.strip_prefix('\u{feff}').unwrap_or(content);
    let after_fence = text.strip_prefix("---\n")?;
    let end = after_fence.find("\n---")?;
    let yaml = after_fence[..end].lines().collect::<Vec<_>>();
    let mut frontmatter = SkillFrontmatter::default();
    let mut index = 0usize;
    while index < yaml.len() {
        let line = yaml[index].trim();
        if line.is_empty() || line.starts_with('#') {
            index += 1;
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return None; // 结构性损坏（非键值行）：视为坏文件
        };
        let key = key.trim();
        let value = value.trim();
        // 块标量（`|`/`>`，可带折叠/strip 修饰）：取后续更深缩进行。
        let block =
            if value == "|" || value == ">" || value.starts_with("|") || value.starts_with(">") {
                let mut block_lines: Vec<String> = Vec::new();
                let mut cursor = index + 1;
                while cursor < yaml.len() {
                    let next = yaml[cursor];
                    if next.trim().is_empty() {
                        block_lines.push(String::new());
                        cursor += 1;
                        continue;
                    }
                    let indent = next.len() - next.trim_start().len();
                    if indent == 0 {
                        break;
                    }
                    block_lines.push(next.trim_start().to_string());
                    cursor += 1;
                }
                index = cursor;
                // `>`（折叠）把换行并为空格；`|`（字面）保留换行。描述展示上两者
                // 都收敛为单行更稳妥（渐进披露只要一行）。
                Some(block_lines.join(" ").trim().to_string())
            } else {
                index += 1;
                Some(value.trim_matches('"').trim_matches('\'').to_string())
            };
        let Some(value) = block else { continue };
        match key {
            "name" => frontmatter.name = value,
            "description" => frontmatter.description = value,
            // 未知键容忍（frontmatter 可扩展；本消费方只取两键）。
            _ => {}
        }
    }
    let trimmed_name = frontmatter.name.trim();
    frontmatter.name = if trimmed_name.is_empty() {
        fallback_name.to_string()
    } else {
        trimmed_name.to_string()
    };
    frontmatter.description = frontmatter.description.trim().to_string();
    Some(frontmatter)
}

/// 扫描单个技能根目录（其下每个子目录 = 一个技能；同级散置 SKILL.md 也接受）。
/// 返回 name -> SkillResource（后写覆盖，供调用方实现就近优先）。
fn scan_root(root: &Path, source: SkillSource, sink: &mut BTreeMap<String, SkillResource>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return; // 目录不存在/不可读：静默（Pi 发现规则）
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let skill_md = if path.is_dir() {
            path.join("SKILL.md")
        } else if entry.file_name().to_string_lossy() == "SKILL.md" {
            path.clone()
        } else {
            continue;
        };
        let Some(content) = std::fs::read_to_string(&skill_md).ok() else {
            continue; // 读失败：静默跳过
        };
        let fallback = if path.is_dir() {
            entry.file_name().to_string_lossy().into_owned()
        } else {
            root.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let Some(frontmatter) = parse_skill_md(&content, &fallback) else {
            continue; // frontmatter 坏：静默跳过
        };
        if frontmatter.name.is_empty() {
            continue;
        }
        sink.insert(
            frontmatter.name.clone(),
            SkillResource {
                name: frontmatter.name,
                description: frontmatter.description,
                path: skill_md,
                source,
            },
        );
    }
}

/// 两级扫描：global 先入、project 后入（同名覆盖 = 就近优先）。
pub fn scan_skills(global_root: &Path, project_root: Option<&Path>) -> Vec<SkillResource> {
    let mut sink = BTreeMap::new();
    scan_root(global_root, SkillSource::Global, &mut sink);
    if let Some(project_root) = project_root {
        scan_root(project_root, SkillSource::Project, &mut sink);
    }
    sink.into_values().collect()
}

/// 全局技能根（AppData data_dir/skills）。
pub fn global_skills_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("skills")
}

/// 项目技能根（workspace/.r-code/skills）。
pub fn project_skills_dir(workspace: &Path) -> PathBuf {
    workspace.join(".r-code").join("skills")
}

/// 渐进式披露（PRD §4.1 R-EXT-02 / M4-02）：系统提示词只注入名称 + 一行
/// 描述；所选工具集不含读取工具（`read_file`/`load_skill`）时不注入。
pub fn render_skills_disclosure(skills: &[SkillResource], selected_tools: &[&str]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    // 无读取能力就不注入技能列表：模型无法按需取全文，列表只是噪声。
    let can_read = selected_tools
        .iter()
        .any(|tool| matches!(*tool, "read_file" | "load_skill"));
    if !can_read {
        return String::new();
    }
    let lines: Vec<String> = skills
        .iter()
        .map(|skill| {
            if skill.description.is_empty() {
                format!("- {}", skill.name)
            } else {
                format!("- {}: {}", skill.name, skill.description)
            }
        })
        .collect();
    format!(
        "可用技能（仅名称与简介；如需使用，经 load_skill 工具加载全文）：
{}",
        lines.join(
            "
"
        )
    )
}

/// 技能目录缓存（M4-04 热重载）：`scan_cached` 命中即返缓存；
/// `reload` 清缓存重扫拿最新内容（/reload 或设置页触发）。
pub struct SkillCatalog {
    global_root: std::path::PathBuf,
    project_root: Option<std::path::PathBuf>,
    cache: std::cell::RefCell<Option<Vec<SkillResource>>>,
}

impl SkillCatalog {
    pub fn new(global_root: std::path::PathBuf, project_root: Option<std::path::PathBuf>) -> Self {
        Self {
            global_root,
            project_root,
            cache: std::cell::RefCell::new(None),
        }
    }

    /// 命中缓存即返（同 run 内多次渲染披露不吃磁盘）。
    pub fn scan_cached(&self) -> Vec<SkillResource> {
        let mut cache = self.cache.borrow_mut();
        cache
            .get_or_insert_with(|| scan_skills(&self.global_root, self.project_root.as_deref()))
            .clone()
    }

    /// 热重载：清缓存重扫（模块/资源缓存失效），返回最新内容。
    pub fn reload(&self) -> Vec<SkillResource> {
        *self.cache.borrow_mut() = None;
        self.scan_cached()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, dir: &str, frontmatter: &str, body: &str) {
        let dir_path = root.join(dir);
        std::fs::create_dir_all(&dir_path).unwrap();
        std::fs::write(
            dir_path.join("SKILL.md"),
            format!("---\n{frontmatter}---\n\n{body}"),
        )
        .unwrap();
    }

    /// M4-01.A2：frontmatter 解析（name/description；缺省回退目录名）。
    #[test]
    fn frontmatter_parsing() {
        let parsed = parse_skill_md(
            "---\nname: my-skill\ndescription: Does things well.\n---\n\nBody here.",
            "fallback",
        )
        .unwrap();
        assert_eq!(parsed.name, "my-skill");
        assert_eq!(parsed.description, "Does things well.");
        // 缺 name → 目录名。
        let parsed = parse_skill_md("---\ndescription: x\n---\nbody", "dir-name").unwrap();
        assert_eq!(parsed.name, "dir-name");
        // 无 frontmatter / 非 `---` 围栏 → None（坏文件跳过）。
        assert!(parse_skill_md("no frontmatter at all", "d").is_none());
        // 非"键: 值"行 → None（结构性损坏）。
        assert!(parse_skill_md("---\njust some prose\n---\n", "d").is_none());
        // 多行块 description（.agents/skills 实际形态）。
        let block = parse_skill_md(
            "---\nname: multi\ndescription: |\n  第一行说明。\n  第二行说明。\n---\n\n# Skill\n",
            "d",
        )
        .unwrap();
        assert_eq!(block.name, "multi");
        assert!(block.description.contains("第一行说明。"));
        assert!(block.description.contains("第二行说明。"));
        assert!(!block.description.contains('\n'), "渐进披露收敛为单行");
        // BOM 容忍。
        assert!(parse_skill_md("\u{feff}---\nname: bom\ndescription: b\n---\n", "d").is_some());
    }

    /// M4-01.A1：两级扫描——global + project；项目级同名覆盖；按名排序稳定。
    #[test]
    fn two_level_scan_with_project_override() {
        let global = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_skill(
            global.path(),
            "search-web",
            "name: search-web\ndescription: global web\n",
            "g",
        );
        write_skill(
            global.path(),
            "commit-helper",
            "name: commit-helper\ndescription: commits\n",
            "g",
        );
        write_skill(
            project.path(),
            "search-web",
            "name: search-web\ndescription: project web\n",
            "p",
        );
        let skills = scan_skills(global.path(), Some(project.path()));
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["commit-helper", "search-web"], "按名排序");
        let search = skills.iter().find(|s| s.name == "search-web").unwrap();
        assert_eq!(search.description, "project web", "项目级覆盖全局级");
        assert_eq!(search.source, SkillSource::Project);
        let commit = skills.iter().find(|s| s.name == "commit-helper").unwrap();
        assert_eq!(commit.source, SkillSource::Global);
    }

    /// 坏文件静默跳过：无 SKILL.md 的目录、坏 frontmatter、非目录非 md 条目。
    #[test]
    fn broken_files_are_skipped_silently() {
        let global = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(global.path().join("empty-dir")).unwrap();
        write_skill(
            global.path(),
            "good",
            "name: good\ndescription: fine\n",
            "ok",
        );
        // 坏 frontmatter（非键值行）。
        write_skill(global.path(), "bad-yaml", "just prose\n", "x");
        // 散置文件（非目录非 SKILL.md）。
        std::fs::write(global.path().join("README.txt"), "hi").unwrap();
        // 根目录不存在的 project：不报错。
        let skills = scan_skills(global.path(), Some(Path::new("Z:/definitely-missing")));
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "good");
    }

    /// 目录约定：data_dir/skills 与 workspace/.r-code/skills。
    #[test]
    fn directory_conventions() {
        assert_eq!(
            global_skills_dir(Path::new("C:/AppData/r-code")),
            PathBuf::from("C:/AppData/r-code/skills")
        );
        assert_eq!(
            project_skills_dir(Path::new("D:/repo")),
            PathBuf::from("D:/repo/.r-code/skills")
        );
    }

    /// M4-02.A1：渐进披露只注入名称 + 一行描述。
    #[test]
    fn disclosure_renders_name_and_one_line_description_only() {
        let global = tempfile::tempdir().unwrap();
        write_skill(
            global.path(),
            "search-web",
            "name: search-web
description: Search the web.
",
            "body with lots of detail",
        );
        let skills = scan_skills(global.path(), None);
        let text = render_skills_disclosure(&skills, &["read_file", "bash"]);
        assert!(text.contains("- search-web: Search the web."));
        assert!(!text.contains("body with lots of detail"), "正文不进披露");
        // 空描述退化为只列名。
        write_skill(
            global.path(),
            "bare",
            "name: bare
description:
",
            "x",
        );
        let skills = scan_skills(global.path(), None);
        let text = render_skills_disclosure(&skills, &["read_file"]);
        assert!(text.lines().any(|line| line.trim_end() == "- bare"));
    }

    /// M4-02.A2：所选工具集不含 read 时不注入技能列表。
    #[test]
    fn disclosure_skipped_without_read_tool() {
        let global = tempfile::tempdir().unwrap();
        write_skill(
            global.path(),
            "search-web",
            "name: search-web
description: Search the web.
",
            "b",
        );
        let skills = scan_skills(global.path(), None);
        // 无任何读取工具：不注入。
        assert_eq!(render_skills_disclosure(&skills, &["bash", "glob"]), "");
        // load_skill 在场也算读取能力（按需加载全文的通道）。
        assert!(!render_skills_disclosure(&skills, &["load_skill", "bash"]).is_empty());
        // 空技能清单：不注入。
        assert_eq!(render_skills_disclosure(&[], &["read_file"]), "");
    }

    /// M4-04.A1/A2：缓存 + 热重载——cached 不吃磁盘变化，reload 拿最新内容。
    #[test]
    fn catalog_cache_and_reload_see_fresh_content() {
        let global = tempfile::tempdir().unwrap();
        write_skill(
            global.path(),
            "first",
            "name: first
description: v1
",
            "b",
        );
        let catalog = SkillCatalog::new(global.path().to_path_buf(), None);
        let first = catalog.scan_cached();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].description, "v1");

        // 磁盘更新：缓存仍返回旧值（这就是需要 reload 的原因）。
        write_skill(
            global.path(),
            "first",
            "name: first
description: v2
",
            "b",
        );
        write_skill(
            global.path(),
            "second",
            "name: second
description: new
",
            "b",
        );
        assert_eq!(catalog.scan_cached().len(), 1, "缓存不吃磁盘");

        // 热重载：清缓存重扫，拿到最新内容（v2 + second）。
        let reloaded = catalog.reload();
        assert_eq!(reloaded.len(), 2);
        let first = reloaded.iter().find(|s| s.name == "first").unwrap();
        assert_eq!(first.description, "v2");
        assert!(reloaded.iter().any(|s| s.name == "second"));
        // reload 后 cached 与最新一致。
        assert_eq!(catalog.scan_cached().len(), 2);
    }

    /// M4-01.A3：与 .agents/skills 语义统一——仓库构建期资产（.agents/skills）
    /// 的 frontmatter 形状可被同一解析器消费。
    #[test]
    fn agents_skills_frontmatter_is_parseable() {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let agents_skills = repo_root.join(".agents").join("skills");
        let Ok(entries) = std::fs::read_dir(&agents_skills) else {
            // `.agents` 是 git submodule：干净 checkout 未初始化时缺席——
            // 语义统一测试只在资产在场时强校验（skip 而非 fail）。
            eprintln!("[skill_resources] .agents/skills 未初始化（submodule），跳过");
            return;
        };
        let mut parsed = 0;
        for entry in entries.flatten() {
            let skill_md = entry.path().join("SKILL.md");
            let Ok(content) = std::fs::read_to_string(&skill_md) else {
                continue;
            };
            let fallback = entry.file_name().to_string_lossy().into_owned();
            let frontmatter = parse_skill_md(&content, &fallback)
                .unwrap_or_else(|| panic!("{} 的 frontmatter 必须可解析", fallback));
            assert!(!frontmatter.name.is_empty());
            parsed += 1;
        }
        assert!(parsed > 0, ".agents/skills 下应有可解析技能");
    }
}
