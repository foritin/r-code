//! P03 — Codex App Server consumption of public host.process.read pages.

use base64::Engine as _;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};

const CODEX: &str = env!("CARGO_BIN_EXE_r-code-harness-codex");

struct CodexDriver {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr: Option<ChildStderr>,
    read_cursors: Vec<u64>,
    host_methods: Vec<String>,
}

impl Drop for CodexDriver {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl CodexDriver {
    fn spawn() -> Self {
        let mut child = Command::new(CODEX)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn Codex harness");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let stderr = child.stderr.take();
        Self {
            child,
            stdin,
            stdout,
            stderr,
            read_cursors: Vec::new(),
            host_methods: Vec::new(),
        }
    }

    fn send(&mut self, value: Value) {
        r_code_harness_protocol::rpc::decode_frame(value.to_string().as_bytes())
            .unwrap_or_else(|error| panic!("test attempted invalid RPC frame {value}: {error}"));
        writeln!(self.stdin, "{value}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn receive(&mut self) -> Value {
        let mut line = String::new();
        let read = self.stdout.read_line(&mut line).unwrap();
        if read == 0 {
            let mut diagnostics = String::new();
            if let Some(mut stderr) = self.stderr.take() {
                let _ = stderr.read_to_string(&mut diagnostics);
            }
            panic!(
                "Codex harness closed before replying; status={:?}; stderr={diagnostics}",
                self.child.try_wait().ok().flatten()
            );
        }
        serde_json::from_str(line.trim())
            .unwrap_or_else(|error| panic!("invalid Codex harness frame {line:?}: {error}"))
    }

    fn reply(&mut self, request: &Value, result: Value) {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": result
        }));
    }

    fn initialize(&mut self) {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 100,
            "method": "initialize",
            "params": {
                "protocol": "r-code-harness/1",
                "host_api": {"major": 1, "minor": 1},
                "identity": identity(),
                "granted_services": [
                    "host.process.open",
                    "host.process.read",
                    "host.process.write",
                    "host.process.close",
                    "host.checkpoint.save"
                ],
                "harness_config": {},
                "limits": {
                    "max_frame_bytes": 1048576,
                    "max_queue_bytes": 16777216,
                    "initialize_timeout_ms": 10000,
                    "cancel_grace_ms": 5000
                }
            }
        }));
        let response = self.receive();
        assert_eq!(response["id"], 100);
        assert_eq!(response["result"]["harnessId"], "codex.r-code");
    }

    fn start_until_read(&mut self) -> Value {
        self.initialize();
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 200,
            "method": "harness.start",
            "params": {
                "identity": identity(),
                "contract": {
                    "workspaceRoot": "D:/workspace",
                    "objective": "inspect the project"
                },
                "transcript_cursor": 0
            }
        }));
        loop {
            let message = self.receive();
            match message["method"].as_str() {
                Some("host.process.open") => {
                    self.host_methods.push("host.process.open".into());
                    self.reply(&message, json!({"handle": "inner-handle"}));
                }
                Some("host.process.write") => {
                    self.host_methods.push("host.process.write".into());
                    self.reply(&message, json!({}));
                }
                Some("host.process.read") => {
                    self.host_methods.push("host.process.read".into());
                    let cursor = message["params"]["cursor"].as_u64().unwrap();
                    self.read_cursors.push(cursor);
                    assert_eq!(message["params"]["handle"], "inner-handle");
                    assert_eq!(message["params"]["maxBytes"], 64 * 1024);
                    assert_eq!(message["params"]["waitMs"], 30_000);
                    return message;
                }
                other => panic!("unexpected pre-read message {other:?}: {message}"),
            }
        }
    }

    fn reply_read(&mut self, request: &Value, page: Value) {
        assert_eq!(request["method"], "host.process.read");
        self.reply(request, page);
    }

    fn receive_next_read(&mut self) -> Value {
        let message = self.receive();
        assert_eq!(
            message["method"], "host.process.read",
            "expected another read, got {message}"
        );
        let cursor = message["params"]["cursor"].as_u64().unwrap();
        self.read_cursors.push(cursor);
        message
    }

    fn finish_started_run(&mut self) -> Value {
        loop {
            let message = self.receive();
            match message["method"].as_str() {
                Some("host.process.write") => {
                    self.host_methods.push("host.process.write".into());
                    self.reply(&message, json!({}));
                }
                Some("host.checkpoint.save") => {
                    self.host_methods.push("host.checkpoint.save".into());
                    self.reply(
                        &message,
                        json!({
                            "checkpoint": {
                                "schema": 1,
                                "blob_id": "blob:codex-checkpoint",
                                "bytes": 1,
                                "sha256": "sha256:checkpoint"
                            },
                            "revision": 1
                        }),
                    );
                }
                Some("host.process.close") => {
                    self.host_methods.push("host.process.close".into());
                    self.reply(&message, json!({}));
                }
                None if message["id"] == 200 => return message,
                other => panic!("unexpected completion message {other:?}: {message}"),
            }
        }
    }
}

