//! P03 — public API v1.1 process-read wire contract.

use r_code_harness_protocol::operations::ReplayClass;
use r_code_harness_protocol::rpc::{is_known_method, PLUGIN_TO_HOST_METHODS};
use r_code_harness_protocol::{
    ApiVersion, HarnessManifestBuilder, HostService, Platform, ProcessOutputFrame,
    ProcessOutputStream, ProcessReadReply, ProcessReadRequest, PROCESS_READ_MAX_BYTES,
    PROCESS_READ_MAX_WAIT_MS,
};
use serde_json::{json, Value};
use std::path::Path;

#[test]
fn rust_wire_shapes_are_strict_camel_case_binary_safe_and_round_trip() {
    let request = ProcessReadRequest {
        handle: "run-1:process-7".into(),
        cursor: 41,
        max_bytes: PROCESS_READ_MAX_BYTES,
        wait_ms: Some(PROCESS_READ_MAX_WAIT_MS),
    };
    let request_json = serde_json::to_value(&request).unwrap();
    assert_eq!(
        request_json,
        json!({
            "handle": "run-1:process-7",
            "cursor": 41,
            "maxBytes": 262144,
            "waitMs": 30000
        })
    );
    assert_eq!(
        serde_json::from_value::<ProcessReadRequest>(request_json).unwrap(),
        request
    );
    for invalid in [
        json!({"handle":"h","cursor":0,"maxBytes":1,"extra":true}),
        json!({"handle":"h","cursor":-1,"maxBytes":1}),
        json!({"handle":"h","cursor":0,"maxBytes":"1"}),
        json!({"cursor":0,"maxBytes":1}),
    ] {
        assert!(serde_json::from_value::<ProcessReadRequest>(invalid).is_err());
    }

    let reply = ProcessReadReply {
        frames: vec![
            ProcessOutputFrame::Data {
                sequence: 41,
                stream: ProcessOutputStream::Stdout,
                data_base64: "AAH+/w==".into(),
            },
            ProcessOutputFrame::Data {
                sequence: 42,
                stream: ProcessOutputStream::Stderr,
                data_base64: "ZXJy".into(),
            },
            ProcessOutputFrame::Eof {
                sequence: 43,
                stream: ProcessOutputStream::Stdout,
            },
            ProcessOutputFrame::Eof {
                sequence: 44,
                stream: ProcessOutputStream::Stderr,
            },
            ProcessOutputFrame::Exit {
                sequence: 45,
                exit_code: None,
            },
        ],
        next_cursor: 46,
        terminal: true,
        exit_code: None,
    };
    let reply_json = serde_json::to_value(&reply).unwrap();
    assert_eq!(reply_json["frames"][0]["kind"], "data");
    assert_eq!(reply_json["frames"][0]["stream"], "stdout");
    assert_eq!(reply_json["frames"][0]["dataBase64"], "AAH+/w==");
    assert_eq!(reply_json["frames"][4]["kind"], "exit");
    assert_eq!(reply_json["frames"][4]["exitCode"], Value::Null);
    assert_eq!(reply_json["nextCursor"], 46);
    assert_eq!(
        serde_json::from_value::<ProcessReadReply>(reply_json).unwrap(),
        reply
    );

    for invalid in [
        json!({"frames":[],"nextCursor":0,"terminal":false,"unknown":1}),
        json!({"frames":[{"kind":"data","sequence":0,"stream":"stdout","dataBase64":"YQ==","x":1}],"nextCursor":1,"terminal":false}),
        json!({"frames":[{"kind":"future","sequence":0}],"nextCursor":1,"terminal":false}),
        json!({"frames":[{"kind":"exit","sequence":0}],"nextCursor":1,"terminal":true}),
    ] {
        assert!(serde_json::from_value::<ProcessReadReply>(invalid).is_err());
    }
}

