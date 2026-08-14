use std::collections::BTreeMap;

use r_code_mcp::{
    decode_tool_name, encode_tool_name, BuiltinMcpServer, McpInstallPlan, McpServerConfig,
    McpServerSource, McpTransportConfig, SecretRef,
};

fn user_stdio_config() -> McpServerConfig {
    McpServerConfig {
        id: "local-tools".to_string(),
        display_name: "Local tools".to_string(),
        description: "A local MCP server".to_string(),
        enabled: true,
        source: McpServerSource::User,
        transport: McpTransportConfig::Stdio {
            command: "C:\\Program Files\\nodejs\\npx.cmd".to_string(),
            args: vec!["--yes".to_string(), "@example/mcp server".to_string()],
            env: BTreeMap::from([(
                "API_TOKEN".to_string(),
                SecretRef::new("credential:mcp/local-tools/api-token").unwrap(),
            )]),
        },
        approved_launch_fingerprint: None,
    }
}

#[test]
fn configuration_round_trip_contains_only_secret_references() {
    let config = user_stdio_config();
    let json = serde_json::to_string_pretty(&config).unwrap();

    assert!(json.contains("credential:mcp/local-tools/api-token"));
    assert!(!json.contains("super-secret-value"));

    let restored: McpServerConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, config);
    restored.validate().unwrap();
}

#[test]
fn transport_rejects_unknown_shell_field() {
    let json = r#"{
        "type": "stdio",
        "command": "node",
        "args": ["server.js"],
        "shell": true
    }"#;

    assert!(serde_json::from_str::<McpTransportConfig>(json).is_err());
}

#[test]
fn server_ids_are_stable_and_cannot_shadow_native_tools() {
    for id in [
        "",
        "Uppercase",
        "1starts-with-number",
        "has__separator",
        "web_search",
        "mcp_save_draft",
    ] {
        let mut config = user_stdio_config();
        config.id = id.to_string();
        assert!(config.validate().is_err(), "id should be rejected: {id}");
    }
}

#[test]
fn http_transport_allows_only_explicit_loopback_cleartext_without_credentials() {
    let insecure = McpTransportConfig::StreamableHttp {
        url: "http://example.com/mcp".to_string(),
        headers: BTreeMap::new(),
    };
    assert!(insecure.validate().is_err());

    for url in [
        "http://127.0.0.1:27200/mcp",
        "http://localhost:27200/mcp",
        "http://[::1]:27200/mcp",
    ] {
        McpTransportConfig::StreamableHttp {
            url: url.to_string(),
            headers: BTreeMap::new(),
        }
        .validate()
        .unwrap_or_else(|error| panic!("explicit loopback URL {url} should be allowed: {error}"));
    }

    for url in [
        "http://127.0.0.2:27200/mcp",
        "http://127.1:27200/mcp",
        "http://2130706433:27200/mcp",
        "http://localhost.example:27200/mcp",
        "http://localhost.:27200/mcp",
        "http://[::ffff:127.0.0.1]:27200/mcp",
        "http://user@localhost:27200/mcp",
        "http://localhost:27200@mcp.example/mcp",
    ] {
        assert!(
            McpTransportConfig::StreamableHttp {
                url: url.to_string(),
                headers: BTreeMap::new(),
            }
            .validate()
            .is_err(),
            "non-canonical or non-loopback cleartext URL should be rejected: {url}"
        );
    }

    let embedded = McpTransportConfig::StreamableHttp {
        url: "https://user:password@example.com/mcp".to_string(),
        headers: BTreeMap::new(),
    };
    assert!(embedded.validate().is_err());

    let valid = McpTransportConfig::StreamableHttp {
        url: "https://example.com/mcp".to_string(),
        headers: BTreeMap::new(),
    };
    valid.validate().unwrap();
}

#[test]
fn stdio_is_argument_safe_and_rejects_control_characters() {
    user_stdio_config().validate().unwrap();

    let invalid = McpTransportConfig::Stdio {
        command: "node\nmalicious".to_string(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };
    assert!(invalid.validate().is_err());
}

#[test]
fn namespaced_tool_names_round_trip() {
    let encoded = encode_tool_name("github", "search_repositories").unwrap();
    assert_eq!(encoded, "github__search_repositories");
    assert_eq!(
        decode_tool_name(&encoded).unwrap(),
        ("github", "search_repositories")
    );
    assert!(encode_tool_name("web_search", "anything").is_err());
    assert!(encode_tool_name("github", "bad__tool").is_err());
}

#[test]
fn registry_install_plan_is_inert_until_user_enables_it() {
    let plan = McpInstallPlan {
        server_id: "registry-server".to_string(),
        display_name: "Registry server".to_string(),
        description: "Registry fixture".to_string(),
        source: McpServerSource::Registry {
            registry_url: "https://registry.modelcontextprotocol.io".to_string(),
            name: "io.example/server".to_string(),
            version: "1.2.3".to_string(),
            repository_url: Some("https://github.com/example/server".to_string()),
        },
        transport: McpTransportConfig::Stdio {
            command: "npx".to_string(),
            args: vec!["--yes".to_string(), "@example/server@1.2.3".to_string()],
            env: BTreeMap::new(),
        },
        required_secret_names: Vec::new(),
    };

    let config = plan.into_disabled_config().unwrap();
    assert!(!config.enabled);
    assert!(config.approved_launch_fingerprint.is_none());
}

#[test]
fn builtin_configuration_has_no_launch_fingerprint() {
    let transport = McpTransportConfig::Builtin {
        server: BuiltinMcpServer::Research,
    };
    assert!(transport.launch_fingerprint_material().is_none());
}
