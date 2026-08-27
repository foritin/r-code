use std::collections::BTreeSet;
use std::str::FromStr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use async_trait::async_trait;
use r_code_core::error::ProductError;
use r_code_core::security::PathGuard;
use r_code_gateway::{PermissionEngine, ToolExecutionContext, ToolGateway};
use r_code_host::browser::{
    browser_agent_contract, codex_dynamic_browser_tools, register_browser_agent_tools,
    BrowserActionMetadata, BrowserElementValue, BrowserEventEnvelope, BrowserOrigin,
    BrowserPermissionCapability, BrowserPermissionGrant, BrowserPermissionScope,
    BrowserRuntimeManifest, BrowserSession, BrowserTab, BrowserTimeoutMs, BrowserToolExecutor,
    BrowserToolName, BrowserToolRequest, BrowserToolResult, BrowserWorkspacePath,
    BROWSER_CONTRACT_SCHEMA_VERSION, MAX_BROWSER_TIMEOUT_MS,
};
use r_code_host::feature_flags::{FeatureFlagService, ProductFeatureFlags};
use serde::Deserialize;
use serde_json::{json, Value};

const CONTRACT_FIXTURE: &str = include_str!("../../fixtures/browser/contract-v1.json");

#[derive(Debug, Deserialize)]
struct ContractFixture {
    schema_version: u32,
    tool_names: Vec<String>,
    runtime_manifest: BrowserRuntimeManifest,
    session: BrowserSession,
    tab: BrowserTab,
    permission_grants: Vec<BrowserPermissionGrant>,
    tool_requests: Vec<Value>,
    tool_results: Vec<Value>,
    events: Vec<BrowserEventEnvelope>,
}

fn expected_tool_names() -> Vec<String> {
    BrowserToolName::ALL
        .into_iter()
        .map(|name| name.as_str().to_owned())
        .collect()
}

fn action_metadata(result: &BrowserToolResult) -> &BrowserActionMetadata {
    match result {
        BrowserToolResult::Open(result) => &result.action,
        BrowserToolResult::Navigate(result) => &result.action,
        BrowserToolResult::Snapshot(result) => &result.action,
        BrowserToolResult::Screenshot(result) => &result.action,
        BrowserToolResult::Click(result)
        | BrowserToolResult::Type(result)
        | BrowserToolResult::Select(result)
        | BrowserToolResult::Press(result)
        | BrowserToolResult::Scroll(result) => &result.action,
        BrowserToolResult::Wait(result) => &result.action,
        BrowserToolResult::Tabs(result) => &result.action,
        BrowserToolResult::Console(result) => &result.action,
        BrowserToolResult::NetworkErrors(result) => &result.action,
        BrowserToolResult::Close(result) => &result.action,
    }
}

fn parse_fixture() -> (Value, ContractFixture) {
    let raw = serde_json::from_str(CONTRACT_FIXTURE).expect("parse raw Browser fixture JSON");
    let fixture =
        serde_json::from_str(CONTRACT_FIXTURE).expect("deserialize Browser contract fixture");
    (raw, fixture)
}

#[test]
fn public_fixture_round_trips_through_rust_dtos() {
    let (raw, fixture) = parse_fixture();

    assert_eq!(fixture.schema_version, BROWSER_CONTRACT_SCHEMA_VERSION);
    assert_eq!(
        fixture.runtime_manifest.schema_version,
        BROWSER_CONTRACT_SCHEMA_VERSION
    );
    assert_eq!(
        serde_json::to_value(&fixture.runtime_manifest).expect("serialize runtime manifest"),
        raw["runtime_manifest"]
    );
    assert_eq!(
        serde_json::to_value(&fixture.session).expect("serialize Browser session"),
        raw["session"]
    );
    assert_eq!(
        serde_json::to_value(&fixture.tab).expect("serialize Browser tab"),
        raw["tab"]
    );
    assert_eq!(
        serde_json::to_value(&fixture.permission_grants).expect("serialize permission grants"),
        raw["permission_grants"]
    );
    assert_eq!(
        serde_json::to_value(&fixture.events).expect("serialize Browser events"),
        raw["events"]
    );
}

