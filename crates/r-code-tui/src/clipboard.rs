//! `/copy` 剪贴板（pi 对齐 G7）。
//!
//! 语义 = pi `/copy`：复制**最后一条 assistant 回复**。传输用 OSC 52 终端
//! 剪贴板序列（`\x1b]52;c;<base64>\x07`）——零依赖、SSH 远程会话同样生效、
//! 序列无显示副作用（不动光标，inline 渲染器的帧间锚点不受扰）。
//!
//! 终端支持面：Windows Terminal / kitty / iTerm2 / WezTerm / tmux（需开
//! `set-clipboard on`）支持；不支持的终端静默忽略——状态行仍提示已发送，
//! 文案注明"经终端剪贴板"。`R_CODE_TUI_NO_OSC52=1` 可禁用（调试/测试）。
//!
//! 体积上限 64 KiB：OSC 52 是单条转义序列，多数终端对超长序列直接丢弃
//! （kitty 默认截断在 ~100KB，Windows Terminal 更保守），宁可明确报错。

use crate::TranscriptRow;

/// OSC 52 单条序列的字节上限（base64 前的原文）。
pub const MAX_BYTES: usize = 64 * 1024;

/// 取最后一条 assistant 回复（未收口的流式段也计入——它是最新内容）。
pub fn last_assistant_text(rows: &[TranscriptRow]) -> Option<&str> {
    rows.iter().rev().find_map(|row| match row {
        TranscriptRow::Assistant { text, .. } => Some(text.as_str()),
        _ => None,
    })
}

/// 组 OSC 52 序列（base64 载荷）。
pub fn osc52_sequence(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

/// 是否放行 OSC 52（环境开关 + 体积上限）。
/// 返回 Err(原因) 时不发序列。
pub fn copy_check(text: &str) -> Result<(), String> {
    if std::env::var_os("R_CODE_TUI_NO_OSC52").is_some() {
        return Err("剪贴板序列已由 R_CODE_TUI_NO_OSC52 禁用".to_string());
    }
    let size = text.len();
    if size > MAX_BYTES {
        return Err(format!(
            "内容过长（{size} 字节 > {MAX_BYTES}），终端会丢弃超长剪贴板序列"
        ));
    }
    Ok(())
}

/// 标准 base64（RFC 4648，含 padding；无需 URL-safe——OSC 52 载荷用标准表）。
pub fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(triple >> 18) as usize & 0x3f] as char);
        out.push(TABLE[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// base64 标准向量（RFC 4648 §10）。
    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // 多字节 UTF-8 不特殊处理（字节流编码）。
        assert_eq!(base64("你".as_bytes()), "5L2g");
    }

    /// OSC 52 序列形态。
    #[test]
    fn osc52_sequence_shape() {
        assert_eq!(osc52_sequence("f"), "\x1b]52;c;Zg==\x07");
    }

    /// /copy 语义：最后一条 assistant（跳过其后的工具卡/系统行）。
    #[test]
    fn last_assistant_text_picks_latest_reply() {
        let rows = vec![
            TranscriptRow::User {
                text: "q1".to_string(),
            },
            TranscriptRow::Assistant {
                text: "旧回复".to_string(),
                complete: true,
            },
            TranscriptRow::ToolCard {
                name: "bash".to_string(),
                summary: "ls".to_string(),
                is_error: false,
            },
            TranscriptRow::System {
                text: "提示".to_string(),
            },
            TranscriptRow::Assistant {
                text: "最新回复".to_string(),
                complete: false,
            },
        ];
        assert_eq!(last_assistant_text(&rows), Some("最新回复"));
        assert_eq!(last_assistant_text(&[]), None);
        let no_reply = vec![TranscriptRow::User {
            text: "只有用户".to_string(),
        }];
        assert_eq!(last_assistant_text(&no_reply), None);
    }

    /// 体积上限：超限报错并给原因；限内放行。
    #[test]
    fn copy_check_enforces_size_cap() {
        assert!(copy_check("短文本").is_ok());
        let huge = "x".repeat(MAX_BYTES + 1);
        let error = copy_check(&huge).expect_err("over cap");
        assert!(error.contains("过长"), "{error}");
    }
}
