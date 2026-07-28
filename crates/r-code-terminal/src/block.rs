//! Block Model — OSC 133 块解析 [doc-03 §4]
//!
//! 解析终端输出中的 OSC 133 转义序列，将命令划分为块。
//! - `A` (PromptStart): 提示符开始
//! - `B` (PromptContinuation): 提示符结束，命令输入开始
//! - `C` (CommandExecuted): 命令已执行，输出开始
//! - `D;exit_code` (CommandExit): 命令退出

use chrono::{DateTime, Utc};

/// 块类型 — 命令或 turn。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockType {
    /// 命令块：prompt -> command -> output -> exit code
    Command,
    /// Turn 块：外部 Agent 会话边界
    Turn,
}

/// 一个终端块。
#[derive(Debug, Clone)]
pub struct Block {
    pub block_type: BlockType,
    pub command: Option<String>,
    pub output: String,
    pub exit_code: Option<i32>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

/// 解析状态。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ParseState {
    /// 在任何块之外，等待 A 标记
    Outside,
    /// A 和 B 之间（提示符文本，丢弃）
    InPrompt,
    /// B 和 C 之间（命令文本）
    InCommand,
    /// C 和 D 之间（命令输出）
    InOutput,
}

/// 块解析器 — 从终端输出中解析 OSC 133 序列。
pub struct BlockParser {
    current_block: Option<Block>,
    blocks: Vec<Block>,
    state: ParseState,
    /// 不完整转义序列的暂存区
    pending: Vec<u8>,
}

impl BlockParser {
    pub fn new() -> Self {
        Self {
            current_block: None,
            blocks: Vec::new(),
            state: ParseState::Outside,
            pending: Vec::new(),
        }
    }

