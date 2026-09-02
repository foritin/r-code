//! 真实 PTY 端到端（用户实测症状的回归测试）。断言基于 **R_CODE_TUI_RECORD
//! 字节级记录**（app 的真输出），不基于 ConPTY 合成流——ConPTY 会把子进程
//! 输出重合成（初次全屏重绘会发射整屏 `\r\n\x1b[K`，流层面不可归因）。
//!
//! 1. 逐字输入时，上方已提交历史逐帧不被改写（"随便输入就把上面的内容顶掉"）。
//! 2. 启动不整屏灌空行（内容顶部就位，live 块紧随其后）。
//! 3. 历史超一屏后滚入 scrollback 不丢失（"上滑看不到历史"）。
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// 迷你 VT：rows×cols 屏幕 + scrollback（顶部滚出行保序追加）。
struct Vt {
    rows: usize,
    cols: usize,
    screen: Vec<Vec<char>>,
    scrollback: Vec<String>,
    row: usize,
    col: usize,
}

impl Vt {
    fn new(rows: usize, cols: usize) -> Self {
        Self {
            rows,
            cols,
            screen: vec![vec![' '; cols]; rows],
            scrollback: Vec::new(),
            row: 0,
            col: 0,
        }
    }

    fn put(&mut self, ch: char) {
        if self.col >= self.cols {
            self.col = 0;
            self.linefeed();
        }
        self.screen[self.row][self.col] = ch;
        self.col += 1;
    }

    fn linefeed(&mut self) {
        if self.row + 1 >= self.rows {
            self.scrollback.push(self.screen[0].iter().collect());
            self.screen.remove(0);
            self.screen.push(vec![' '; self.cols]);
        } else {
            self.row += 1;
        }
    }

    fn erase_chars(&mut self, n: usize) {
        for c in self.col..(self.col + n).min(self.cols) {
            self.screen[self.row][c] = ' ';
        }
    }

    fn clear_line(&mut self) {
        for c in 0..self.cols {
            self.screen[self.row][c] = ' ';
        }
    }

    fn clear_to_end(&mut self) {
        for c in self.col..self.cols {
            self.screen[self.row][c] = ' ';
        }
        for r in (self.row + 1)..self.rows {
            self.screen[r] = vec![' '; self.cols];
        }
    }

    fn line_text(&self, r: usize) -> String {
        self.screen[r]
            .iter()
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    /// 喂字节流（增量解析；未完成的 CSI 尾巴缓存到下一批）。
    fn feed(&mut self, chunk: &str, pending: &mut String) {
        let text = format!("{pending}{chunk}");
        pending.clear();
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '\r' => self.col = 0,
                '\n' => self.linefeed(),
                '\u{7}' => {}
                '\u{8}' => {
                    self.col = self.col.saturating_sub(1);
                }
                '\u{1b}' => {
                    let mut seq = String::from('\u{1b}');
                    let Some(next) = chars.next() else {
                        *pending = seq;
                        return;
                    };
                    seq.push(next);
                    match next {
                        '[' => {
                            let mut finalizer = None;
                            for c in chars.by_ref() {
                                seq.push(c);
                                if c.is_ascii_alphabetic() || c == '@' {
                                    finalizer = Some(c);
                                    break;
                                }
                            }
                            if finalizer.is_none() {
                                *pending = seq;
                                return;
                            }
                            self.apply_csi(&seq[2..seq.len() - 1], finalizer.unwrap());
                        }
                        ']' => {
                            for c in chars.by_ref() {
                                if c == '\u{7}' {
                                    break;
                                }
                                if c == '\u{1b}' {
                                    let _ = chars.next();
                                    break;
                                }
                            }
                        }
                        '(' | ')' => {
                            let _ = chars.next();
                        }
                        _ => {}
                    }
                }
                _ => self.put(ch),
            }
        }
    }

    fn apply_csi(&mut self, params: &str, finalizer: char) {
        let nums: Vec<usize> = params
            .split(&[';', '?', '!', '>', '<'][..])
            .map(|p| p.parse::<usize>().unwrap_or(0))
            .collect();
        let n = nums.first().copied().unwrap_or(0).max(1);
        match finalizer {
            'A' => self.row = self.row.saturating_sub(n),
            'B' => self.row = (self.row + n).min(self.rows - 1),
            'C' => self.col = (self.col + n).min(self.cols - 1),
            'D' => self.col = self.col.saturating_sub(n),
            'G' => self.col = (n - 1).min(self.cols - 1),
            'H' | 'f' => {
                let r = nums.first().copied().unwrap_or(1).max(1);
                let c = nums.get(1).copied().unwrap_or(1).max(1);
                self.row = (r - 1).min(self.rows - 1);
                self.col = (c - 1).min(self.cols - 1);
            }
            'J' => {
                if n == 2 || n == 3 {
                    for r in 0..self.rows {
                        self.screen[r] = vec![' '; self.cols];
                    }
                } else {
                    self.clear_to_end();
                }
            }
            'K' => {
                if n == 2 {
                    self.clear_line();
                } else if n == 1 {
                    for c in 0..=self.col.min(self.cols - 1) {
                        self.screen[self.row][c] = ' ';
                    }
                } else {
                    for c in self.col..self.cols {
                        self.screen[self.row][c] = ' ';
                    }
                }
            }
            'X' => self.erase_chars(n),
            _ => {}
        }
    }

    fn screen_contains(&self, needle: &str) -> bool {
        (0..self.rows).any(|r| self.line_text(r).contains(needle))
    }

    fn all_lines_contain(&self, needle: &str) -> bool {
        self.screen_contains(needle) || self.scrollback.iter().any(|l| l.contains(needle))
    }
}

