//! 真实 PTY 端到端：M8 三命令（G7/G9/G11）的交互路径回归。
//!
//! - `/session`：会话统计卡 commit 进 transcript（未落盘会话文件回退行）。
//! - `/export <绝对路径>.md`：系统行确认 + 磁盘上真有 markdown 文件。
//! - `/copy`：无 assistant 回复时的可操作提示。
//! - `/setup` 环境变量模式（G11）：key 步 Tab 切换 → 变量清单渲染；Esc 退出
//!   不落盘（PTY 编译为非 test 配置，明文 key 保存会写真实凭据后端——env
//!   模式保存只写 config.toml，但本测试只验切换与渲染，不提交）。
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
}

#[test]
fn session_card_and_export_commands_work() {
    let Some(mut session) = spawn_tui() else {
        eprintln!("pty 不可用，跳过");
        return;
    };
    session.wait_for("尚未配置", Duration::from_secs(20));

    // 1) /session：统计卡进 transcript（G9）。新会话未发送过 → 会话文件行
    //    显示"未落盘"回退。
    session.send("/session\r");
    let out = session.wait_for("R-Code CLI 会话", Duration::from_secs(5));
    assert!(out.contains("messages:"), "消息计数行：{out:?}");
    assert!(
        out.contains("未落盘"),
        "未发送过的会话无 JSONL 文件：{out:?}"
    );

    // 2) /export <绝对路径>：系统行确认 + 磁盘真有文件（G7）。
    let target = tempfile::tempdir().expect("export dir");
    let export_path = target.path().join("pty-export.md");
    let export_arg = export_path.to_string_lossy().replace('\\', "/");
    session.send(format!("/export {export_arg}\r").as_str());
    session.wait_for("已导出会话", Duration::from_secs(5));
    let content = std::fs::read_to_string(&export_path).expect("导出文件必须落盘");
    assert!(
        content.contains("# R-Code CLI 会话导出"),
        "markdown 头：{content}"
    );
    // 引导行（System）也在导出里——transcript 视图与所见一致。
    assert!(content.contains("尚未配置"), "{content}");

    // 3) /copy：无 assistant 回复 → 可操作提示（G7）。
    session.send("/copy\r");
    session.wait_for("没有可复制的回复", Duration::from_secs(5));

    // 清理。
    session.send("\x03");
    session.send("\x03");
    let _ = session.child.wait();
}

#[test]
fn setup_env_mode_toggle_renders_var_list() {
    let Some(mut session) = spawn_tui() else {
        eprintln!("pty 不可用，跳过");
        return;
    };
    session.wait_for("尚未配置", Duration::from_secs(20));
    // 1) /setup → 过滤 openai → Enter 进 key 步。
    session.send("/setup\r");
    session.wait_for("配置模型服务 — 选择预设", Duration::from_secs(5));
    session.send("open");
    session.wait_for("› OpenAI", Duration::from_secs(5));
    session.send("\r");
    session.wait_for("输入 API Key", Duration::from_secs(5));
    // 2) Tab 切环境变量模式（G11）：变量清单出现（厂商别名 + profile 变量）。
    session.send("\t");
    let out = session.wait_for("环境变量鉴权", Duration::from_secs(5));
    assert!(out.contains("OPENAI_API_KEY"), "厂商别名：{out:?}");
    assert!(
        out.contains("R_CODE_PROVIDER_OPENAI_API_KEY"),
        "profile 作用域变量：{out:?}"
    );
    // 3) 不提交（Esc Esc 退出；本测试只验切换与渲染）。逐步等待：背靠背
    //    写入会被 crossterm 解析成 Alt+<char>（\x1b 前缀合并）。
    session.send("\x1b");
    session.wait_for("配置模型服务 — 选择预设", Duration::from_secs(5));
    session.send("\x1b");
    session.send("x");
    let _ = session.wait_for("> x", Duration::from_secs(5));
    session.send("\x03");
    session.send("\x03");
    let _ = session.child.wait();
}