    /// 输入终端输出并解析块。
    pub fn feed(&mut self, data: &[u8]) {
        // 合并暂存区与新数据
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(data);

        let mut i = 0;
        while i < buf.len() {
            if buf[i] == 0x1b {
                // ESC — 转义序列
                if i + 1 >= buf.len() {
                    // ESC 在缓冲区末尾 — 不完整
                    self.pending = buf[i..].to_vec();
                    return;
                }
                match buf[i + 1] {
                    b']' => {
                        // OSC: ESC ] ... BEL(0x07) 或 ST(ESC \)
                        let osc_start = i + 2;
                        if let Some((param_end, term_len)) = find_osc_terminator(&buf, osc_start) {
                            self.handle_osc(&buf[osc_start..param_end]);
                            i = param_end + term_len;
                        } else {
                            // 不完整 OSC — 暂存
                            self.pending = buf[i..].to_vec();
                            return;
                        }
                    }
                    b'[' => {
                        // CSI: ESC [ ... final byte (0x40–0x7e)
                        let mut j = i + 2;
                        while j < buf.len() && !(buf[j] >= 0x40 && buf[j] <= 0x7e) {
                            j += 1;
                        }
                        if j < buf.len() {
                            i = j + 1; // 跳过 final byte
                        } else {
                            // 不完整 CSI — 暂存
                            self.pending = buf[i..].to_vec();
                            return;
                        }
                    }
                    b'P' | b'X' | b'^' | b'_' => {
                        // DCS/SOS/PM/APC: 以 ST(ESC \) 结束
                        if let Some(end) = find_string_terminator(&buf, i + 2) {
                            i = end;
                        } else {
                            self.pending = buf[i..].to_vec();
                            return;
                        }
                    }
                    b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/' => {
                        // 字符集指定：ESC ( B（3 字节）
                        if i + 2 < buf.len() {
                            i += 3;
                        } else {
                            self.pending = buf[i..].to_vec();
                            return;
                        }
                    }
                    _ => {
                        // 其他转义：ESC + 单字节
                        i += 2;
                    }
                }
            } else {
                // 文本 — 直到下一个 ESC 或缓冲区末尾
                let text_start = i;
                while i < buf.len() && buf[i] != 0x1b {
                    i += 1;
                }
                self.accumulate_text(&buf[text_start..i]);
            }
        }
        // 全部消费完毕
        self.pending.clear();
    }

    /// 获取已完成的块。
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// 获取当前（进行中）的块。
    pub fn current(&self) -> Option<&Block> {
        self.current_block.as_ref()
    }

    /// 当前 shell 是否已经开始执行一条命令。
    ///
    /// `OSC 133;C` 表示输入已提交、命令输出即将开始；直到随后收到
    /// `OSC 133;D;<exit>` 之前，终端应被视为忙碌。这个小查询让 PTY 管理器
    /// 可以根据 shell 集成的真实边界更新状态，而不是根据一次 `send` 猜测。
    pub fn command_is_running(&self) -> bool {
        self.state == ParseState::InOutput
    }

    /// 返回当前正在执行的命令文本（若 shell 集成已经捕获到）。
    ///
    /// 仅在 [`Self::command_is_running`] 为真时使用；调用方不得把该文本写入
    /// 不受信任的日志或跨进程协议中。
    pub fn running_command(&self) -> Option<&str> {
        if !self.command_is_running() {
            return None;
        }
        self.current_block
            .as_ref()
            .and_then(|block| block.command.as_deref())
    }

    /// 处理 OSC 序列参数。
    fn handle_osc(&mut self, params: &[u8]) {
        let param_str = String::from_utf8_lossy(params);
        let mut parts = param_str.splitn(3, ';');
        let kind = parts.next().unwrap_or("");
        if kind != "133" {
            return; // 非 OSC 133 — 忽略
        }
        let marker = parts.next().unwrap_or("");
        match marker {
            "A" => self.handle_prompt_start(),
            "B" => self.handle_command_start(),
            "C" => self.handle_command_executed(),
            "D" => {
                let exit_code = parts.next().and_then(|s| s.trim().parse::<i32>().ok());
                self.handle_command_exit(exit_code);
            }
            _ => {}
        }
    }

    fn handle_prompt_start(&mut self) {
        // A 标记：提示符开始
        // 如果有未关闭的块，先完成它（无 exit code）
        if let Some(mut block) = self.current_block.take() {
            block.ended_at = Some(Utc::now());
            self.blocks.push(block);
        }
        self.current_block = Some(Block {
            block_type: BlockType::Command,
            command: None,
            output: String::new(),
            exit_code: None,
            started_at: Utc::now(),
            ended_at: None,
        });
        self.state = ParseState::InPrompt;
    }

    fn handle_command_start(&mut self) {
        // B 标记：提示符结束，命令输入开始
        self.state = ParseState::InCommand;
    }

    fn handle_command_executed(&mut self) {
        // C 标记：命令已执行，输出开始
        // 修剪命令文本（去除尾部换行/空白）
        if let Some(block) = &mut self.current_block {
            if let Some(cmd) = &mut block.command {
                let trimmed = cmd.trim();
                if trimmed.is_empty() {
                    block.command = None;
                } else {
                    *cmd = trimmed.to_string();
                }
            }
        }
        self.state = ParseState::InOutput;
    }

    fn handle_command_exit(&mut self, exit_code: Option<i32>) {
        // D 标记：命令退出
        if let Some(block) = &mut self.current_block {
            block.exit_code = exit_code;
            block.ended_at = Some(Utc::now());
        }
        if let Some(block) = self.current_block.take() {
            self.blocks.push(block);
        }
        self.state = ParseState::Outside;
    }

    /// 根据当前状态累积文本。
    fn accumulate_text(&mut self, text: &[u8]) {
        if text.is_empty() {
            return;
        }
        if let Some(block) = &mut self.current_block {
            let s = String::from_utf8_lossy(text);
            match self.state {
                ParseState::InCommand => {
                    let cmd = block.command.get_or_insert_with(String::new);
                    cmd.push_str(&s);
                }
                ParseState::InOutput => {
                    block.output.push_str(&s);
                }
                // InPrompt / Outside — 丢弃
                _ => {}
            }
        }
    }
}

impl Default for BlockParser {
    fn default() -> Self {
        Self::new()
    }
}

/// 在 OSC 参数中查找终止符，返回 (参数结束位置, 终止符长度)。
/// BEL(0x07) -> 长度 1；ST(ESC \) -> 长度 2。
fn find_osc_terminator(buf: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut i = start;
    while i < buf.len() {
        if buf[i] == 0x07 {
            return Some((i, 1));
        }
        if buf[i] == 0x1b && i + 1 < buf.len() && buf[i + 1] == b'\\' {
            return Some((i, 2));
        }
        i += 1;
    }
    None
}

