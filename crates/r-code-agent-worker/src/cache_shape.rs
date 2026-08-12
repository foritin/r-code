//! P2-H（docs/archive/deepseek-prefix-cache.md §5 P2-H）：DeepSeek 前缀缓存形状归因。
//!
//! 每轮请求前捕获可缓存前缀的指纹（system 哈希、tools 哈希、历史改写版本号），
//! 请求后比对，把缓存变化归因到具体原因（system/tools/压缩/修复/工作区/委派开关），
//! 供命中率观测与守卫测试使用。
//!
//! 对齐 Reasonix `internal/agent/cache_shape.go`（`cache_shape.go:66-73` 归因规则）：
//! **仅"真正改写 provider 可见字节"的操作上报缓存变化**；纯本地元数据（决策回执、
//! preview 替换、Edited 消息替换）bump 版本号但**不算 miss**。r-code 用两个独立
//! 版本号表达这一区别：
//! - `provider_visible_version`：参与归因（变化 → `Rewrite`）；
//! - `local_metadata_version`：只记录、不上报（变化 → `None`）。

use hermes_core::ToolSpec;

/// 请求可缓存前缀的指纹（PrefixShape）。
///
/// system/tools 各自独立哈希，改写版本号分 provider 可见与纯本地两档。
/// 相同 shape ⇒ 前缀逐字节稳定 ⇒ 服务端前缀缓存可复用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixShape {
    /// system 文本指纹（FNV-1a 64，确定性：同输入同输出，跨进程/重启稳定）。
    pub system_hash: u64,
    /// tools 列表指纹：先按 name/description/input_schema 归一排序再哈希，
    /// 顺序漂移（A4：HashMap 迭代序）不产生伪变化。
    pub tools_hash: u64,
    /// provider 可见改写版本号：参与归因，变化即缓存重置点
    /// （压缩、修复、workspace 切换等对 provider 可见字节的改写）。
    pub provider_visible_version: u64,
    /// 纯本地元数据版本号：决策回执、preview 替换等**不进请求体**的编辑，
    /// 只记录不上报（cache_shape.go:66-73：bare 版本变化不算缓存变化）。
    pub local_metadata_version: u64,
    /// 工具 schema 序列化的估算 token 数（诊断展示用；`len/4` 启发式，
    /// 对齐 Reasonix `estimateTokens`）。
    pub tool_schema_tokens: Option<u32>,
}

impl PrefixShape {
    /// 空形状：表示"上一轮未知"（首次捕获前）。`compare` 对全 0 的 prev
    /// 返回 `None`——没有历史可比，不产生伪归因（对应 Go 版
    /// `prev.SystemHash != ""` 的跳过语义，u64 用 0 作哨兵）。
    pub fn empty() -> Self {
        Self {
            system_hash: 0,
            tools_hash: 0,
            provider_visible_version: 0,
            local_metadata_version: 0,
            tool_schema_tokens: None,
        }
    }

    /// 是否为占位空形状。
    pub fn is_empty(&self) -> bool {
        *self == Self::empty()
    }
}

/// 缓存变化归因原因。
///
/// 枚举保留 Workspace / Memory / Delegation 三个细分变体，但 `compare` 默认把
/// 版本类变化统一归为 [`CacheChangeCause::Rewrite`]——r-code 当前没有把三类
/// 改写编码进版本号的机制，调用方如需要细分，可按版本号段自行区分
/// （例如 `provider_visible_version` 高位段编码类别），或直接使用
/// [`compare_with_rewrite_cause`] 传入已知原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CacheChangeCause {
    /// system 文本变化（provider 可见字节被改写）。
    System,
    /// tools 列表变化（名称/描述/schema/来源/顺序归一后仍不同）。
    Tools,
    /// 历史改写（压缩、异常修复、workspace 切换、委派开关等版本类重置点）。
    Rewrite,
    /// workspace attach/detach 引起的合法重置点（保留，供调用方细分）。
    Workspace,
    /// memory 跨 run 变化引起的合法重置点（保留，供调用方细分）。
    Memory,
    /// 委派提示开关变化引起的合法重置点（保留，供调用方细分）。
    Delegation,
    /// 无变化——或仅纯本地元数据变化（不上报缓存变化）。
    None,
}

