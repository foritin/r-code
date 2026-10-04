//! A07 — SDK serve loop: EOF fast-fail of in-flight host calls, cancellation
//! wakeup, shutdown acknowledgement. Drives the `sdk_chaos_fixture` example
//! binary over real stdio.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn fixture_binary() -> PathBuf {
    let output =
        std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args([
                "build",
                "-p",
                "r-code-harness-sdk",
                "--example",
                "sdk_chaos_fixture",
            ])
            .output()
            .expect("build chaos fixture");
    assert!(
        output.status.success(),
        "fixture build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let exe = if cfg!(windows) {
        "sdk_chaos_fixture.exe"
    } else {
        "sdk_chaos_fixture"
    };
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/examples")
        .join(exe)
}

fn identity() -> serde_json::Value {
    serde_json::json!({
        "task_id": "t", "branch_id": "b", "run_id": "r", "attempt_id": "a", "generation": 1
    })
}

fn spawn_fixture() -> (
    Child,
    std::process::ChildStdin,
    BufReader<std::process::ChildStdout>,
) {
    let mut child = Command::new(fixture_binary())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fixture");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    (child, stdin, BufReader::new(stdout))
}

fn send(stdin: &mut std::process::ChildStdin, value: &serde_json::Value) {
    writeln!(stdin, "{value}").expect("write frame");
    stdin.flush().expect("flush");
}

fn read_reply(reader: &mut BufReader<std::process::ChildStdout>) -> serde_json::Value {
    let mut line = String::new();
    reader.read_line(&mut line).expect("read reply");
    serde_json::from_str(line.trim()).expect("parse reply")
}

fn initialize(id: u64) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "initialize",
        "params": {
            "protocol": "r-code-harness/1",
            "host_api": {"major": 1, "minor": 0},
            "identity": identity(),
            "granted_services": [],
            "harness_config": {},
            "limits": {"max_frame_bytes": 1048576, "max_queue_bytes": 16777216,
                        "initialize_timeout_ms": 10000, "cancel_grace_ms": 5000}
        }
    })
}

fn start(id: u64) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "harness.start",
        "params": {
            "identity": identity(),
            "contract": {"objective": "hold a host call"},
            "input": {"message_id": "m1", "input_seq": 1, "kind": "User", "text": "go"},
            "capabilities": {}
        }
    })
}

fn cancel(id: u64) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "harness.cancel",
        "params": {"identity": identity(), "reason": "test"}
    })
}

fn shutdown(id: u64) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "shutdown",
        "params": {"identity": identity()}
    })
}

#[test]
fn a07_eof_fails_inflight_host_calls_fast() {
    let (mut child, mut stdin, mut reader) = spawn_fixture();
    send(&mut stdin, &initialize(1));
    let reply = read_reply(&mut reader);
    assert_eq!(reply["id"], 1, "initialize ok: {reply}");
    // 启动会卡在 300s host_call 里（宿主不回）。
    send(&mut stdin, &start(2));
    std::thread::sleep(Duration::from_millis(300));
    // EOF：stdin 关闭。EOF 排空应立即终结在飞 host_call，进程数秒内退出
    //（修复前会挂满 300s）。
    drop(stdin);
    let started = Instant::now();
    let status = wait_with_timeout(&mut child, Duration::from_secs(10));
    assert!(
        status.is_some(),
        "process must exit promptly after EOF (waited 10s)"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "EOF drain must not wait out the 300s host_call"
    );
}

#[test]
fn a07_cancel_wins_over_inflight_host_call() {
    let (mut child, mut stdin, mut reader) = spawn_fixture();
    send(&mut stdin, &initialize(1));
    let _ = read_reply(&mut reader);
    send(&mut stdin, &start(2));
    std::thread::sleep(Duration::from_millis(300));
    // 取消到达：host_call select 取消 → on_start 返回 → start 响应快速回包。
    send(&mut stdin, &cancel(9));
    let started = Instant::now();
    // start 的响应可能在 cancel 的 ack 之前或之后；读到 id=2 即证明取消
    // 打断了 300s 等待。
    let mut got_start = false;
    let mut got_cancel = false;
    while started.elapsed() < Duration::from_secs(10) && !(got_start && got_cancel) {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
            break;
        }
        let Ok(reply) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        match reply["id"].as_i64() {
            Some(2) => got_start = true,
            Some(9) => got_cancel = true,
            _ => {}
        }
    }
    assert!(
        got_start,
        "cancel must interrupt the in-flight host call (start settled <10s)"
    );
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn a07_shutdown_ack_delivered() {
    let (mut child, mut stdin, mut reader) = spawn_fixture();
    send(&mut stdin, &initialize(1));
    let _ = read_reply(&mut reader);
    send(&mut stdin, &shutdown(7));
    let reply = read_reply(&mut reader);
    assert_eq!(reply["id"], 7, "shutdown ack frame delivered: {reply}");
    let status = wait_with_timeout(&mut child, Duration::from_secs(10));
    assert!(status.is_some(), "process exits after shutdown");
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}