/// 查找字符串终止符 ST(ESC \)，返回其后的位置。
fn find_string_terminator(buf: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    while i < buf.len() {
        if buf[i] == 0x1b && i + 1 < buf.len() && buf[i + 1] == b'\\' {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OSC 133 序列辅助构造（BEL 终止符）
    fn osc133(marker: &str) -> Vec<u8> {
        format!("\x1b]133;{marker}\x07").into_bytes()
    }

    /// OSC 133;D;exit 序列（BEL 终止符）
    fn osc133_d(exit_code: i32) -> Vec<u8> {
        format!("\x1b]133;D;{exit_code}\x07").into_bytes()
    }

    #[test]
    fn empty_feed_does_nothing() {
        let mut parser = BlockParser::new();
        parser.feed(b"");
        assert!(parser.blocks().is_empty());
        assert!(parser.current().is_none());
    }

    #[test]
    fn text_without_osc_is_ignored() {
        let mut parser = BlockParser::new();
        parser.feed(b"some random text");
        assert!(parser.blocks().is_empty());
        assert!(parser.current().is_none());
    }

    #[test]
    fn complete_command_block_with_bel_terminator() {
        let mut parser = BlockParser::new();
        let mut data = Vec::new();
        data.extend(osc133("A")); // prompt start
        data.extend(osc133("B")); // command input start
        data.extend(b"echo hello"); // command text
        data.extend(osc133("C")); // command executed
        data.extend(b"hello\r\n"); // output
        data.extend(osc133_d(0)); // exit code 0

        parser.feed(&data);

        assert_eq!(parser.blocks().len(), 1);
        let block = &parser.blocks()[0];
        assert_eq!(block.block_type, BlockType::Command);
        assert_eq!(block.command.as_deref(), Some("echo hello"));
        assert!(block.output.contains("hello"));
        assert_eq!(block.exit_code, Some(0));
        assert!(block.ended_at.is_some());
        assert!(parser.current().is_none());
    }

    #[test]
    fn osc133_with_st_terminator() {
        let mut parser = BlockParser::new();
        // 使用 ST (ESC \) 终止符
        let data = b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\";
        parser.feed(data);

        assert_eq!(parser.blocks().len(), 1);
        assert_eq!(parser.blocks()[0].exit_code, Some(0));
    }

    #[test]
    fn exit_code_nonzero() {
        let mut parser = BlockParser::new();
        let mut data = Vec::new();
        data.extend(osc133("A"));
        data.extend(osc133("B"));
        data.extend(b"false");
        data.extend(osc133("C"));
        data.extend(osc133_d(1));

        parser.feed(&data);

        assert_eq!(parser.blocks().len(), 1);
        assert_eq!(parser.blocks()[0].exit_code, Some(1));
        assert_eq!(parser.blocks()[0].command.as_deref(), Some("false"));
    }

    #[test]
    fn exit_code_missing_treated_as_none() {
        let mut parser = BlockParser::new();
        let mut data = Vec::new();
        data.extend(osc133("A"));
        data.extend(osc133("B"));
        data.extend(b"true");
        data.extend(osc133("C"));
        data.extend(osc133("D")); // D without exit code

        parser.feed(&data);

        assert_eq!(parser.blocks().len(), 1);
        assert_eq!(parser.blocks()[0].exit_code, None);
    }

    #[test]
    fn multiple_blocks() {
        let mut parser = BlockParser::new();
        let mut data = Vec::new();

        // Block 1
        data.extend(osc133("A"));
        data.extend(osc133("B"));
        data.extend(b"echo one");
        data.extend(osc133("C"));
        data.extend(b"one\r\n");
        data.extend(osc133_d(0));

        // Block 2
        data.extend(osc133("A"));
        data.extend(osc133("B"));
        data.extend(b"echo two");
        data.extend(osc133("C"));
        data.extend(b"two\r\n");
        data.extend(osc133_d(0));

        parser.feed(&data);

        assert_eq!(parser.blocks().len(), 2);
        assert_eq!(parser.blocks()[0].command.as_deref(), Some("echo one"));
        assert_eq!(parser.blocks()[1].command.as_deref(), Some("echo two"));
    }

    #[test]
    fn split_across_feeds() {
        let mut parser = BlockParser::new();

        // 第一次 feed：A + B + 部分命令
        let mut part1 = Vec::new();
        part1.extend(osc133("A"));
        part1.extend(osc133("B"));
        part1.extend(b"echo");
        parser.feed(&part1);

        // 当前块应该存在，命令部分累积
        assert!(parser.current().is_some());
        // 命令尚未完成（C 未到），但文本已累积
        assert_eq!(parser.current().unwrap().command.as_deref(), Some("echo"));

        // 第二次 feed：剩余命令 + C + 输出 + D
        let mut part2 = Vec::new();
        part2.extend(b" hello");
        part2.extend(osc133("C"));
        part2.extend(b"hello\r\n");
        part2.extend(osc133_d(0));
        parser.feed(&part2);

        assert_eq!(parser.blocks().len(), 1);
        assert_eq!(parser.blocks()[0].command.as_deref(), Some("echo hello"));
        assert!(parser.blocks()[0].output.contains("hello"));
    }

    #[test]
    fn reports_running_command_only_between_c_and_d() {
        let mut parser = BlockParser::new();
        let mut start = Vec::new();
        start.extend(osc133("A"));
        start.extend(osc133("B"));
        start.extend(b"codex review");
        parser.feed(&start);
        assert!(!parser.command_is_running());
        assert_eq!(parser.running_command(), None);

        parser.feed(&osc133("C"));
        assert!(parser.command_is_running());
        assert_eq!(parser.running_command(), Some("codex review"));

        parser.feed(&osc133_d(0));
        assert!(!parser.command_is_running());
        assert_eq!(parser.running_command(), None);
    }

    #[test]
    fn split_osc_sequence_across_feeds() {
        let mut parser = BlockParser::new();

        // feed 部分 OSC 序列
        parser.feed(b"\x1b]133");
        assert!(parser.current().is_none()); // 未完成，无块

        // feed 剩余部分
        parser.feed(b";A\x07");
        assert!(parser.current().is_some()); // A 标记处理，块创建
    }

    #[test]
    fn csi_sequences_stripped_from_output() {
        let mut parser = BlockParser::new();
        let mut data = Vec::new();
        data.extend(osc133("A"));
        data.extend(osc133("B"));
        data.extend(b"ls");
        data.extend(osc133("C"));
        // 输出包含 ANSI 颜色码
        data.extend(b"\x1b[31m");
        data.extend(b"file.txt");
        data.extend(b"\x1b[0m");
        data.extend(b"\r\n");
        data.extend(osc133_d(0));

        parser.feed(&data);

        let block = &parser.blocks()[0];
        assert!(block.output.contains("file.txt"));
        assert!(!block.output.contains("\x1b["));
    }

    #[test]
    fn non_133_osc_ignored() {
        let mut parser = BlockParser::new();
        let mut data = Vec::new();
        data.extend(osc133("A"));
        data.extend(osc133("B"));
        data.extend(b"pwd");
        data.extend(osc133("C"));
        // OSC 8 (hyperlink) — 应被忽略，不影响块解析
        data.extend(b"\x1b]8;;https://example.com\x07");
        data.extend(b"/home/user");
        data.extend(b"\x1b]8;;\x07");
        data.extend(b"\r\n");
        data.extend(osc133_d(0));

        parser.feed(&data);

        assert_eq!(parser.blocks().len(), 1);
        let block = &parser.blocks()[0];
        assert!(block.output.contains("/home/user"));
        assert!(!block.output.contains("example.com"));
    }

    #[test]
    fn unclosed_block_finalized_on_next_a() {
        let mut parser = BlockParser::new();
        let mut data = Vec::new();

        // Block 1: A, B, C — 但没有 D
        data.extend(osc133("A"));
        data.extend(osc133("B"));
        data.extend(b"cmd1");
        data.extend(osc133("C"));
        data.extend(b"output1");

        // Block 2: A（此时 Block 1 应被关闭，无 exit code）
        data.extend(osc133("A"));
        data.extend(osc133("B"));
        data.extend(b"cmd2");
        data.extend(osc133("C"));
        data.extend(b"output2");
        data.extend(osc133_d(0));

        parser.feed(&data);

        assert_eq!(parser.blocks().len(), 2);
        // Block 1 无 exit code
        assert_eq!(parser.blocks()[0].exit_code, None);
        assert_eq!(parser.blocks()[0].command.as_deref(), Some("cmd1"));
        // Block 2 有 exit code
        assert_eq!(parser.blocks()[1].exit_code, Some(0));
        assert_eq!(parser.blocks()[1].command.as_deref(), Some("cmd2"));
    }

    #[test]
    fn empty_command_treated_as_none() {
        let mut parser = BlockParser::new();
        let mut data = Vec::new();
        data.extend(osc133("A"));
        data.extend(osc133("B"));
        data.extend(b"   \r\n"); // 只有空白
        data.extend(osc133("C"));
        data.extend(b"\r\n");
        data.extend(osc133_d(0));

        parser.feed(&data);

        assert_eq!(parser.blocks().len(), 1);
        assert_eq!(parser.blocks()[0].command, None);
    }

    #[test]
    fn default_creates_new_parser() {
        let parser = BlockParser::default();
        assert!(parser.blocks().is_empty());
        assert!(parser.current().is_none());
    }

    #[test]
    fn d_marker_before_first_a_creates_no_block() {
        let mut parser = BlockParser::new();
        // D 标记在 A 之前（不应该发生，但需健壮处理）
        parser.feed(&osc133_d(0));
        assert!(parser.blocks().is_empty());
        assert!(parser.current().is_none());
    }
}
