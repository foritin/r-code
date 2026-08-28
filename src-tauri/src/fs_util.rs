//! 配置文件原子写（F-obs-05）：主配置 / feature flags / MCP settings 共用。
//!
//! 裸 `std::fs::write` 在写一半崩溃（断电/panic/磁盘满）会留下截断的 TOML，
//! 下次启动解析失败且无备份可回滚——任务启动与设置页全部失效。同仓
//! mcp_settings.rs 已有 temp+fsync+rename 范式，此处收敢单一实现供全部
//! 配置持久化路径复用。

use std::io::Write;
use std::path::Path;

use r_code_core::error::ProductError;

/// 原子替换 `path` 内容：同目录临时文件 → write → fsync → rename。
///
/// 任一步失败时原文件保持完整；rename 在同一卷上原子。
pub(crate) fn atomic_write(path: &Path, content: &[u8]) -> Result<(), ProductError> {
    let parent = path.parent().ok_or_else(|| {
        ProductError::ConfigError(format!("path has no parent: {}", path.display()))
    })?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(content)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| ProductError::from(error.error))?;
    Ok(())
}
