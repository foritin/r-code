//! Test-only harness fixture binary (T09). Not shipped in installers.
//!
//! Modes (first argument):
//! - `serve`: full peer — initialize handshake, nested host callback during
//!   `harness.start`, progress notifications, graceful cancel.
//! - `malformed`: emits invalid JSON frames around one valid response.
//! - `oversized`: emits a frame exceeding 1 MiB.
//! - `silent`: never responds (initialize timeout fixture).
//! - `blocked`: stops reading stdin (queue-overflow fixture).
//! - `stderr-flood`: floods stderr while answering initialize correctly.
//! - `ignore-cancel`: answers initialize but ignores harness.cancel.

use std::io::{BufRead, Write};

fn send(value: serde_json::Value) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = writeln!(lock, "{value}");
    let _ = lock.flush();
}

fn response(id: &serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: &serde_json::Value, code: i64, message: &str) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn serve_mode() {
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    while let Some(Ok(line)) = lines.next() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let method = message.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = message
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        match method {
            "initialize" => {
                send(response(
                    &id,
                    serde_json::json!({
                        "harnessId": "fixture.harness",
                        "harnessVersion": "0.1.0"
                    }),
                ));
            }
            "harness.start" => {
                // Nested callback: call the host while it awaits our start
                // response. The host reader must serve this concurrently.
                send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "cb-1",
                    "method": "host.tools.list",
                    "params": {}
                }));
                // Read the host's response to our callback.
                if let Some(Ok(reply)) = lines.next() {
                    let _ = reply;
                }
                send(response(&id, serde_json::json!({"started": true})));
                send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "harness.event",
                    "params": {"kind": "progress", "payload": {"note": "started"}}
                }));
            }
            "harness.cancel" => {
                send(response(&id, serde_json::json!({"acknowledged": true})));
                // Stop cleanly after acknowledging.
                let _ = std::io::stdout().flush();
                return;
            }
            "shutdown" => {
                send(response(&id, serde_json::Value::Null));
                return;
            }
            _ => {
                send(error_response(
                    &id,
                    -32601,
                    "fixture does not implement this method",
                ));
            }
        }
    }
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "serve".into());
    match mode.as_str() {
        "serve" => serve_mode(),
        "malformed" => {
            let mut stdout = std::io::stdout();
            let _ = writeln!(stdout, "{{not json at all");
            let _ = writeln!(
                stdout,
                "{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"harnessId\":\"fixture\",\"harnessVersion\":\"0\"}}}}"
            );
            let _ = writeln!(stdout, "[1,2,3");
            let _ = stdout.flush();
            // Keep alive briefly so the host observes several bad frames.
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
        "oversized" => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let blob = "x".repeat(1024 * 1024 + 4096);
            let _ = writeln!(lock, "{{\"jsonrpc\":\"2.0\",\"method\":\"harness.event\",\"params\":{{\"blob\":\"{blob}\"}}}}");
            let _ = lock.flush();
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
        "silent" => {
            // Drain stdin slowly, never answer.
            let stdin = std::io::stdin();
            let mut lines = stdin.lock().lines();
            while let Some(Ok(_)) = lines.next() {}
        }
        "blocked" => {
            // Never read stdin, never write stdout.
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
        "stderr-flood" => {
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    let stderr = std::io::stderr();
                    let mut lock = stderr.lock();
                    for index in 0..20_000 {
                        let _ = writeln!(lock, "noise line {index} with some padding bytes");
                    }
                    let _ = lock.flush();
                });
                let stdin = std::io::stdin();
                let mut lines = stdin.lock().lines();
                while let Some(Ok(line)) = lines.next() {
                    if let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) {
                        if message.get("method").and_then(|m| m.as_str()) == Some("initialize") {
                            let id = message
                                .get("id")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            send(response(
                                &id,
                                serde_json::json!({
                                    "harnessId": "fixture.stderr",
                                    "harnessVersion": "0.1.0"
                                }),
                            ));
                            break;
                        }
                    }
                }
            });
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
        "ignore-cancel" => {
            let stdin = std::io::stdin();
            let mut lines = stdin.lock().lines();
            while let Some(Ok(line)) = lines.next() {
                if let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) {
                    if message.get("method").and_then(|m| m.as_str()) == Some("initialize") {
                        let id = message
                            .get("id")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        send(response(
                            &id,
                            serde_json::json!({
                                "harnessId": "fixture.stubborn",
                                "harnessVersion": "0.1.0"
                            }),
                        ));
                    }
                    // harness.cancel deliberately ignored: never reply.
                }
            }
        }
        other => {
            eprintln!("unknown fixture mode {other:?}");
            std::process::exit(2);
        }
    }
}