struct Session {
    writer: Box<dyn Write + Send>,
    output: mpsc::Receiver<String>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    // ConPTY：master drop = 子进程输入挂断，必须保活到会话结束。
    _master: Box<dyn MasterPty + Send>,
}

fn spawn_tui(record_path: &str, rows: u16) -> Option<Session> {
    let bin = std::env::var("CARGO_BIN_EXE_r-code-tui").ok()?;
    let dir = tempfile::tempdir().ok()?;
    let keep = String::from(dir.path().to_str()?);
    std::mem::forget(dir);
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .ok()?;
    let mut cmd = CommandBuilder::new(bin);
    cmd.args(["--data-dir", &keep]);
    cmd.env("RUST_BACKTRACE", "0");
    cmd.env("R_CODE_TUI_RECORD", record_path);
    let child = pair.slave.spawn_command(cmd).ok()?;
    let writer = pair.master.take_writer().ok()?;
    let reader = pair.master.try_clone_reader().ok()?;
    let master = pair.master;
    let (tx, output) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx
                        .send(String::from_utf8_lossy(&buf[..n]).to_string())
                        .is_err()
                    {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    Some(Session {
        writer,
        output,
        child,
        _master: master,
    })
}

impl Session {
    fn send(&mut self, keys: &str) {
        let _ = self.writer.write_all(keys.as_bytes());
        let _ = self.writer.flush();
    }

    /// 同步用：在 ConPTY（合成）输出里等 needle。断言不基于此流。
    fn wait_for(&mut self, needle: &str, timeout: Duration) -> bool {
        let mut seen = String::new();
        let deadline = Instant::now() + timeout;
        loop {
            if seen.replace('\u{1b}', " ").contains(needle) {
                return true;
            }
            let remain = deadline.saturating_duration_since(Instant::now());
            if remain.is_zero() {
                return false;
            }
            match self
                .output
                .recv_timeout(remain.min(Duration::from_millis(50)))
            {
                Ok(chunk) => seen.push_str(&chunk),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => return seen.contains(needle),
            }
        }
    }

    fn drain(&mut self, dur: Duration) {
        let deadline = Instant::now() + dur;
        while Instant::now() < deadline {
            let _ = self.output.recv_timeout(Duration::from_millis(50));
        }
    }
}

/// 读完记录文件并喂入 VT 模型。
fn recorded_vt(record_path: &str, rows: usize) -> Vt {
    let raw = std::fs::read_to_string(record_path).expect("读记录文件");
    let mut vt = Vt::new(rows, 100);
    let mut pending = String::new();
    vt.feed(&raw, &mut pending);
    vt
}

fn temp_record_path(tag: &str) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(format!("record-{tag}.bin"));
    std::mem::forget(dir);
    path.to_str().expect("utf8 path").to_string()
}

