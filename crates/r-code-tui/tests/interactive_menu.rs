//! 真实 PTY 端到端：驱动真实 r-code-tui 二进制，注入按键，断言 `/` 菜单
//! 出现且 ↑/↓ 移动选中项（用户实测"上下键不起作用"的回归测试）。
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

struct Session {
    writer: Box<dyn Write + Send>,
    output: std::sync::mpsc::Receiver<String>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    // ConPTY：master 句柄一旦 drop，子进程立刻收到输入挂断而走优雅退出
    // （表现：启动即 EOF、无任何渲染输出）。Unix 侧 reader 的 fd dup 本就
    // 保活 master，此字段无副作用；Windows 必须由 Session 持有到会话结束。
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

fn spawn_tui() -> Option<Session> {
    let bin = std::env::var("CARGO_BIN_EXE_r-code-tui").ok()?;
    let dir = tempfile::tempdir().ok()?;
    // tempfile 会 drop；把路径搬到泄漏的 Box 里保持存活。
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
    // 读线程：阻塞读推进 channel；wait_for 侧用 recv_timeout 控制 deadline，
    // 阻塞读不再绕过超时。
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
    /// 读输出直到出现 `needle` 或超时；返回累计输出。
    fn wait_for(&mut self, needle: &str, timeout: Duration) -> String {
        // 匹配基于去 ANSI 文本（live 行 prompt 与正文之间有 reset 序列）。
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
fn slash_menu_arrow_keys_move_selection() {
    let Some(mut session) = spawn_tui() else {
        eprintln!("pty 不可用，跳过");
        return;
    };
    // 1) 启动：出现引导（真实模式，无配置）。
    session.wait_for("尚未配置", Duration::from_secs(20));
    // 2) 输入 "/"：斜杠菜单出现。
    session.send("/");
    let out = session.wait_for("/model", Duration::from_secs(5));
    assert!(out.contains("/model"), "菜单必须出现：{out:?}");
    // 3) ↓：选中从 /model 移到 /setup（注册表第二位；cyan bold › 前缀随行重写）。
    session.send("\x1b[B");
    let out = session.wait_for("› /setup", Duration::from_secs(5));
    assert!(out.contains("› /setup"), "↓ 必须移动选中项：{out:?}");
    // 4) ↑：移回 /model。
    session.send("\x1b[A");
    let out = session.wait_for("› /model", Duration::from_secs(5));
    assert!(out.contains("› /model"), "↑ 必须移回：{out:?}");
    // 5) 输入完整消息并发送（无配置 → 错误 System 行 commit，输入行恢复可用）。
    session.send("\x15"); // Ctrl-U 清行（防残留）
    session.send("产出一首诗\r");
    let out = session.wait_for("发送失败", Duration::from_secs(10));
    assert!(
        out.contains("发送失败") && out.contains("anthropic"),
        "错误行必须 commit 且含指引：{out:?}"
    );
    // 6) 错误后输入行仍可输入（提示符行重绘出现）。
    session.send("next");
    // wait_for 内部已做去 ANSI 匹配（超时会 panic）；此处仅消费返回值。
    let _ = session.wait_for("> next", Duration::from_secs(5));
    // 7) 清理：Ctrl-C 两次退出。
    session.send("\x03");
    session.send("\x03");
    let _ = session.child.wait();
}

/// 症状3回归：/setup 引导流可达且可操作（选预设 → 过滤 → 进 key 步 → 掩码
/// 输入 → Esc 返回）。不提交保存——PTY e2e 编译为非 test 配置，保存会写
/// 真实平台凭据后端（Windows Credential Manager，账号 = provider 名），
/// 会覆盖用户真实 key；保存路径由 setup_flow 单测（test 凭据后端，config_dir
/// 命名空间隔离）覆盖。
#[test]
fn setup_flow_reachable_and_cancellable() {
    let Some(mut session) = spawn_tui() else {
        eprintln!("pty 不可用，跳过");
        return;
    };
    session.wait_for("尚未配置", Duration::from_secs(20));
    // 1) /setup 打开向导：预设列表出现。
    session.send("/setup\r");
    let out = session.wait_for("配置模型服务 — 选择预设", Duration::from_secs(5));
    assert!(out.contains("配置模型服务"), "向导标题：{out:?}");
    // 2) 过滤 "open"：命中 openai 预设。
    session.send("open");
    let out = session.wait_for("› OpenAI", Duration::from_secs(5));
    assert!(out.contains("› OpenAI"), "过滤后选中 OpenAI：{out:?}");
    // 3) Enter 进 key 步：端点与掩码行出现。
    session.send("\r");
    let out = session.wait_for("输入 API Key", Duration::from_secs(5));
    assert!(out.contains("api.openai.com"), "端点行：{out:?}");
    // 4) 输入被掩码（*** 而非原文）。
    session.send("sk-secret-value");
    let out = session.wait_for("***", Duration::from_secs(5));
    assert!(
        !out.contains("sk-secret-value"),
        "key 原文不得出现在输出：{out:?}"
    );
    // 5) Esc 返回选预设步；再 Esc 关闭向导。
    session.send("\x1b");
    let out = session.wait_for("配置模型服务 — 选择预设", Duration::from_secs(5));
    assert!(out.contains("配置模型服务"), "Esc 返回预设步：{out:?}");
    session.send("\x1b");
    // 向导关闭后输入行恢复可用：键入 x 应出现在提示符后
    //（wait_for 按 strip_ANSI 文本匹配，命中即证明输入行回来了）。
    session.send("x");
    // wait_for 超时自行 panic，命中即证明输入行恢复。
    let _ = session.wait_for("> x", Duration::from_secs(5));
    // 清理。
    session.send("\x03");
    session.send("\x03");
    let _ = session.child.wait();
}
