//! fullscreen/regular 双态切换（PRD §4.1 R-TUI-06 / M8-03.A2）。
//!
//! regular = 常规滚动终端（不打破 scrollback 惯例，历史随终端滚动）；
//! fullscreen = 备用屏（VStack 主区 + HStack 输入区布局 + 全文搜索）。
//! 切换是纯状态机；备用屏进入/退出（EnterAlternateScreen/Leave）在壳层。

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScreenMode {
    /// 默认：常规滚动终端。
    #[default]
    Regular,
    /// 备用屏：全屏应用布局。
    Fullscreen,
}

/// 全文搜索状态（fullscreen 态可用）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchState {
    pub query: String,
    pub active: bool,
}

/// 布局模式状态机。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScreenState {
    pub mode: ScreenMode,
    pub search: SearchState,
}

impl ScreenState {
    pub fn toggle(&mut self) {
        self.mode = match self.mode {
            ScreenMode::Regular => ScreenMode::Fullscreen,
            ScreenMode::Fullscreen => ScreenMode::Regular,
        };
        // 退出 fullscreen 时关闭搜索（搜索框是备用屏布局的一部分）。
        if self.mode == ScreenMode::Regular {
            self.search = SearchState::default();
        }
    }

    /// 打开/关闭搜索（仅 fullscreen 态生效——regular 态无搜索框布局）。
    pub fn toggle_search(&mut self) {
        if self.mode == ScreenMode::Fullscreen {
            self.search.active = !self.search.active;
            if !self.search.active {
                self.search.query.clear();
            }
        }
    }

    /// 备用屏布局区域（VStack：主区在上占剩余高度，输入区在下固定 3 行；
    /// 搜索激活时主区顶部再让出 1 行给搜索框）。
    pub fn fullscreen_layout(
        &self,
        total_height: u16,
        total_width: u16,
    ) -> Vec<(u16, u16, u16, u16)> {
        debug_assert!(self.mode == ScreenMode::Fullscreen);
        let mut top = 0u16;
        if self.search.active {
            let search_rows = 1;
            // 搜索框（HStack：内容 + 状态）。
            let _ = total_width;
            top += search_rows;
        }
        let input_rows = 3u16;
        let main_height = total_height.saturating_sub(top + input_rows);
        vec![
            // 主区（transcript，VStack 上段）。
            (0, top, total_width, main_height),
            // 输入区（HStack：提示符 + 输入 + 发送）。
            (0, top + main_height, total_width, input_rows),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M8-03.A2：双态切换——regular↔fullscreen 翻转；退出关搜索。
    #[test]
    fn toggling_switches_modes_and_closes_search() {
        let mut state = ScreenState::default();
        assert_eq!(
            state.mode,
            ScreenMode::Regular,
            "默认 regular（不打破 scrollback）"
        );
        state.toggle();
        assert_eq!(state.mode, ScreenMode::Fullscreen);
        // fullscreen 开搜索。
        state.toggle_search();
        state.search.query.push_str("find-me");
        assert!(state.search.active);
        // 退 fullscreen：搜索随之关闭清空。
        state.toggle();
        assert_eq!(state.mode, ScreenMode::Regular);
        assert!(!state.search.active);
        assert!(state.search.query.is_empty());
        // regular 态开搜索无效。
        state.toggle_search();
        assert!(!state.search.active);
    }

    /// 备用屏布局：VStack 主区 + HStack 输入区；搜索激活时让出搜索行。
    #[test]
    fn fullscreen_layout_partitions_screen() {
        let mut state = ScreenState {
            mode: ScreenMode::Fullscreen,
            ..Default::default()
        };
        let layout = state.fullscreen_layout(30, 100);
        assert_eq!(layout.len(), 2);
        // 主区 (0,0,100,27) + 输入区 (0,27,100,3)：高度无重叠无遗漏。
        assert_eq!(layout[0], (0, 0, 100, 27));
        assert_eq!(layout[1], (0, 27, 100, 3));
        // 搜索激活：主区顶部让 1 行。
        state.toggle_search();
        let layout = state.fullscreen_layout(30, 100);
        assert_eq!(layout[0].1, 1);
        assert_eq!(layout[0].3, 26);
        assert_eq!(layout[1].1, 27);
    }
}
