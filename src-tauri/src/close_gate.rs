//! M3-01：Host 权威、可重入的窗口关闭状态机。
//!
//! 所有 close 触发源（自绘 X、Alt+F4、native CloseRequested）都必须先经过
//! [`CloseGate`]：偏好 `ask | hide | quit` 决定动作，restore 能力约束 hide，
//! epoch 保证 prompt 单例且过期/重复决定被拒绝。纯核心，不含 Tauri 类型，
//! 可用 fake adapter 单测（A1–A4）。

use serde::{Deserialize, Serialize};

/// 关闭偏好（持久化 `lifecycle.toml` 的 `[close] behavior`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosePreference {
    Ask,
    Hide,
    Quit,
}

impl ClosePreference {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Hide => "hide",
            Self::Quit => "quit",
        }
    }
}

/// 三个等价触发源：产生同一 CloseIntent 与状态序列（M3-01.A1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseTrigger {
    Titlebar,
    AltF4,
    NativeClose,
}

/// 关闭时剩余的恢复面；决定 hide 是否可安全执行（M3-01.A4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreCapability {
    Tray,
    Dock,
    Companion,
    None,
}

/// 旧配置/平台默认迁移到偏好枚举（M3-01.A3）。
/// - `None`/`"ask"` → Ask；`"hide"` → Hide；`"quit"` → Quit；
/// - 未知值一律回落 Ask（默认安全），且函数是纯映射（幂等）。
pub fn migrate_preference(raw: Option<&str>) -> ClosePreference {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some("hide") => ClosePreference::Hide,
        Some("quit") => ClosePreference::Quit,
        // "ask"、None、未知值、空白串：一律安全默认。
        _ => ClosePreference::Ask,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAction {
    /// prevent_close + 显示（或聚焦）确认对话框。
    ShowPrompt,
    Hide,
    Quit,
    /// 已有 prompt 在场：吞掉重复 close 并聚焦现有对话框。
    IgnoreFocusExisting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseDecision {
    pub action: CloseAction,
    pub preference: ClosePreference,
    pub focus_existing: bool,
    /// Ask 路径的 prompt 会话号；resolve/cancel 必须携带匹配 epoch。
    pub epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseDecisionError {
    /// epoch 与当前 prompt 会话不匹配（迟到的决定）。
    Stale,
    /// 当前没有待决 prompt。
    NotPrompting,
    /// 同一 epoch 的决定已被处理（重复提交）。
    AlreadyResolved,
}

/// 用户在对话框上的决定；cancel/Escape 不保存任何偏好。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptDecision {
    Hide,
    Quit,
    Cancel,
}

/// 可重入门：`idle → prompting → executing` 的原子转换在此收口。
#[derive(Debug, Default)]
pub struct CloseGate {
    epoch: u64,
    prompting: bool,
    resolved: bool,
}

impl CloseGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// 单一 close 入口。等价触发共享同一实现（A1）；
    /// 重复 close 在 prompting 态只聚焦现有对话框（A2）；
    /// Hide + 无恢复面永不执行 hide（A4）。
    pub fn handle_close(
        &mut self,
        _trigger: CloseTrigger,
        preference: ClosePreference,
        restore: RestoreCapability,
    ) -> CloseDecision {
        match preference {
            ClosePreference::Quit => CloseDecision {
                action: CloseAction::Quit,
                preference,
                focus_existing: false,
                epoch: self.epoch,
            },
            ClosePreference::Hide if restore != RestoreCapability::None => {
                CloseDecision {
                    action: CloseAction::Hide,
                    preference,
                    focus_existing: false,
                    epoch: self.epoch,
                }
            }
            // Ask，以及「想 hide 但没有恢复面」的降级：都必须问人，且保持窗口可达。
            ClosePreference::Ask | ClosePreference::Hide => {
                if self.prompting {
                    return CloseDecision {
                        action: CloseAction::IgnoreFocusExisting,
                        preference,
                        focus_existing: true,
                        epoch: self.epoch,
                    };
                }
                self.epoch = self.epoch.wrapping_add(1);
                self.prompting = true;
                self.resolved = false;
                CloseDecision {
                    action: CloseAction::ShowPrompt,
                    preference,
                    focus_existing: false,
                    epoch: self.epoch,
                }
            }
        }
    }

    /// 用户决定落账：epoch 匹配且未重复提交才生效（A2）。
    pub fn resolve(
        &mut self,
        epoch: u64,
        decision: PromptDecision,
    ) -> Result<Option<CloseAction>, CloseDecisionError> {
        if epoch != self.epoch {
            return Err(CloseDecisionError::Stale);
        }
        if self.resolved {
            return Err(CloseDecisionError::AlreadyResolved);
        }
        if !self.prompting {
            return Err(CloseDecisionError::NotPrompting);
        }
        self.resolved = true;
        self.prompting = false;
        Ok(match decision {
            PromptDecision::Hide => Some(CloseAction::Hide),
            PromptDecision::Quit => Some(CloseAction::Quit),
            PromptDecision::Cancel => None,
        })
    }

    pub fn is_prompting(&self) -> bool {
        self.prompting
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a1_equivalent_triggers_produce_same_intent_and_sequence() {
        let triggers = [
            CloseTrigger::Titlebar,
            CloseTrigger::AltF4,
            CloseTrigger::NativeClose,
        ];
        for trigger in triggers {
            let mut gate = CloseGate::new();
            let first = gate.handle_close(trigger, ClosePreference::Ask, RestoreCapability::Tray);
            assert_eq!(first.action, CloseAction::ShowPrompt);
            assert!(first.epoch > 0);
            let second = gate.handle_close(trigger, ClosePreference::Ask, RestoreCapability::Tray);
            assert_eq!(second.action, CloseAction::IgnoreFocusExisting);
            assert!(second.focus_existing);
            assert_eq!(second.epoch, first.epoch);
        }
    }

    #[test]
    fn a2_stale_and_duplicate_decisions_are_rejected() {
        let mut gate = CloseGate::new();
        let d1 = gate.handle_close(CloseTrigger::Titlebar, ClosePreference::Ask, RestoreCapability::Dock);
        let d2 = gate.handle_close(CloseTrigger::AltF4, ClosePreference::Ask, RestoreCapability::Dock);
        assert_eq!(d1.epoch, d2.epoch, "同一 prompt 会话 epoch 不变");
        // 迟到的旧 epoch 决定
        assert_eq!(
            gate.resolve(d1.epoch.wrapping_sub(1), PromptDecision::Quit),
            Err(CloseDecisionError::Stale)
        );
        // 同 epoch 第一次有效
        assert_eq!(gate.resolve(d2.epoch, PromptDecision::Quit), Ok(Some(CloseAction::Quit)));
        // 同 epoch 重复提交
        assert_eq!(
            gate.resolve(d2.epoch, PromptDecision::Hide),
            Err(CloseDecisionError::AlreadyResolved)
        );
        // 结束后不再处于 prompting
        assert!(!gate.is_prompting());
    }

    #[test]
    fn a3_preference_migration_is_idempotent_and_safe_by_default() {
        assert_eq!(migrate_preference(None), ClosePreference::Ask);
        assert_eq!(migrate_preference(Some("ask")), ClosePreference::Ask);
        assert_eq!(migrate_preference(Some("hide")), ClosePreference::Hide);
        assert_eq!(migrate_preference(Some("quit")), ClosePreference::Quit);
        assert_eq!(migrate_preference(Some("  hide ")), ClosePreference::Hide);
        assert_eq!(migrate_preference(Some("what-ever")), ClosePreference::Ask);
        assert_eq!(migrate_preference(Some("")), ClosePreference::Ask);
        // 幂等：对迁移结果再迁移不改变。
        for raw in [None, Some("hide"), Some("quit"), Some("nonsense")] {
            let once = migrate_preference(raw);
            let again = migrate_preference(Some(once.as_str()));
            assert_eq!(once, again);
        }
    }

    #[test]
    fn a4_hide_never_executes_without_restore_capability() {
        let mut gate = CloseGate::new();
        let decision = gate.handle_close(
            CloseTrigger::NativeClose,
            ClosePreference::Hide,
            RestoreCapability::None,
        );
        assert_ne!(decision.action, CloseAction::Hide);
        assert_eq!(decision.action, CloseAction::ShowPrompt, "无恢复面时降级为询问，窗口保持可达");

        // 有恢复面时 hide 直接执行。
        for restore in [RestoreCapability::Tray, RestoreCapability::Dock, RestoreCapability::Companion] {
            let mut gate = CloseGate::new();
            let decision = gate.handle_close(CloseTrigger::Titlebar, ClosePreference::Hide, restore);
            assert_eq!(decision.action, CloseAction::Hide);
        }
    }

    #[test]
    fn quit_and_cancel_paths() {
        let mut gate = CloseGate::new();
        let decision = gate.handle_close(CloseTrigger::Titlebar, ClosePreference::Quit, RestoreCapability::None);
        assert_eq!(decision.action, CloseAction::Quit, "quit 偏好不需要恢复面");

        let mut gate = CloseGate::new();
        let d = gate.handle_close(CloseTrigger::Titlebar, ClosePreference::Ask, RestoreCapability::Dock);
        assert_eq!(gate.resolve(d.epoch, PromptDecision::Cancel), Ok(None));
        assert!(!gate.is_prompting());
    }
}

// ---- 持久化（lifecycle.toml，FeatureFlagService 同款模式）----

use std::path::{Path, PathBuf};

const LIFECYCLE_FILE: &str = "lifecycle.toml";

#[derive(Debug, Clone)]
pub struct ClosePreferenceService {
    config_dir: PathBuf,
}

impl ClosePreferenceService {
    pub fn new(config_dir: PathBuf) -> Self {
        Self { config_dir }
    }

    fn path(&self) -> PathBuf {
        self.config_dir.join(LIFECYCLE_FILE)
    }

    /// 读取偏好：缺文件/缺字段/未知值一律经 migrate 回落 Ask（M3-01.A3）。
    pub fn load(&self) -> ClosePreference {
        let path = self.path();
        let Ok(content) = std::fs::read_to_string(&path) else {
            return migrate_preference(None);
        };
        let raw = content
            .lines()
            .find_map(|line| line.strip_prefix("behavior = "))
            .map(|v| v.trim().trim_matches('"').to_string());
        migrate_preference(raw.as_deref())
    }

    /// 保存只发生在已确认的 hide/quit/ask 路径（M3-01 步骤 3）。
    pub fn save(&self, preference: ClosePreference) -> Result<(), String> {
        let path = self.path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(
            &path,
            format!("[close]\nbehavior = \"{}\"\n", preference.as_str()),
        )
        .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;

    fn temp_dir(label: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("r-code-lifecycle-{label}-"))
            .tempdir()
            .expect("tempdir")
    }

    #[test]
    fn close_behavior_round_trip_and_safe_defaults() {
        let temp = temp_dir("round-trip");
        let service = ClosePreferenceService::new(temp.path().to_path_buf());
        assert_eq!(service.load(), ClosePreference::Ask, "缺文件默认 ask");

        service.save(ClosePreference::Hide).expect("save hide");
        assert_eq!(service.load(), ClosePreference::Hide);
        service.save(ClosePreference::Quit).expect("save quit");
        assert_eq!(service.load(), ClosePreference::Quit);

        // 未知/手工脏值回落 ask（幂等安全默认）。
        std::fs::write(service.path(), "[close]\nbehavior = \"weird\"\n").unwrap();
        assert_eq!(service.load(), ClosePreference::Ask);
        std::fs::write(service.path(), "garbage content\n").unwrap();
        assert_eq!(service.load(), ClosePreference::Ask);
    }
}