/// 症状 1：逐字输入不改写上方历史；live 块不上漂。
#[test]
fn typing_does_not_rewrite_history_above() {
    let record = temp_record_path("typing");
    let Some(mut session) = spawn_tui(&record, 30) else {
        eprintln!("pty 不可用，跳过");
        return;
    };
    assert!(
        session.wait_for("尚未配置", Duration::from_secs(20)),
        "启动引导出现"
    );
    session.drain(Duration::from_millis(300));

    for ch in "hello world".chars() {
        session.send(&ch.to_string());
        session.drain(Duration::from_millis(120));
    }
    session.drain(Duration::from_millis(500));

    session.send("\x03");
    session.send("\x03");
    let _ = session.child.wait();

    let vt = recorded_vt(&record, 30);
    // 引导历史仍在屏上（未被吃掉）。
    assert!(
        vt.screen_contains("尚未配置"),
        "症状1复现：引导被输入改写。screen={:?}",
        (0..vt.rows).map(|r| vt.line_text(r)).collect::<Vec<_>>()
    );
    // 输入行在引导下方（live 块未上漂越过历史）。
    let guidance_row = (0..vt.rows)
        .find(|&r| vt.line_text(r).contains("尚未配置"))
        .expect("引导行");
    let input_row = (0..vt.rows)
        .find(|&r| vt.line_text(r).starts_with('>'))
        .expect("输入行");
    assert!(
        input_row > guidance_row,
        "live 块必须在历史下方：guidance={guidance_row} input={input_row}"
    );
    assert!(
        vt.screen_contains("> hello world"),
        "输入行显示完整输入：{:?}",
        (0..vt.rows).map(|r| vt.line_text(r)).collect::<Vec<_>>()
    );
}

/// 症状 2 前置：启动不整屏灌空行（历史顶部就位，scrollback 为空）。
#[test]
fn startup_does_not_bulk_scroll() {
    let record = temp_record_path("startup");
    let Some(mut session) = spawn_tui(&record, 30) else {
        eprintln!("pty 不可用，跳过");
        return;
    };
    assert!(
        session.wait_for("尚未配置", Duration::from_secs(20)),
        "启动引导出现"
    );
    session.drain(Duration::from_millis(500));
    session.send("\x03");
    session.send("\x03");
    let _ = session.child.wait();

    let vt = recorded_vt(&record, 30);
    assert!(
        vt.scrollback.is_empty(),
        "启动后 scrollback 必须为空（整屏灌空行=历史被顶进 scrollback）：{:?}",
        vt.scrollback
    );
    let first_content = (0..vt.rows)
        .find(|&r| !vt.line_text(r).is_empty())
        .expect("首行内容");
    assert!(
        first_content <= 1,
        "内容必须从屏幕顶部开始（首内容行={}）",
        first_content
    );
}

/// 症状 2：历史超一屏滚入 scrollback 不丢失。
#[test]
fn history_survives_in_scrollback() {
    let record = temp_record_path("scroll");
    let Some(mut session) = spawn_tui(&record, 20) else {
        eprintln!("pty 不可用，跳过");
        return;
    };
    assert!(
        session.wait_for("尚未配置", Duration::from_secs(20)),
        "启动引导出现"
    );
    for i in 0..8 {
        let needle = format!("msg-{i:02}");
        session.send(&format!("{needle}\r"));
        let echoed = session.wait_for(&needle, Duration::from_secs(10));
        assert!(echoed, "第 {i} 条消息出现（屏或 scrollback）");
        // 等 commit 后的错误行稳定（无配置 → 发送失败指引）。
        session.drain(Duration::from_millis(400));
    }
    session.send("\x03");
    session.send("\x03");
    let _ = session.child.wait();

    let vt = recorded_vt(&record, 20);
    assert!(
        vt.all_lines_contain("尚未配置"),
        "启动引导保留在屏或 scrollback：scrollback={:?}",
        vt.scrollback
    );
    for i in 0..8 {
        let needle = format!("msg-{i:02}");
        assert!(
            vt.all_lines_contain(&needle),
            "{needle} 丢失：screen={:?}\nscrollback={:?}",
            (0..vt.rows).map(|r| vt.line_text(r)).collect::<Vec<_>>(),
            vt.scrollback
        );
    }
}