#[test]
fn fixture_and_rust_publish_exactly_fourteen_unique_tool_names() {
    let (_, fixture) = parse_fixture();
    let expected = expected_tool_names();

    assert_eq!(expected.len(), 14);
    assert_eq!(fixture.tool_names, expected);
    assert_eq!(
        fixture.tool_names.iter().collect::<BTreeSet<_>>().len(),
        fixture.tool_names.len(),
        "Browser tool names must remain unique"
    );
    for name in &fixture.tool_names {
        assert_eq!(
            BrowserToolName::from_str(name)
                .unwrap_or_else(|_| panic!("fixture contains unknown Browser tool {name}"))
                .as_str(),
            name
        );
    }
}

#[test]
fn every_fixture_request_decodes_via_the_single_rust_dispatch_contract() {
    let (_, fixture) = parse_fixture();
    let mut decoded_names = Vec::new();

    for raw_request in &fixture.tool_requests {
        let name_text = raw_request["tool"]
            .as_str()
            .expect("fixture request tool must be a string");
        let name = BrowserToolName::from_str(name_text)
            .unwrap_or_else(|_| panic!("fixture request uses unknown tool {name_text}"));
        let request = BrowserToolRequest::from_input(name, raw_request["input"].clone())
            .unwrap_or_else(|error| panic!("decode {name_text} request: {error}"));

        assert_eq!(request.tool_name(), name);
        assert_eq!(
            serde_json::to_value(&request).expect("serialize Browser request"),
            *raw_request,
            "{name_text} request changed its wire shape"
        );
        decoded_names.push(name_text.to_owned());
    }

    assert_eq!(decoded_names, expected_tool_names());
}

#[test]
fn fixture_results_round_trip_and_always_carry_action_metadata() {
    let (_, fixture) = parse_fixture();

    for raw_result in &fixture.tool_results {
        let output = raw_result["output"]
            .as_object()
            .expect("fixture result output must be an object");
        for field in ["session_id", "tab_id", "url", "action_id", "timestamp"] {
            assert!(
                output.contains_key(field),
                "{} result must contain action metadata field {field}",
                raw_result["tool"]
            );
        }

        let result: BrowserToolResult = serde_json::from_value(raw_result.clone())
            .unwrap_or_else(|error| panic!("decode Browser result {raw_result}: {error}"));
        assert_eq!(
            result.tool_name().as_str(),
            raw_result["tool"].as_str().expect("result tool string")
        );
        assert_eq!(
            serde_json::to_value(&result).expect("serialize Browser result"),
            *raw_result,
            "{} result changed its wire shape",
            result.tool_name().as_str()
        );

        let action = action_metadata(&result);
        assert!(!action.session_id.is_empty());
        assert!(!action.action_id.is_empty());
        assert_eq!(
            serde_json::to_value(action).expect("serialize action metadata")["timestamp"],
            raw_result["output"]["timestamp"]
        );
    }
}

#[test]
fn fixture_proves_snapshot_console_and_network_redaction() {
    let (_, fixture) = parse_fixture();
    let results = fixture
        .tool_results
        .iter()
        .map(|value| {
            serde_json::from_value::<BrowserToolResult>(value.clone())
                .expect("decode Browser result for redaction checks")
        })
        .collect::<Vec<_>>();

    let snapshot = results
        .iter()
        .find_map(|result| match result {
            BrowserToolResult::Snapshot(result) => Some(result),
            _ => None,
        })
        .expect("fixture must include a snapshot result with a sensitive element");
    let password = snapshot
        .snapshot
        .elements
        .iter()
        .find(|element| element.name.eq_ignore_ascii_case("password"))
        .expect("snapshot fixture must identify the password element");
    assert!(matches!(password.value, BrowserElementValue::Redacted));
    assert_eq!(
        serde_json::to_value(&password.value).expect("serialize redacted element value"),
        json!({ "state": "redacted" })
    );

    let console = results
        .iter()
        .find_map(|result| match result {
            BrowserToolResult::Console(result) => Some(result),
            _ => None,
        })
        .expect("fixture must include a console result proving secret redaction");
    assert!(
        !console.entries.is_empty(),
        "console redaction fixture must contain at least one entry"
    );
    assert!(
        console.entries.iter().all(|entry| entry.redacted),
        "every console fixture entry must be explicitly redacted"
    );

    let network = results
        .iter()
        .find_map(|result| match result {
            BrowserToolResult::NetworkErrors(result) => Some(result),
            _ => None,
        })
        .expect("fixture must include a network-errors result proving secret redaction");
    assert!(
        !network.errors.is_empty(),
        "network redaction fixture must contain at least one error"
    );
    assert!(
        network.errors.iter().all(|error| error.redacted),
        "every network fixture entry must be explicitly redacted"
    );
}

