//! 真实 PTY 端到端：G8/G10 新命令（/tree /fork /clone /login）+ /new 切换修复。
//!
//! - `/tree`：新会话 main-only 树浮层渲染；Enter 切换（同分支幂等）出系统行。
//! - `/fork`：无可分叉消息 → 可操作提示（不弹死浮层）。
//! - `/clone`：真克隆落库 → 系统行确认（/resume 可打开）。
//! - `/new`：真正切换到新会话（G8 修复：此前只建任务不切换）。
//! - `/login`：Codex 状态 + 其余厂商诚实引导（不出现假 OAuth）。
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

struct Session {
    writer: Box<dyn Write + Send>,
    output: std::sync::mpsc::Receiver<String>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

fn spawn_tui() -> Option<Session> {
    let bin = std::env::var("CARGO_BIN_EXE_r-code-tui").ok()?;
    let dir = tempfile::tempdir().ok()?;
    let keep = String::from(dir.path().to_str()?);
    std::mem::forget(dir);
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .ok()?;
    let mut cmd = CommandBuilder::new(bin);
    cmd.args(["--data-dir", &keep]);
    cmd.env("RUST_BACKTRACE", "0");
    let child = pair.slave.spawn_command(cmd).ok()?;
    let writer = pair.master.take_writer().ok()?;
    let reader = pair.master.try_clone_reader().ok()?;
    let master = pair.master;
    let (tx, output) = std::sync::mpsc::channel();
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
    fn wait_for(&mut self, needle: &str, timeout: Duration) -> String {
        fn strip_ansi(text: &str) -> String {
            let mut out = String::new();
            let mut chars = text.chars().peekable();
            while let Some(ch) = chars.next() {
                if ch == '\u{1b}' {
                    if chars.peek() == Some(&'[') {
                        chars.next();
                        for inner in chars.by_ref() {
                            if inner.is_ascii_alphabetic() {
                                break;
                            }
                        }
                    }
                } else {
                    out.push(ch);
                }
            }
            out
        }
        let mut out = String::new();
        let deadline = Instant::now() + timeout;
        loop {
            if strip_ansi(&out).contains(needle) {
                return out;
            }
            let remain = deadline.saturating_duration_since(Instant::now());
            if remain.is_zero() {
                break;
            }
            match self
                .output
                .recv_timeout(remain.min(Duration::from_millis(100)))
            {
                Ok(chunk) => out.push_str(&chunk),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => break,
            }
        }
        panic!(
            "wait_for 超时（needle={needle:?}），实际输出尾段：{:?}",
            &out[out.len().saturating_sub(2000)..]
        );
    }

    fn send(&mut self, keys: &str) {
        let _ = self.writer.write_all(keys.as_bytes());
        let _ = self.writer.flush();
    }

    /// 静置：排空 300ms 输出（Esc 与后续按键之间留间隔，避免 crossterm 把
    /// `\x1b` 与下一键合并成 Alt+<char>）。
    fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_millis(300);
        loop {
            let remain = deadline.saturating_duration_since(Instant::now());
            if remain.is_zero() {
                break;
            }
            match self
                .output
                .recv_timeout(remain.min(Duration::from_millis(50)))
            {
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
    }
}

#[test]
fn tree_fork_clone_login_new_commands_work() {
    let Some(mut session) = spawn_tui() else {
        eprintln!("pty 不可用，跳过");
        return;
    };
    session.wait_for("尚未配置", Duration::from_secs(20));

    // 1) /tree：main-only 树浮层（G8）。
    session.send("/tree\r");
    let out = session.wait_for("enter 切换分支", Duration::from_secs(5));
    assert!(out.contains("main"), "main 分支可见：{out:?}");
    assert!(out.contains("← 当前"), "活跃标记：{out:?}");
    // Enter 切换（同分支幂等）→ 系统行 + 视图重建。
    session.send("\r");
    session.wait_for("已切换到分支 main", Duration::from_secs(5));

    // 2) /fork：新会话无消息 → 可操作提示（不弹空浮层）。
    session.send("/fork\r");
    session.wait_for("还没有可分叉的消息", Duration::from_secs(5));

    // 3) /clone：真克隆落库 → 系统行（标题带克隆后缀）。
    session.send("/clone\r");
    let out = session.wait_for("已克隆会话", Duration::from_secs(10));
    assert!(out.contains("（克隆）"), "克隆标题后缀：{out:?}");
    assert!(out.contains("/resume"), "指引打开途径：{out:?}");

    // 4) /new：真正切换（G8 修复）——系统行 + 清屏重建。
    session.send("/new\r");
    session.wait_for("已新建空白会话", Duration::from_secs(5));

    // 5) /login：Codex 状态行 + 其余厂商诚实引导（不出现假 OAuth 选项文案）。
    session.send("/login\r");
    let out = session.wait_for("其余模型服务", Duration::from_secs(5));
    assert!(
        out.contains("API key") && out.contains("/setup"),
        "诚实引导：{out:?}"
    );
    // Esc 关闭浮层（settle 防 Alt 合并），输入回显确认焦点回编辑器。
    session.send("\x1b");
    session.settle();
    session.send("z");
    session.wait_for("> z", Duration::from_secs(5));

    // 清理。
    session.send("\x03");
    session.send("\x03");
    let _ = session.child.wait();
}