fn identity() -> Value {
    json!({
        "task_id": "task-codex-p03",
        "branch_id": "branch-codex-p03",
        "run_id": "run-codex-p03",
        "attempt_id": "attempt-codex-p03",
        "generation": 1
    })
}

fn data(sequence: u64, stream: &str, bytes: &[u8]) -> Value {
    json!({
        "kind": "data",
        "sequence": sequence,
        "stream": stream,
        "dataBase64": base64::engine::general_purpose::STANDARD.encode(bytes)
    })
}

fn assert_start_error(message: Value) {
    assert_eq!(
        message["id"], 200,
        "expected harness.start response: {message}"
    );
    assert!(
        message["error"].is_object(),
        "expected error response: {message}"
    );
}

#[test]
fn fragmented_multiple_event_pages_ignore_stderr_and_advance_only_validated_cursor() {
    let mut driver = CodexDriver::spawn();
    let first_read = driver.start_until_read();
    let first_fragment = br#"{"method":"initi"#;
    driver.reply_read(
        &first_read,
        json!({
            "frames": [
                data(0, "stdout", first_fragment),
                data(1, "stderr", b"not-json diagnostics\n")
            ],
            "nextCursor": 2,
            "terminal": false
        }),
    );

    let empty_read = driver.receive_next_read();
    assert_eq!(empty_read["params"]["cursor"], 2);
    driver.reply_read(
        &empty_read,
        json!({"frames": [], "nextCursor": 2, "terminal": false}),
    );

    let final_read = driver.receive_next_read();
    assert_eq!(final_read["params"]["cursor"], 2);
    let remainder = br#"alized","params":{"threadId":"thread-p03"}}
{"method":"item/started","params":{"itemId":"item-p03"}}
"#;
    driver.reply_read(
        &final_read,
        json!({
            "frames": [
                data(2, "stdout", remainder),
                {"kind":"eof","sequence":3,"stream":"stderr"},
                {"kind":"eof","sequence":4,"stream":"stdout"},
                {"kind":"exit","sequence":5,"exitCode":0}
            ],
            "nextCursor": 6,
            "terminal": true,
            "exitCode": 0
        }),
    );

    let started = driver.finish_started_run();
    assert_eq!(started["result"]["kind"], "initialized");
    assert_eq!(started["result"]["thread_id"], "thread-p03");
    assert_eq!(driver.read_cursors, [0, 2, 2]);
    assert_eq!(
        driver.host_methods,
        [
            "host.process.open",
            "host.process.write",
            "host.process.read",
            "host.process.write",
            "host.checkpoint.save",
            "host.process.close"
        ]
    );
}

