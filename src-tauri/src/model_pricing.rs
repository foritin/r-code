//! 分层定价与思考等级映射（docs/pi-alignment PRD §4.1 R-PRV-04 / M1-04）。
//!
//! 两个声明式模型级配置，随 M1-02 的 provider 声明（provider-decls.toml）
//! 携带，服务于 `usage_json` 成本归因与思考档位切换：
//!
//! - `cost.tiers`：分层定价。判据 = `input + cacheRead + cacheWrite`；命中的
//!   tier **整套替换**费率（不是逐段拼装），且对整个请求适用；多个 tier 的
//!   阈值同时满足时**最高阈值胜出**（长上下文档比基础档贵）。
//! - `cost.thinking_level_map` + `cost.hidden_thinking_levels`：UI 思考档位 →
//!   wire 值的三态映射——省略（档位不在 map 里，不发字段）/ 字符串（发指定
//!   值）/ null（档位对该模型不存在，UI 隐藏、切换跳过）。
//!
//! 本模块只含纯语义（可单测），IO 与声明解析在 provider_decl / commands。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 单层费率（USD / 百万 token，四桶齐全 = 整套替换的最小单位）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CostRates {
    pub input_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub cache_write_per_mtok: f64,
    pub output_per_mtok: f64,
}

/// 一层定价。`threshold_tokens = None` 是基础档（判据再小也适用）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostTier {
    /// 展示名（声明表键名）。
    pub name: String,
    /// 判据达到该阈值才适用；None = 基础档。
    pub threshold_tokens: Option<u64>,
    pub rates: CostRates,
}

/// 声明式成本配置（provider-decls.toml 的 `[decls.<name>.cost]`）。
///
/// serde 形态是 [`DeclCostRaw`]（tier 名做 TOML 表键；费率平铺），load 时经
/// `TryFrom` 校验费率非负有限。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "DeclCostRaw", into = "DeclCostRaw")]
pub struct DeclCost {
    /// 空 = 无定价（usage_json 不做成本归因）。
    pub tiers: Vec<CostTier>,
    /// UI 档位 → wire 字符串。档位缺省（不在 map）= 省略该字段。
    pub thinking_level_map: BTreeMap<String, String>,
    /// null 档（该模型不存在的档位）：UI 隐藏、切换跳过。
    pub hidden_thinking_levels: Vec<String>,
}

impl DeclCost {
    pub fn is_empty(&self) -> bool {
        self.tiers.is_empty()
            && self.thinking_level_map.is_empty()
            && self.hidden_thinking_levels.is_empty()
    }
}

/// `[cost]` 表的 TOML 形态。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeclCostRaw {
    /// `<tier-name>` → 该层定价；无 `threshold_tokens` 键 = 基础档。
    #[serde(default)]
    pub tiers: BTreeMap<String, CostTierRaw>,
    /// UI 档位 → wire 字符串；缺省档位 = 省略态。
    #[serde(default)]
    pub thinking_level_map: BTreeMap<String, String>,
    /// null 档（该模型不存在）：UI 隐藏、切换跳过。
    #[serde(default)]
    pub hidden_thinking_levels: Vec<String>,
}

/// 单层定价的 TOML 形态（费率平铺，阈值可选）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostTierRaw {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_tokens: Option<u64>,
    pub input_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub cache_write_per_mtok: f64,
    pub output_per_mtok: f64,
}

impl TryFrom<DeclCostRaw> for DeclCost {
    type Error = String;

    fn try_from(raw: DeclCostRaw) -> Result<Self, String> {
        let mut tiers = Vec::with_capacity(raw.tiers.len());
        for (name, tier) in &raw.tiers {
            let rates = CostRates {
                input_per_mtok: tier.input_per_mtok,
                cache_read_per_mtok: tier.cache_read_per_mtok,
                cache_write_per_mtok: tier.cache_write_per_mtok,
                output_per_mtok: tier.output_per_mtok,
            };
            // 负数/非有限费率是声明错误：load 时报错（进文件级诊断），不静默。
            let sane = [
                rates.input_per_mtok,
                rates.cache_read_per_mtok,
                rates.cache_write_per_mtok,
                rates.output_per_mtok,
            ]
            .iter()
            .all(|value| value.is_finite() && *value >= 0.0);
            if !sane {
                return Err(format!("cost tier '{name}': 费率必须是非负有限数"));
            }
            tiers.push(CostTier {
                name: name.clone(),
                threshold_tokens: tier.threshold_tokens,
                rates,
            });
        }
        Ok(DeclCost {
            tiers,
            thinking_level_map: raw.thinking_level_map,
            hidden_thinking_levels: raw.hidden_thinking_levels,
        })
    }
}

