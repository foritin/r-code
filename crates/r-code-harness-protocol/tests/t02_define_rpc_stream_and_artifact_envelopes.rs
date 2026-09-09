//! T02 — RPC, stream and artifact envelopes.
//!
//! Acceptance: native and external fixtures use identical wire shapes; no
//! secret-bearing Provider or product DTO leaks onto the protocol; frame-size
//! and unknown-method error fixtures behave deterministically.

use r_code_harness_protocol::*;

fn run_identity() -> RunIdentity {
    RunIdentity {
        task_id: "task-1".into(),
        branch_id: "branch-1".into(),
        run_id: "run-1".into(),
        attempt_id: "attempt-1".into(),
        generation: 1,
    }
}

#[test]
fn native_and_external_fixtures_use_identical_wire_shapes() {
    // "Native" side: built through typed DTOs.
    let typed = InitializeParams {
        protocol: "r-code-harness/1".into(),
        host_api: ApiVersion::new(1, 3),
        identity: run_identity(),
        granted_services: vec![HostService::ModelStream, HostService::ToolsCall],
        harness_config: serde_json::json!({"mode": "repair"}),
        limits: ProtocolLimits::default(),
    };
    let native = serde_json::to_value(&typed).expect("serialize");

    // "External" side: an independently authored JSON document with the same
    // contract, byte-for-byte equal on the wire.
    let external: serde_json::Value = serde_json::json!({
        "protocol": "r-code-harness/1",
        "host_api": {"major": 1, "minor": 3},
        "identity": {
            "task_id": "task-1",
            "branch_id": "branch-1",
            "run_id": "run-1",
            "attempt_id": "attempt-1",
            "generation": 1
        },
        "granted_services": ["host.model.stream", "host.tools.call"],
        "harness_config": {"mode": "repair"},
        "limits": {
            "max_frame_bytes": 1048576,
            "max_queue_bytes": 16777216,
            "initialize_timeout_ms": 10000,
            "cancel_grace_ms": 5000
        }
    });
    assert_eq!(native, external);

    // Round trip back through the typed layer.
    let parsed: InitializeParams = serde_json::from_value(native).expect("deserialize");
    assert_eq!(parsed, typed);
}

#[test]
fn rpc_round_trips_and_direction_methods_are_enumerated() {
    let request = RpcMessage::Request(RpcRequest {
        jsonrpc: "2.0".into(),
        id: RpcId::Number(7),
        method: "host.tools.call".into(),
        params: Some(serde_json::json!({"tool": "read_file"})),
    });
    let frame = encode_frame(&request).expect("encode");
    let decoded = decode_frame(&frame[..frame.len() - 1]).expect("decode");
    assert_eq!(decoded, request);

    let notification = RpcMessage::Notification(RpcNotification {
        jsonrpc: "2.0".into(),
        method: "harness.event".into(),
        params: Some(
            serde_json::to_value(HarnessEventParams {
                kind: EventKind::Progress,
                payload: serde_json::json!({"note": "planning"}),
                stream_id: None,
            })
            .unwrap(),
        ),
    });
    let frame = encode_frame(&notification).expect("encode");
    let decoded = decode_frame(&frame[..frame.len() - 1]).expect("decode");
    assert_eq!(decoded, notification);

    for method in HOST_TO_PLUGIN_METHODS {
        assert!(
            is_known_method(method),
            "host->plugin method {method} known"
        );
    }
    for method in PLUGIN_TO_HOST_METHODS {
        assert!(
            is_known_method(method),
            "plugin->host method {method} known"
        );
    }
    assert!(!is_known_method("host.debug.spawn"));
    assert!(!is_known_method("harness.start.extra"));
}