impl CacheChangeCause {
    /// 是否属于会上报的缓存变化（`None` 不上报；本地元数据变化归为 `None`）。
    pub fn is_cache_change(self) -> bool {
        !matches!(self, Self::None)
    }
}

impl std::fmt::Display for CacheChangeCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::System => "system",
            Self::Tools => "tools",
            Self::Rewrite => "rewrite",
            Self::Workspace => "workspace",
            Self::Memory => "memory",
            Self::Delegation => "delegation",
            Self::None => "none",
        };
        f.write_str(s)
    }
}

/// 从请求侧输入捕获前缀形状。
///
/// `system` 为 system 文本；`tools` 为发给 provider 的工具规格列表
/// （内部先归一排序再哈希，顺序无关）；`provider_visible_version` 与
/// `local_metadata_version` 为两个独立改写版本号（见模块文档）。
pub fn capture(
    system: &str,
    tools: &[ToolSpec],
    provider_visible_version: u64,
    local_metadata_version: u64,
) -> PrefixShape {
    let system_hash = fnv1a(system.as_bytes());
    let (tools_hash, schema_tokens) = hash_tools(tools);
    PrefixShape {
        system_hash,
        tools_hash,
        provider_visible_version,
        local_metadata_version,
        tool_schema_tokens: Some(schema_tokens),
    }
}

/// 比对两个形状，返回变化原因。
///
/// 归因优先级（对齐 Reasonix `CompareShape`）：
/// 1. system 哈希变化 → [`CacheChangeCause::System`]；
/// 2. tools 哈希变化 → [`CacheChangeCause::Tools`]；
/// 3. `provider_visible_version` 变化且其余相同 → [`CacheChangeCause::Rewrite`]；
/// 4. 其余（含仅 `local_metadata_version` 变化）→ [`CacheChangeCause::None`]。
///
/// `prev` 为 `PrefixShape::empty()`（未知历史）时返回 `None`。
pub fn compare(prev: &PrefixShape, cur: &PrefixShape) -> CacheChangeCause {
    if prev.is_empty() {
        return CacheChangeCause::None;
    }
    if prev.system_hash != cur.system_hash {
        return CacheChangeCause::System;
    }
    if prev.tools_hash != cur.tools_hash {
        return CacheChangeCause::Tools;
    }
    if prev.provider_visible_version != cur.provider_visible_version {
        return CacheChangeCause::Rewrite;
    }
    // local_metadata_version 变化（或一切相同）不算缓存变化：
    // 纯本地元数据不进入 provider 可见字节（cache_shape.go:66-73）。
    CacheChangeCause::None
}

/// 比对两个形状，并在版本类变化时使用调用方提供的细分原因。
///
/// 当 system/tools 相同而 `provider_visible_version` 变化时，返回
/// `rewrite_cause`（若它本身是 `None` 或 `Rewrite` 则回落为 `Rewrite`）；
/// 其余情形与 [`compare`] 一致。用于调用方已知改写类别
/// （workspace / memory / delegation）的场景。
pub fn compare_with_rewrite_cause(
    prev: &PrefixShape,
    cur: &PrefixShape,
    rewrite_cause: CacheChangeCause,
) -> CacheChangeCause {
    match compare(prev, cur) {
        CacheChangeCause::Rewrite
            if matches!(
                rewrite_cause,
                CacheChangeCause::Workspace
                    | CacheChangeCause::Memory
                    | CacheChangeCause::Delegation
            ) =>
        {
            rewrite_cause
        }
        other => other,
    }
}

/// FNV-1a 64 位哈希：确定性、零依赖、跨进程稳定。
///
/// 不用 `std::collections::hash_map::DefaultHasher`（RandomState 每次进程随机，
/// 重启后同输入不同输出，不利 A4 类"重启后漂移"排查与快照比对）。
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a(bytes: &[u8]) -> u64 {
    fnv1a_continue(FNV_OFFSET, bytes)
}