impl From<DeclCost> for DeclCostRaw {
    fn from(cost: DeclCost) -> Self {
        DeclCostRaw {
            tiers: cost
                .tiers
                .into_iter()
                .map(|tier| {
                    (
                        tier.name,
                        CostTierRaw {
                            threshold_tokens: tier.threshold_tokens,
                            input_per_mtok: tier.rates.input_per_mtok,
                            cache_read_per_mtok: tier.rates.cache_read_per_mtok,
                            cache_write_per_mtok: tier.rates.cache_write_per_mtok,
                            output_per_mtok: tier.rates.output_per_mtok,
                        },
                    )
                })
                .collect(),
            thinking_level_map: cost.thinking_level_map,
            hidden_thinking_levels: cost.hidden_thinking_levels,
        }
    }
}

/// tier 判据：input + cacheRead + cacheWrite（缺桶按 0 计）。
/// 返回 None 表示 usage map 里连一个判据桶都没有（无从归因）。
pub fn tier_criterion(map: &serde_json::Map<String, serde_json::Value>) -> Option<u64> {
    let bucket = |key: &str| {
        map.get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    if !map.contains_key("input_tokens")
        && !map.contains_key("cache_read_tokens")
        && !map.contains_key("cache_write_tokens")
    {
        return None;
    }
    Some(bucket("input_tokens") + bucket("cache_read_tokens") + bucket("cache_write_tokens"))
}

/// 选层：阈值 ≤ 判据的 tier 中**最高阈值胜出**；基础档（None 阈值）兜底。
/// 空表返回 None；表里只有带阈值的高档且判据未达阈值时，取表中最低档
/// （声明不完整时宁可按最接近的档计，不静默归零）。
pub fn select_tier(tiers: &[CostTier], criterion: u64) -> Option<&CostTier> {
    if tiers.is_empty() {
        return None;
    }
    let rank = |tier: &CostTier| tier.threshold_tokens.unwrap_or(0);
    tiers
        .iter()
        .filter(|tier| rank(tier) <= criterion)
        .max_by_key(|tier| rank(tier))
        .or_else(|| tiers.iter().min_by_key(|tier| rank(tier)))
}

/// 按整套费率算一次请求成本（USD）。判据三桶用 tier 费率、输出桶单列。
pub fn cost_usd(tier: &CostTier, map: &serde_json::Map<String, serde_json::Value>) -> f64 {
    let bucket = |key: &str| {
        map.get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0) as f64
    };
    let rates = &tier.rates;
    (bucket("input_tokens") * rates.input_per_mtok
        + bucket("cache_read_tokens") * rates.cache_read_per_mtok
        + bucket("cache_write_tokens") * rates.cache_write_per_mtok
        + bucket("output_tokens") * rates.output_per_mtok)
        / 1_000_000.0
}

/// 成本归因接入 usage_json：判据 → 选层 → 整套费率，把 `cost_usd`（美元，
/// 6 位小数）写进 map。usage 是 run 级累计值，每次事件覆盖式重算；无定价
/// 或无判据桶时不动 map（返回 false）。
pub fn attribute_cost(
    map: &mut serde_json::Map<String, serde_json::Value>,
    cost: &DeclCost,
) -> bool {
    let Some(criterion) = tier_criterion(map) else {
        return false;
    };
    let Some(tier) = select_tier(&cost.tiers, criterion) else {
        return false;
    };
    let usd = (cost_usd(tier, map) * 1e6).round() / 1e6;
    map.insert("cost_usd".to_string(), serde_json::json!(usd));
    true
}

/// 思考档位的三态 wire 值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThinkingWire {
    /// 省略：档位不在 map 里，请求不发 thinking/reasoning 字段。
    Omit,
    /// 字符串：发 map 里指定的 wire 值。
    Level(String),
    /// null：该档位对该模型不存在（hidden），UI 隐藏、切换跳过。
    Null,
}