#[test]
fn oversized_frames_are_rejected_on_encode_and_decode() {
    let big = RpcMessage::Notification(RpcNotification {
        jsonrpc: "2.0".into(),
        method: "harness.event".into(),
        params: Some(serde_json::json!({"blob": "x".repeat(MAX_FRAME_BYTES)})),
    });
    match encode_frame(&big) {
        Err(FrameError::TooLarge { size, max }) => {
            assert!(size > max);
            assert_eq!(max, MAX_FRAME_BYTES);
        }
        other => panic!("expected TooLarge, got {other:?}"),
    }

    let oversized_line = format!(
        "{{\"jsonrpc\":\"2.0\",\"method\":\"harness.event\",\"params\":{{\"blob\":\"{}\"}}}}",
        "x".repeat(MAX_FRAME_BYTES)
    );
    assert!(matches!(
        decode_frame(oversized_line.as_bytes()),
        Err(FrameError::TooLarge { .. })
    ));
}

#[test]
fn unknown_methods_fail_closed_with_typed_errors() {
    let err = reject_unknown_method("host.model.vaporize").expect("rejected");
    assert_eq!(err.code, -32601);
    assert!(reject_unknown_method("host.model.stream").is_none());

    let response = RpcResponse {
        jsonrpc: "2.0".into(),
        id: RpcId::Text("req-9".into()),
        result: None,
        error: Some(RpcError::method_not_found("host.model.vaporize")),
    };
    let frame = encode_frame(&RpcMessage::Response(response.clone())).unwrap();
    let decoded = decode_frame(&frame[..frame.len() - 1]).unwrap();
    match decoded {
        RpcMessage::Response(r) => {
            assert_eq!(r.error.expect("error present").code, -32601);
            assert_eq!(r.id, RpcId::Text("req-9".into()));
        }
        other => panic!("unexpected frame {other:?}"),
    }
}

#[test]
fn large_content_travels_via_versioned_artifact_refs() {
    let artifact = ArtifactRef {
        schema: ArtifactRef::SCHEMA,
        blob_id: "blob:sha256:abc".into(),
        bytes: 5 * 1024 * 1024,
        sha256: "abc".into(),
        media_type: Some("image/png".into()),
    };
    let message = ModelMessage {
        role: ModelRole::User,
        content: vec![
            ContentBlock::Text {
                text: "see attachment".into(),
            },
            ContentBlock::Image {
                artifact: artifact.clone(),
            },
        ],
    };
    let request = ModelStreamRequest {
        selection: Some("provider-a/model-x".into()),
        messages: vec![message],
        tools: Vec::new(),
        inference: None,
        deadline_ms: Some(120_000),
    };
    let value = serde_json::to_value(&request).expect("serialize");
    // The binary bytes are NOT inline: only the versioned reference is.
    assert_eq!(value["messages"][0]["content"][1]["artifact"]["schema"], 1);
    assert_eq!(
        value["messages"][0]["content"][1]["artifact"]["blob_id"],
        "blob:sha256:abc"
    );
    let _frame_stays_small = encode_frame(&RpcMessage::Request(RpcRequest {
        jsonrpc: "2.0".into(),
        id: RpcId::Number(1),
        method: "host.model.stream".into(),
        params: Some(value),
    }))
    .expect("5MiB image stays under frame limit as a reference");

    // Stream chunks carry correlated ids and usage stays honest.
    let chunk = StreamEvent {
        stream_id: "stream-1".into(),
        sequence: 3,
        payload: StreamPayload::Finish {
            reason: "stop".into(),
            usage: ModelUsage {
                input_tokens: Some(10),
                output_tokens: Some(5),
                cost_micros: None,
            },
        },
        done: Some(true),
    };
    let value = serde_json::to_value(&chunk).unwrap();
    assert_eq!(value["stream_id"], "stream-1");
    assert_eq!(value["sequence"], 3);
    assert_eq!(value["done"], true);
    assert!(value["usage"].get("cost_micros").is_none());
}