/// 从既有状态继续吸收字节（供多段累积哈希使用）。
fn fnv1a_continue(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// 归一排序后的 tools 指纹与 schema token 估算。
///
/// 排序键对齐 Reasonix `normalizeToolSchemas`（name → description →
/// input_schema 字符串）；哈希覆盖完整 ToolSpec（任何字段变化都归因 Tools，
/// 宁可保守不漏报）。schema tokens 取整个排序后 tools JSON 的 `len/4`，
/// 对齐 Reasonix `estimateTokens(string(toolsJSON))`。
fn hash_tools(tools: &[ToolSpec]) -> (u64, u32) {
    if tools.is_empty() {
        return (fnv1a(b""), 0);
    }
    let mut normalized: Vec<&ToolSpec> = tools.iter().collect();
    normalized.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.description.cmp(&b.description))
            .then_with(|| schema_json(&a.input_schema).cmp(&schema_json(&b.input_schema)))
    });
    let mut hasher = FnvHasher::new();
    for tool in &normalized {
        let json = serde_json::to_string(tool).unwrap_or_default();
        hasher.write(json.as_bytes());
        hasher.write(&[0xff]); // 条目分隔符，避免 "ab"+"c" 与 "a"+"bc" 混淆
    }
    let tokens = {
        let json = serde_json::to_string(&normalized).unwrap_or_default();
        (json.len() / 4) as u32
    };
    (hasher.finish(), tokens)
}

/// `input_schema` 的稳定字符串形态（serde_json 默认无 preserve_order，
/// Object 键按字典序序列化，字节稳定——PRD A12）。
fn schema_json(schema: &serde_json::Value) -> String {
    serde_json::to_string(schema).unwrap_or_default()
}

/// 轻量 FNV-1a 累加器。
struct FnvHasher(u64);

impl FnvHasher {
    fn new() -> Self {
        Self(FNV_OFFSET)
    }