#[test]
fn rust_method_service_replay_and_schema_surfaces_are_exactly_aligned() {
    assert!(HostService::ALL.contains(&HostService::ProcessRead));
    assert_eq!(HostService::ProcessRead.wire_name(), "host.process.read");
    assert_eq!(
        serde_json::to_string(&HostService::ProcessRead).unwrap(),
        "\"host.process.read\""
    );
    assert!(PLUGIN_TO_HOST_METHODS.contains(&"host.process.read"));
    assert!(is_known_method("host.process.read"));
    assert_eq!(
        ReplayClass::for_method("host.process.read"),
        Some(ReplayClass::Idempotent)
    );
    for method in [
        "host.process.open",
        "host.process.write",
        "host.process.close",
    ] {
        assert_eq!(
            ReplayClass::for_method(method),
            Some(ReplayClass::ProcessEffect)
        );
    }

    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../schema/harness-v1.schema.json")).unwrap();
    let services = schema["properties"]["requestedHostServices"]["items"]["enum"]
        .as_array()
        .unwrap();
    assert!(services.iter().any(|value| value == "host.process.read"));
    let request = &schema["definitions"]["ProcessReadRequest"];
    assert_eq!(request["additionalProperties"], false);
    assert_eq!(request["properties"]["maxBytes"]["minimum"], 1);
    assert_eq!(request["properties"]["maxBytes"]["maximum"], 262144);
    assert_eq!(request["properties"]["waitMs"]["maximum"], 30000);
    let reply = &schema["definitions"]["ProcessReadReply"];
    assert_eq!(reply["additionalProperties"], false);
    assert_eq!(
        reply["properties"]["frames"]["items"]["$ref"],
        "#/definitions/ProcessOutputFrame"
    );
    let frame_variants = schema["definitions"]["ProcessOutputFrame"]["oneOf"]
        .as_array()
        .unwrap();
    assert_eq!(frame_variants.len(), 3);
    assert!(frame_variants
        .iter()
        .all(|variant| variant["additionalProperties"] == false));

    let schema_text = include_str!("../schema/harness-v1.schema.json");
    assert!(!schema_text.contains("codex.event.next"));
}

#[test]
fn api_v1_minor_one_is_additive_and_old_minor_zero_needs_no_process_read() {
    let old = HarnessManifestBuilder::new("old-v1.fixture", "1.0.0")
        .api(1, 0)
        .entrypoint(Platform::current(), "bin/fixture", &[])
        .services(&[HostService::ToolsList, HostService::ProcessOpen])
        .build();
    let negotiated = old
        .negotiate(ApiVersion::new(1, 1), Platform::current(), HostService::ALL)
        .expect("old 1.0 package remains compatible with host 1.1");
    assert_eq!(negotiated.plugin_api, ApiVersion::new(1, 0));
    assert_eq!(negotiated.host_api, ApiVersion::new(1, 1));
    assert!(!negotiated.grants(HostService::ProcessRead));
    assert!(negotiated.grants(HostService::ProcessOpen));
    assert!(ApiVersion::new(1, 0).is_supported_by(&ApiVersion::new(1, 1)));
    assert!(!ApiVersion::new(1, 2).is_supported_by(&ApiVersion::new(1, 1)));
    assert!(!ApiVersion::new(2, 0).is_supported_by(&ApiVersion::new(1, 1)));
}

#[test]
fn package_and_schema_sources_have_no_private_codex_polling_method() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for path in [
        root.join("crates/r-code-harness-protocol/src/rpc.rs"),
        root.join("crates/r-code-harness-protocol/schema/harness-v1.schema.json"),
        root.join("plugins/codex/src/app_server.rs"),
        root.join("plugins/codex/harness.json"),
        root.join("src-tauri/plugins/codex/harness.json"),
    ] {
        let source = std::fs::read_to_string(&path).unwrap();
        assert!(
            !source.contains("codex.event.next"),
            "private polling method remains in {}",
            path.display()
        );
    }
}