#[test]
fn exact_origins_are_normalized_and_reject_credentials() {
    let https = BrowserOrigin::parse("HTTPS://EXAMPLE.COM/path?query=ignored")
        .expect("parse normalized HTTPS origin");
    assert_eq!(https.scheme, "https");
    assert_eq!(https.host, "example.com");
    assert_eq!(https.effective_port, 443);
    assert_eq!(https.as_string(), "https://example.com:443");
    assert_eq!(
        https,
        BrowserOrigin::parse("https://example.com:443/another/path#fragment")
            .expect("path and query must not change an exact origin")
    );

    assert_eq!(
        BrowserOrigin::parse("http://example.com")
            .expect("parse default HTTP port")
            .effective_port,
        80
    );
    assert_eq!(
        BrowserOrigin::parse("https://example.com:8443")
            .expect("parse custom HTTPS port")
            .effective_port,
        8443
    );
    assert!(BrowserOrigin::parse("https://user:password@example.com").is_err());
    assert!(BrowserOrigin::parse("file:///workspace/index.html").is_err());
}

#[test]
fn workspace_file_paths_are_portable_relative_and_fail_closed() {
    let valid = BrowserWorkspacePath::parse("docs/index.html")
        .expect("portable task-relative path should be accepted");
    assert_eq!(valid.as_str(), "docs/index.html");

    for invalid in [
        "",
        "/absolute/path",
        "../escape",
        "a/../escape",
        "a/./file",
        r"a\b",
        "C:/escape",
        "a//b",
        ".",
        "..",
        "a:\\b",
        "a\0b",
    ] {
        assert!(
            BrowserWorkspacePath::parse(invalid).is_err(),
            "unsafe workspace path should be rejected: {invalid:?}"
        );
    }
}

#[test]
fn timeout_contract_accepts_only_one_through_thirty_seconds() {
    assert_eq!(MAX_BROWSER_TIMEOUT_MS, 30_000);
    assert_eq!(BrowserTimeoutMs::new(1).expect("minimum timeout").get(), 1);
    assert_eq!(
        BrowserTimeoutMs::new(MAX_BROWSER_TIMEOUT_MS)
            .expect("maximum timeout")
            .get(),
        MAX_BROWSER_TIMEOUT_MS
    );
    assert!(BrowserTimeoutMs::new(0).is_err());
    assert!(BrowserTimeoutMs::new(MAX_BROWSER_TIMEOUT_MS + 1).is_err());

    let default_wait = BrowserToolRequest::from_input(
        BrowserToolName::Wait,
        json!({
            "session_id": "browser-session-1",
            "condition": { "kind": "selector", "selector": "#ready" }
        }),
    )
    .expect("wait should use the bounded default timeout");
    let BrowserToolRequest::Wait(default_wait) = default_wait else {
        panic!("wait input must decode to a wait request");
    };
    assert_eq!(default_wait.timeout_ms.get(), 10_000);

    for invalid in [0, MAX_BROWSER_TIMEOUT_MS + 1] {
        assert!(
            BrowserToolRequest::from_input(
                BrowserToolName::Wait,
                json!({
                    "session_id": "browser-session-1",
                    "condition": { "kind": "url", "url": "https://example.com" },
                    "timeout_ms": invalid
                }),
            )
            .is_err(),
            "wait timeout {invalid} must fail closed"
        );
    }
}