    fn write(&mut self, bytes: &[u8]) {
        self.0 = fnv1a_continue(self.0, bytes);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::ToolSource;

    fn tool(name: &str, description: &str, schema: serde_json::Value) -> ToolSpec {
        ToolSpec {
            name: name.to_string(),
            description: description.to_string(),
            input_schema: schema,
            source: ToolSource::Builtin,
            requires_confirmation: false,
        }
    }

    fn shape(
        system: &str,
        tools: &[ToolSpec],
        provider_visible_version: u64,
        local_metadata_version: u64,
    ) -> PrefixShape {
        capture(
            system,
            tools,
            provider_visible_version,
            local_metadata_version,
        )
    }

    const SYSTEM_A: &str = "you are r-code, a coding agent.";
    const SYSTEM_B: &str = "you are r-code, a coding agent. (changed)";

    fn sample_tools() -> Vec<ToolSpec> {
        vec![
            tool(
                "read_file",
                "read a file",
                serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            ),
            tool(
                "bash",
                "run a shell command",
                serde_json::json!({"type": "object", "properties": {"cmd": {"type": "string"}}}),
            ),
        ]
    }

    #[test]
    fn identical_shapes_attribute_to_none() {
        let a = shape(SYSTEM_A, &sample_tools(), 1, 2);
        let b = shape(SYSTEM_A, &sample_tools(), 1, 2);
        assert_eq!(compare(&a, &b), CacheChangeCause::None);
        assert!(!compare(&a, &b).is_cache_change());
    }

    #[test]
    fn system_change_attributes_to_system() {
        let a = shape(SYSTEM_A, &sample_tools(), 1, 0);
        let b = shape(SYSTEM_B, &sample_tools(), 1, 0);
        assert_eq!(compare(&a, &b), CacheChangeCause::System);
        assert!(compare(&a, &b).is_cache_change());
    }

    #[test]
    fn tools_change_attributes_to_tools() {
        let base = sample_tools();
        // 名称变化
        let renamed = vec![
            tool(
                "read_file_renamed",
                "read a file",
                serde_json::json!({"type": "object"}),
            ),
            tool(
                "bash",
                "run a shell command",
                serde_json::json!({"type": "object"}),
            ),
        ];
        assert_eq!(
            compare(
                &shape(SYSTEM_A, &base, 1, 0),
                &shape(SYSTEM_A, &renamed, 1, 0)
            ),
            CacheChangeCause::Tools
        );
        // schema 变化
        let schema_changed = vec![
            tool(
                "read_file",
                "read a file",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "extra": {"type": "number"}
                    }
                }),
            ),
            tool(
                "bash",
                "run a shell command",
                serde_json::json!({"type": "object"}),
            ),
        ];
        assert_eq!(
            compare(
                &shape(SYSTEM_A, &base, 1, 0),
                &shape(SYSTEM_A, &schema_changed, 1, 0)
            ),
            CacheChangeCause::Tools
        );
        // 描述变化
        let desc_changed = vec![
            tool(
                "read_file",
                "read a file NOW",
                serde_json::json!({"type": "object"}),
            ),
            tool(
                "bash",
                "run a shell command",
                serde_json::json!({"type": "object"}),
            ),
        ];
        assert_eq!(
            compare(
                &shape(SYSTEM_A, &base, 1, 0),
                &shape(SYSTEM_A, &desc_changed, 1, 0)
            ),
            CacheChangeCause::Tools
        );
    }

    #[test]
    fn tools_order_does_not_change_the_hash() {
        // P1-C：tools 按名排序输出——顺序漂移（HashMap 迭代序、重启随机）
        // 不得产生伪归因。
        let mut reversed = sample_tools();
        reversed.reverse();
        assert_ne!(reversed[0].name, sample_tools()[0].name);
        let a = shape(SYSTEM_A, &sample_tools(), 1, 0);
        let b = shape(SYSTEM_A, &reversed, 1, 0);
        assert_eq!(a.tools_hash, b.tools_hash);
        assert_eq!(compare(&a, &b), CacheChangeCause::None);
    }

    #[test]
    fn rewrite_version_change_attributes_to_rewrite() {
        let a = shape(SYSTEM_A, &sample_tools(), 1, 0);
        let b = shape(SYSTEM_A, &sample_tools(), 2, 0);
        assert_eq!(compare(&a, &b), CacheChangeCause::Rewrite);
        assert!(compare(&a, &b).is_cache_change());
    }

    #[test]
    fn local_metadata_version_change_is_not_reported() {
        // cache_shape.go:66-73：bare 版本变化（决策回执、preview 替换等
        // 纯本地元数据编辑）不算缓存变化。
        let a = shape(SYSTEM_A, &sample_tools(), 1, 0);
        let b = shape(SYSTEM_A, &sample_tools(), 1, 7);
        assert_eq!(compare(&a, &b), CacheChangeCause::None);
        assert!(!compare(&a, &b).is_cache_change());
    }

    #[test]
    fn rewrite_cause_subdivision_works() {
        let a = shape(SYSTEM_A, &sample_tools(), 1, 0);
        let b = shape(SYSTEM_A, &sample_tools(), 2, 0);
        assert_eq!(
            compare_with_rewrite_cause(&a, &b, CacheChangeCause::Workspace),
            CacheChangeCause::Workspace
        );
        assert_eq!(
            compare_with_rewrite_cause(&a, &b, CacheChangeCause::Memory),
            CacheChangeCause::Memory
        );
        assert_eq!(
            compare_with_rewrite_cause(&a, &b, CacheChangeCause::Delegation),
            CacheChangeCause::Delegation
        );
        // 传入 None 时回落为 Rewrite；非版本变化不受细分影响
        assert_eq!(
            compare_with_rewrite_cause(&a, &b, CacheChangeCause::None),
            CacheChangeCause::Rewrite
        );
        assert_eq!(
            compare_with_rewrite_cause(&a, &a, CacheChangeCause::Workspace),
            CacheChangeCause::None
        );
    }

    #[test]
    fn empty_prev_is_not_attributed() {
        let cur = shape(SYSTEM_A, &sample_tools(), 3, 9);
        assert_eq!(compare(&PrefixShape::empty(), &cur), CacheChangeCause::None);
        assert!(PrefixShape::empty().is_empty());
    }

    #[test]
    fn capture_hashes_are_deterministic_and_tokens_estimated() {
        let a = shape(SYSTEM_A, &sample_tools(), 1, 0);
        let b = shape(SYSTEM_A, &sample_tools(), 1, 0);
        assert_eq!(a.system_hash, b.system_hash);
        assert_eq!(a.tools_hash, b.tools_hash);
        assert_ne!(a.system_hash, a.tools_hash);
        let tokens = a.tool_schema_tokens.expect("tools 非空时必有估算");
        assert!(tokens > 0, "schema tokens 应为正数，实际 {tokens}");
        // 空 tools → 0 token、None 以外的确定性值
        let empty = shape(SYSTEM_A, &[], 1, 0);
        assert_eq!(empty.tool_schema_tokens, Some(0));
        // 不同 system 哈希不同
        assert_ne!(
            a.system_hash,
            shape(SYSTEM_B, &sample_tools(), 1, 0).system_hash
        );
    }
}
