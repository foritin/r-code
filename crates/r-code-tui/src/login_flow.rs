//! /login 账号登录（G10：Codex / ChatGPT OAuth 委托 + 其余厂商诚实拒绝）。
//!
//! 立项调研结论（2026-09-03）：30 个 provider 预设无一提供第三方 TUI 可用的
//! OAuth/device-code 端点（Anthropic OAuth 需官方客户端白名单、有 ToS 风险；
//! OpenRouter/DeepSeek/国内厂商全部 API key）。产品内唯一真实 OAuth 通道是
//! 宿主已落地的 Codex CLI 委托登录（`codex_start_login`/`codex_start_device_login`
//! 新开系统终端窗口完成浏览器/设备码交互，不读登录输出、不碰 auth.json）。
//! 本模块把这条真实通道接到 TUI；对其余厂商显式引导 /setup——不做假 OAuth
//! （禁 mock 红线 R1/R2）。

/// 一个登录选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginOption {
    /// 回调键（browser / device / refresh）。
    pub key: &'static str,
    pub label: String,
}

/// /login 选择器状态。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LoginPicker {
    options: Vec<LoginOption>,
    selected: usize,
    codex_available: bool,
    authenticated: Option<bool>,
    auth_method: Option<String>,
}

impl LoginPicker {
    /// 按宿主 `codex_integration_status` 快照构造。
    pub fn new(
        codex_available: bool,
        authenticated: Option<bool>,
        auth_method: Option<String>,
    ) -> Self {
        let options = if codex_available {
            vec![
                LoginOption {
                    key: "browser",
                    label: "浏览器登录 Codex（ChatGPT 账号，OAuth）".to_string(),
                },
                LoginOption {
                    key: "device",
                    label: "设备码登录 Codex（无浏览器/远程桌面适用）".to_string(),
                },
                LoginOption {
                    key: "refresh",
                    label: "刷新登录状态".to_string(),
                },
            ]
        } else {
            Vec::new()
        };
        Self {
            options,
            selected: 0,
            codex_available,
            authenticated,
            auth_method,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.options.is_empty()
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.options.len() {
            self.selected += 1;
        }
    }

    pub fn selection(&self) -> Option<&LoginOption> {
        self.options.get(self.selected)
    }

    /// 渲染行：Codex 状态行 + 选项 + 其余厂商诚实说明。
    pub fn visible_rows(&self) -> Vec<String> {
        let mut rows = Vec::new();
        if self.codex_available {
            let status = match self.authenticated {
                Some(true) => format!(
                    "已登录（{}）",
                    self.auth_method.as_deref().unwrap_or("Codex")
                ),
                Some(false) => "未登录".to_string(),
                None => "状态未知".to_string(),
            };
            rows.push(format!("Codex CLI：{status}"));
            for (index, option) in self.options.iter().enumerate() {
                let cursor = if index == self.selected { "❯" } else { " " };
                rows.push(format!("{cursor} {}", option.label));
            }
        } else {
            rows.push("未检测到 Codex CLI（ChatGPT 账号登录需要它）".to_string());
        }
        rows.push(
            "其余模型服务（Anthropic/DeepSeek/OpenRouter 等）均只提供 API key 鉴权——/setup 配置（Tab 可切环境变量鉴权）"
                .to_string(),
        );
        rows.push("  enter 选择 · esc 取消".to_string());
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// G10.A1：Codex 可用 → 三选项（浏览器/设备码/刷新）+ 其余厂商说明。
    #[test]
    fn picker_lists_codex_options_when_available() {
        let mut picker = LoginPicker::new(true, Some(false), None);
        let rows = picker.visible_rows();
        assert!(rows[0].contains("未登录"), "{rows:?}");
        assert!(rows[1].contains("❯"), "默认选中第一项：{rows:?}");
        assert!(rows[1].contains("浏览器登录"), "{rows:?}");
        assert!(rows[2].contains("设备码登录"), "{rows:?}");
        assert!(rows[3].contains("刷新登录状态"), "{rows:?}");
        assert!(
            rows.iter().any(|row| row.contains("API key")),
            "其余厂商诚实引导 /setup：{rows:?}"
        );
        assert_eq!(picker.selection().unwrap().key, "browser");
        picker.move_down();
        assert_eq!(picker.selection().unwrap().key, "device");
        picker.move_down();
        assert_eq!(picker.selection().unwrap().key, "refresh");
        picker.move_down();
        assert_eq!(picker.selection().unwrap().key, "refresh", "下移钳位");
    }

    /// G10.A2：Codex 不可用 → 空选择器 + 说明（不出现假 OAuth 选项）。
    #[test]
    fn picker_without_codex_is_empty_and_honest() {
        let picker = LoginPicker::new(false, None, None);
        assert!(picker.is_empty());
        assert!(picker.selection().is_none());
        let rows = picker.visible_rows();
        assert!(rows[0].contains("未检测到 Codex CLI"), "{rows:?}");
        assert!(
            !rows.iter().any(|row| row.contains("❯")),
            "无可选项：{rows:?}"
        );
    }

    /// 已登录态展示认证方式。
    #[test]
    fn picker_shows_authenticated_state() {
        let picker = LoginPicker::new(true, Some(true), Some("ChatGPT".to_string()));
        let rows = picker.visible_rows();
        assert!(rows[0].contains("已登录（ChatGPT）"), "{rows:?}");
    }
}