#[test]
fn no_secret_bearing_fields_leak_onto_the_protocol() {
    fn scan(value: &serde_json::Value, path: &str, hits: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, val) in map {
                    let lower = key.to_ascii_lowercase();
                    if [
                        "api_key",
                        "apikey",
                        "secret",
                        "password",
                        "credential",
                        "bearer",
                    ]
                    .iter()
                    .any(|bad| lower.contains(bad))
                    {
                        hits.push(format!("{path}.{key}"));
                    }
                    scan(val, &format!("{path}.{key}"), hits);
                }
            }
            serde_json::Value::Array(items) => {
                for (idx, item) in items.iter().enumerate() {
                    scan(item, &format!("{path}[{idx}]"), hits);
                }
            }
            _ => {}
        }
    }

    let samples: Vec<serde_json::Value> = vec![
        serde_json::to_value(ModelStreamRequest {
            selection: Some("provider-a/model-x".into()),
            messages: vec![ModelMessage {
                role: ModelRole::User,
                content: vec![ContentBlock::Text { text: "hi".into() }],
            }],
            tools: vec![ToolDescriptor {
                name: "read_file".into(),
                description: String::new(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
            inference: Some(serde_json::json!({"temperature": 0.2})),
            deadline_ms: None,
        })
        .unwrap(),
        serde_json::to_value(ToolCallRequest {
            tool: "bash".into(),
            input: serde_json::json!({"command": "ls"}),
        })
        .unwrap(),
        serde_json::to_value(ProcessOpenRequest {
            profile: "app-server".into(),
            arguments: vec!["--serve".into()],
            cwd: None,
            env: Some(
                [("PATH".to_string(), "/usr/bin".to_string())]
                    .into_iter()
                    .collect(),
            ),
        })
        .unwrap(),
        serde_json::to_value(InitializeParams {
            protocol: "r-code-harness/1".into(),
            host_api: ApiVersion::new(1, 0),
            identity: run_identity(),
            granted_services: HostService::ALL.to_vec(),
            harness_config: serde_json::json!({}),
            limits: ProtocolLimits::default(),
        })
        .unwrap(),
    ];

    for sample in &samples {
        let mut hits = Vec::new();
        scan(sample, "$", &mut hits);
        assert!(hits.is_empty(), "secret-bearing fields leaked: {hits:?}");
    }
}

#[test]
fn provenance_distinguishes_host_facts_from_plugin_observations() {
    let host_fact = EventEnvelope {
        seq: 11,
        task_id: "task-1".into(),
        run_id: "run-1".into(),
        kind: EventKind::VerificationFinished,
        source: Provenance::Host,
        payload: serde_json::json!({"check": "cargo-test", "exit": 0}),
    };
    let plugin_claim = EventEnvelope {
        seq: 12,
        task_id: "task-1".into(),
        run_id: "run-1".into(),
        kind: EventKind::VerificationFinished,
        source: Provenance::Plugin {
            harness_id: "example.harness".into(),
            package_digest: "sha256:deadbeef".into(),
        },
        payload: serde_json::json!({"check": "cargo-test", "passed": true}),
    };
    let host_json = serde_json::to_value(&host_fact).unwrap();
    let plugin_json = serde_json::to_value(&plugin_claim).unwrap();
    assert_eq!(host_json["source"]["source"], "host");
    assert_eq!(plugin_json["source"]["source"], "plugin");
    assert_ne!(host_json["source"], plugin_json["source"]);
}

#[test]
fn approvals_only_reference_host_pending_operations() {
    let request = ApprovalsRequest {
        pending_operation: PendingOperationRef {
            operation_id: "op-123".into(),
            input_hash: "deadbeef".into(),
        },
        summary: "run cargo test".into(),
    };
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["pending_operation"]["operation_id"], "op-123");
    assert_eq!(value["pending_operation"]["inputHash"], "deadbeef");
    // Generic questions carry no grant semantics at all.
    let question = QuestionsAskRequest {
        text: "Which database?".into(),
        options: vec!["sqlite".into(), "postgres".into()],
        blocking: true,
    };
    let value = serde_json::to_value(&question).unwrap();
    assert!(value.get("permissions").is_none());
    assert!(value.get("grant").is_none());
}