/// 解析一个 UI 档位的三态映射。
pub fn thinking_wire(cost: &DeclCost, ui_level: &str) -> ThinkingWire {
    if cost
        .hidden_thinking_levels
        .iter()
        .any(|level| level == ui_level)
    {
        return ThinkingWire::Null;
    }
    match cost.thinking_level_map.get(ui_level) {
        Some(value) => ThinkingWire::Level(value.clone()),
        None => ThinkingWire::Omit,
    }
}

/// 档位切换序列：跳过 null 档（该模型不存在），省略档保留（不发字段即关闭）。
/// 返回仍可切换的档位（保持传入顺序；空序列原样返回）。
pub fn cyclable_levels<'a>(cost: &DeclCost, levels: &[&'a str]) -> Vec<&'a str> {
    levels
        .iter()
        .copied()
        .filter(|level| thinking_wire(cost, level) != ThinkingWire::Null)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rates(v: f64) -> CostRates {
        CostRates {
            input_per_mtok: v,
            cache_read_per_mtok: v,
            cache_write_per_mtok: v,
            output_per_mtok: v,
        }
    }

    fn usage_map(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        value.as_object().unwrap().clone()
    }

    /// M1-04.A1：判据 = input+cacheRead+cacheWrite；最高阈值胜出；整套替换。
    #[test]
    fn highest_applicable_threshold_wins() {
        let tiers = vec![
            CostTier {
                name: "base".to_string(),
                threshold_tokens: None,
                rates: rates(1.0),
            },
            CostTier {
                name: "over-128k".to_string(),
                threshold_tokens: Some(128_000),
                rates: rates(2.0),
            },
            CostTier {
                name: "over-1m".to_string(),
                threshold_tokens: Some(1_000_000),
                rates: rates(4.0),
            },
        ];
        // 判据 130k（input 50k + read 60k + write 20k）：128k 档生效，1m 不满足。
        let criterion = 50_000 + 60_000 + 20_000;
        assert_eq!(select_tier(&tiers, criterion).unwrap().name, "over-128k");
        // 判据低于一切阈值：基础档兜底。
        assert_eq!(select_tier(&tiers, 100).unwrap().name, "base");
        // 判据越过 1m：最高阈值胜出。
        assert_eq!(select_tier(&tiers, 2_000_000).unwrap().name, "over-1m");
        // 声明不完整（无基础档）且判据未达最低阈值：取最低档，不静默归零。
        let high_only = vec![CostTier {
            name: "over-128k".to_string(),
            threshold_tokens: Some(128_000),
            rates: rates(2.0),
        }];
        assert_eq!(select_tier(&high_only, 100).unwrap().name, "over-128k");
        assert!(select_tier(&[], 100).is_none());
    }

    /// M1-04.A1（续）：整套替换——判据三桶全部按命中 tier 的费率计，输出桶
    /// 同 tier；不出现"前 128k 按基础费率、超出部分按高档"的拼装。
    #[test]
    fn whole_request_uses_single_tier_rates() {
        let tiers = vec![
            CostTier {
                name: "base".to_string(),
                threshold_tokens: None,
                rates: CostRates {
                    input_per_mtok: 1.0,
                    cache_read_per_mtok: 0.1,
                    cache_write_per_mtok: 0.5,
                    output_per_mtok: 2.0,
                },
            },
            CostTier {
                name: "over-100".to_string(),
                threshold_tokens: Some(100),
                rates: CostRates {
                    input_per_mtok: 3.0,
                    cache_read_per_mtok: 0.3,
                    cache_write_per_mtok: 1.5,
                    output_per_mtok: 6.0,
                },
            },
        ];
        // 判据 = 60 + 30 + 20 = 110 ≥ 100 → 高档整套装载。
        let map = usage_map(json!({
            "input_tokens": 60,
            "cache_read_tokens": 30,
            "cache_write_tokens": 20,
            "output_tokens": 10,
        }));
        let tier = select_tier(&tiers, 110).unwrap();
        let usd = cost_usd(tier, &map);
        // (60*3 + 30*0.3 + 20*1.5 + 10*6) / 1e6 = (180+9+30+60)/1e6。
        assert!((usd - 279.0 / 1_000_000.0).abs() < 1e-12);
        // 判据 90 → 基础档整套装载：同一桶结构另一套费率。
        let map_low = usage_map(json!({
            "input_tokens": 60,
            "cache_read_tokens": 20,
            "cache_write_tokens": 10,
            "output_tokens": 10,
        }));
        let base = select_tier(&tiers, 90).unwrap();
        assert!((cost_usd(base, &map_low) - (60.0 + 2.0 + 5.0 + 20.0) / 1_000_000.0).abs() < 1e-12);
    }

    /// M1-04.A1（续）：usage_json 归因——cost_usd 写入累计 map；无判据桶/无定价不动。
    #[test]
    fn attribute_cost_merges_into_usage_map() {
        let cost = DeclCost {
            tiers: vec![CostTier {
                name: "base".to_string(),
                threshold_tokens: None,
                rates: CostRates {
                    input_per_mtok: 1_000_000.0, // 1 USD / token → 便于断言
                    cache_read_per_mtok: 0.0,
                    cache_write_per_mtok: 0.0,
                    output_per_mtok: 0.0,
                },
            }],
            ..DeclCost::default()
        };
        let mut map = usage_map(json!({"input_tokens": 3, "output_tokens": 5}));
        assert!(attribute_cost(&mut map, &cost));
        assert_eq!(map.get("cost_usd").unwrap().as_f64().unwrap(), 3.0);
        // 覆盖式重算：usage 累计到 7 后再次归因取新值。
        map.insert("input_tokens".to_string(), json!(7));
        assert!(attribute_cost(&mut map, &cost));
        assert_eq!(map.get("cost_usd").unwrap().as_f64().unwrap(), 7.0);
        // 无判据桶（只有 output）：无从归因。
        let mut output_only = usage_map(json!({"output_tokens": 5}));
        assert!(!attribute_cost(&mut output_only, &cost));
        assert!(output_only.get("cost_usd").is_none());
        // 无定价：不动 map。
        let mut no_tiers = usage_map(json!({"input_tokens": 3}));
        assert!(!attribute_cost(&mut no_tiers, &DeclCost::default()));
    }

    /// M1-04.A2：三态映射——省略（缺省）/字符串/null（hidden 隐藏）。
    #[test]
    fn thinking_level_map_three_states() {
        let cost = DeclCost {
            thinking_level_map: BTreeMap::from([
                ("low".to_string(), "low".to_string()),
                ("medium".to_string(), "high".to_string()),
            ]),
            hidden_thinking_levels: vec!["high".to_string()],
            ..DeclCost::default()
        };
        assert_eq!(
            thinking_wire(&cost, "low"),
            ThinkingWire::Level("low".to_string())
        );
        // 字符串态允许重映射（medium → wire high）。
        assert_eq!(
            thinking_wire(&cost, "medium"),
            ThinkingWire::Level("high".to_string())
        );
        // 缺省档 = 省略：不发字段。
        assert_eq!(thinking_wire(&cost, "off"), ThinkingWire::Omit);
        // hidden 档 = null：UI 隐藏。
        assert_eq!(thinking_wire(&cost, "high"), ThinkingWire::Null);
    }

    /// M1-04.A2（续）：null 档切换跳过——循环序列里不可出现的档位被剔除。
    #[test]
    fn cyclable_levels_skip_null_tiers() {
        let cost = DeclCost {
            hidden_thinking_levels: vec!["high".to_string()],
            ..DeclCost::default()
        };
        let levels = vec!["off", "low", "medium", "high"];
        assert_eq!(
            cyclable_levels(&cost, &levels),
            vec!["off", "low", "medium"]
        );
        // 无 hidden：全量保留。
        assert_eq!(
            cyclable_levels(&DeclCost::default(), &levels),
            vec!["off", "low", "medium", "high"],
        );
    }

    /// M1-04.A3 的语义面：判据漏计 cacheWrite 会选错层——判据必须三桶齐全。
    #[test]
    fn criterion_counts_cache_write_not_input_only() {
        let tiers = vec![
            CostTier {
                name: "base".to_string(),
                threshold_tokens: None,
                rates: rates(1.0),
            },
            CostTier {
                name: "over-100".to_string(),
                threshold_tokens: Some(100),
                rates: rates(2.0),
            },
        ];
        // input 90 + write 20 = 110 ≥ 100 → 高档；只看 input 会错选基础档。
        let map = usage_map(json!({
            "input_tokens": 90,
            "cache_write_tokens": 20,
            "output_tokens": 1,
        }));
        let criterion = tier_criterion(&map).unwrap();
        assert_eq!(criterion, 110);
        assert_eq!(select_tier(&tiers, criterion).unwrap().name, "over-100");
    }
}