#[test]
fn permission_capability_and_scope_names_cover_all_four_combinations() {
    let capabilities = [
        (BrowserPermissionCapability::Browse, "browse"),
        (BrowserPermissionCapability::Interact, "interact"),
    ];
    let scopes = [
        (BrowserPermissionScope::Once, "once"),
        (BrowserPermissionScope::Task, "task"),
    ];
    let mut combinations = Vec::new();

    for (capability, capability_name) in capabilities {
        let wire = serde_json::to_value(capability).expect("serialize permission capability");
        assert_eq!(wire, json!(capability_name));
        assert_eq!(
            serde_json::from_value::<BrowserPermissionCapability>(wire)
                .expect("deserialize permission capability"),
            capability
        );
        for (scope, scope_name) in scopes {
            combinations.push((capability_name, scope_name));
            let wire = serde_json::to_value(scope).expect("serialize permission scope");
            assert_eq!(wire, json!(scope_name));
            assert_eq!(
                serde_json::from_value::<BrowserPermissionScope>(wire)
                    .expect("deserialize permission scope"),
                scope
            );
        }
    }

    assert_eq!(
        combinations,
        vec![
            ("browse", "once"),
            ("browse", "task"),
            ("interact", "once"),
            ("interact", "task"),
        ]
    );
}

struct NeverExecuteBrowser;

#[async_trait]
impl BrowserToolExecutor for NeverExecuteBrowser {
    async fn execute(
        &self,
        _request: BrowserToolRequest,
        _context: &ToolExecutionContext,
        _workspace_guard: &PathGuard,
        _abort_flag: Option<&AtomicBool>,
    ) -> Result<BrowserToolResult, ProductError> {
        panic!("contract registration tests must never execute Browser tools")
    }
}

fn empty_gateway() -> ToolGateway {
    ToolGateway::new(Arc::new(PermissionEngine::new()))
}

#[test]
fn browser_is_disabled_server_side_and_registers_no_placeholder_tools() {
    let config_dir = tempfile::tempdir().expect("create temporary feature config directory");
    let error = browser_agent_contract(config_dir.path())
        .expect_err("Browser contract command must be disabled by default");
    assert_eq!(error.code, "browser.feature_disabled");

    let mut gateway = empty_gateway();
    assert!(codex_dynamic_browser_tools(&gateway).is_empty());
    let error = register_browser_agent_tools(
        ProductFeatureFlags::default(),
        &mut gateway,
        Arc::new(NeverExecuteBrowser),
    )
    .expect_err("disabled Browser feature must reject backend registration");
    assert_eq!(error.code, "browser.feature_disabled");
    assert!(gateway.tool_specs().is_empty());
    assert!(codex_dynamic_browser_tools(&gateway).is_empty());
}

#[test]
fn enabled_native_and_codex_agents_project_the_same_gateway_contract() {
    let flags = ProductFeatureFlags {
        browser_enabled: true,
        automation_enabled: false,
    };
    let config_dir = tempfile::tempdir().expect("create temporary feature config directory");
    FeatureFlagService::new(config_dir.path().to_path_buf())
        .save(flags)
        .expect("enable Browser in the temporary feature config");
    let agent_contract =
        browser_agent_contract(config_dir.path()).expect("load enabled Browser agent contract");
    assert_eq!(
        agent_contract
            .tools
            .iter()
            .map(|tool| tool.name.as_str().to_owned())
            .collect::<Vec<_>>(),
        expected_tool_names()
    );

    let mut gateway = empty_gateway();
    register_browser_agent_tools(flags, &mut gateway, Arc::new(NeverExecuteBrowser))
        .expect("register enabled Browser contract");

    let mut native_names = gateway
        .tool_specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    let mut expected_sorted = expected_tool_names();
    expected_sorted.sort();
    assert_eq!(native_names.len(), 14);
    assert_eq!(native_names, expected_sorted);

    let codex_tools = codex_dynamic_browser_tools(&gateway);
    let codex_names = codex_tools
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("Codex Browser descriptor name")
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(codex_names, expected_tool_names());

    native_names.sort();
    let mut codex_sorted = codex_names;
    codex_sorted.sort();
    assert_eq!(native_names, codex_sorted);
    for (descriptor, contract) in codex_tools.iter().zip(agent_contract.tools.iter()) {
        assert_eq!(descriptor["inputSchema"], contract.input_schema);
        assert_eq!(descriptor["description"], contract.description);
    }
}
