//! turn 级窗口化（R-TUI-05 / M8-02.A3）：turn = 相邻 user 行之间的所有行；
//! 渲染窗口只含最近 N turn，窗口外行不进渲染集（滚动不整帧重渲全史）。

use crate::TranscriptRow;

/// 按 turn 分组：每组以 User 行开始（首组可无 user 头）。
pub fn group_turns(rows: &[TranscriptRow]) -> Vec<Vec<&TranscriptRow>> {
    let mut turns: Vec<Vec<&TranscriptRow>> = Vec::new();
    for row in rows {
        if matches!(row, TranscriptRow::User { .. }) || turns.is_empty() {
            turns.push(Vec::new());
        }
        turns.last_mut().expect("non-empty").push(row);
    }
    turns
}

/// 窗口化：最近 `max_turns` 个 turn 的行集合（顺序保持；窗口边界在 turn
/// 边界，不切断 turn 内部）。
pub fn windowed(rows: &[TranscriptRow], max_turns: usize) -> Vec<&TranscriptRow> {
    let turns = group_turns(rows);
    let skip = turns.len().saturating_sub(max_turns);
    turns.into_iter().skip(skip).flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> TranscriptRow {
        TranscriptRow::User { text: text.into() }
    }

    fn assistant(text: &str) -> TranscriptRow {
        TranscriptRow::Assistant {
            text: text.into(),
            complete: true,
        }
    }

    /// M8-02.A3：turn 级窗口化——最近 N turn 之外不进渲染集；窗口边界恰在
    /// turn 边界（不切断 turn 内部）；窗口放大到全量时零丢失。
    #[test]
    fn turn_windowing_keeps_recent_turns_whole() {
        let rows = vec![
            user("t1"),
            assistant("a1"),
            user("t2"),
            assistant("a2-1"),
            assistant("a2-2"),
            user("t3"),
            assistant("a3"),
        ];
        // 窗口 = 2：保留 t2/t3 两组（a2-1 与 a2-2 同 turn 不拆）。
        let visible = windowed(&rows, 2);
        let texts: Vec<&str> = visible
            .iter()
            .map(|row| match row {
                TranscriptRow::User { text } | TranscriptRow::Assistant { text, .. } => {
                    text.as_str()
                }
                _ => "",
            })
            .collect();
        assert_eq!(texts, vec!["t2", "a2-1", "a2-2", "t3", "a3"]);
        // 窗口 ≥ turn 数：全量（零丢失）。
        assert_eq!(windowed(&rows, 10).len(), rows.len());
        // 窗口 1：只剩最后 turn。
        let last = windowed(&rows, 1);
        assert_eq!(last.len(), 2);
        // 分组正确：3 组。
        assert_eq!(group_turns(&rows).len(), 3);
    }

    /// 无 user 头的前置行自成首组（流式打开时的常见形态）。
    #[test]
    fn leading_rows_form_own_turn() {
        let rows = vec![assistant("pre"), user("u1"), assistant("a1")];
        let turns = group_turns(&rows);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].len(), 1);
        let last = windowed(&rows, 1);
        assert_eq!(last.len(), 2);
    }
}