#[test]
fn sdk_rejects_nonempty_terminal_without_exit_and_metadata_mismatch_but_allows_terminal_empty() {
    for page in [
        json!({
            "frames": [data(0, "stdout", b"partial")],
            "nextCursor": 1,
            "terminal": true
        }),
        json!({
            "frames": [{"kind":"eof","sequence":0,"stream":"stdout"}],
            "nextCursor": 1,
            "terminal": true
        }),
        json!({
            "frames": [{"kind":"exit","sequence":0,"exitCode":null}],
            "nextCursor": 1,
            "terminal": true,
            "exitCode": 0
        }),
    ] {
        let mut driver = CodexDriver::spawn();
        let read = driver.start_until_read();
        driver.reply_read(&read, page);
        assert_start_error(driver.receive());
    }

    let mut terminal_empty = CodexDriver::spawn();
    let read = terminal_empty.start_until_read();
    terminal_empty.reply_read(
        &read,
        json!({"frames": [], "nextCursor": 0, "terminal": true, "exitCode": 7}),
    );
    assert_start_error(terminal_empty.receive());

    let mut exit_none = CodexDriver::spawn();
    let read = exit_none.start_until_read();
    exit_none.reply_read(
        &read,
        json!({
            "frames": [{"kind":"exit","sequence":0,"exitCode":null}],
            "nextCursor": 1,
            "terminal": true
        }),
    );
    assert_start_error(exit_none.receive());
}

#[test]
fn newline_free_stdout_is_bounded_at_one_mebibyte() {
    let mut driver = CodexDriver::spawn();
    let mut read = driver.start_until_read();
    let chunk = vec![b'x'; 64 * 1024];
    for sequence in 0..17u64 {
        driver.reply_read(
            &read,
            json!({
                "frames": [data(sequence, "stdout", &chunk)],
                "nextCursor": sequence + 1,
                "terminal": false
            }),
        );
        if sequence < 16 {
            read = driver.receive_next_read();
            assert_eq!(read["params"]["cursor"], sequence + 1);
        }
    }
    assert_start_error(driver.receive());
}

#[test]
fn source_staged_manifest_and_public_authority_surfaces_remain_in_lockstep() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source_bytes = std::fs::read(root.join("plugins/codex/harness.json")).unwrap();
    let staged_bytes = std::fs::read(root.join("src-tauri/plugins/codex/harness.json")).unwrap();
    let source: Value = serde_json::from_slice(&source_bytes).unwrap();
    let staged: Value = serde_json::from_slice(&staged_bytes).unwrap();
    assert_eq!(staged, source);
    assert_eq!(source["apiMajor"], 1);
    assert_eq!(source["apiMinor"], 1);

    let services = source["requestedHostServices"].as_array().unwrap();
    let mut process = services
        .iter()
        .filter_map(Value::as_str)
        .filter(|service| service.starts_with("host.process."))
        .collect::<Vec<_>>();
    process.sort_unstable();
    assert_eq!(
        process,
        [
            "host.process.close",
            "host.process.open",
            "host.process.read",
            "host.process.write"
        ]
    );
    let mut non_process = services
        .iter()
        .filter_map(Value::as_str)
        .filter(|service| !service.starts_with("host.process."))
        .collect::<Vec<_>>();
    non_process.sort_unstable();
    assert_eq!(
        non_process,
        [
            "host.approvals.request",
            "host.artifacts.put",
            "host.artifacts.read",
            "host.checkpoint.save",
            "host.children.cancel",
            "host.children.spawn",
            "host.children.wait",
            "host.completion.propose",
            "host.questions.ask",
            "host.tools.call",
            "host.tools.list"
        ]
    );
    assert_eq!(
        r_code_harness_protocol::canonical_input_hash(&source),
        r_code_harness_protocol::canonical_input_hash(&staged)
    );

    for path in [
        root.join("plugins/codex/src/app_server.rs"),
        root.join("crates/r-code-harness-protocol/src/rpc.rs"),
        root.join("crates/r-code-harness-protocol/schema/harness-v1.schema.json"),
    ] {
        let text = std::fs::read_to_string(path).unwrap();
        assert!(!text.contains("codex.event.next"));
    }
    let app_server = std::fs::read_to_string(root.join("plugins/codex/src/app_server.rs")).unwrap();
    assert!(app_server.contains("process_read"));
    let packaging =
        std::fs::read_to_string(root.join("scripts/harness-packaging.test.mjs")).unwrap();
    assert!(packaging.contains("staged Codex manifest must match its source"));
    assert!(packaging.contains("host.process.read"));
}
