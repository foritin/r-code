use std::{
    collections::{BTreeMap, VecDeque},
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use r_code_mcp::{
    BuiltinMcpServer, DnsResolver, EmptySecretResolver, McpServerConfig, McpServerSource,
    McpServerState, McpSupervisor, McpTransportConfig, RmcpConnector, WebClient, WebError,
    WebHttpAdapter, WebHttpRequest, WebHttpResponse, WebLimits, WebSearchConfiguration,
};
use serde_json::json;

struct PublicDns;

#[async_trait]
impl DnsResolver for PublicDns {
    async fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<IpAddr>, WebError> {
        Ok(vec![IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))])
    }
}

#[derive(Default)]
struct QueueHttp {
    responses: Mutex<VecDeque<WebHttpResponse>>,
}

impl QueueHttp {
    fn push_search(&self, url: &str, title: &str) {
        self.responses.lock().unwrap().push_back(WebHttpResponse {
            status: 200,
            headers: BTreeMap::from([("content-type".to_string(), "application/json".to_string())]),
            body: json!({
                "data": [{"title": title, "url": url, "description": "evidence"}]
            })
            .to_string()
            .into_bytes(),
            truncated: false,
        });
    }
}

#[async_trait]
impl WebHttpAdapter for QueueHttp {
    async fn get(&self, _request: WebHttpRequest) -> Result<WebHttpResponse, WebError> {
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(WebError::RequestFailed)
    }
}

fn built_in(enabled: bool) -> McpServerConfig {
    McpServerConfig {
        id: "r-code-research".to_string(),
        display_name: "R-Code Research".to_string(),
        description: String::new(),
        enabled,
        source: McpServerSource::Builtin,
        transport: McpTransportConfig::Builtin {
            server: BuiltinMcpServer::Research,
        },
        approved_launch_fingerprint: None,
    }
}

fn supervisor(http: Arc<QueueHttp>, enabled: bool) -> McpSupervisor {
    let web = Arc::new(WebClient::with_adapters(
        WebSearchConfiguration::jina(None),
        WebLimits::default(),
        Arc::new(PublicDns),
        http,
    ));
    let connector = Arc::new(RmcpConnector::new(Arc::new(EmptySecretResolver)).with_research(web));
    McpSupervisor::new(connector, vec![built_in(enabled)]).unwrap()
}

#[tokio::test]
async fn in_process_server_initializes_lists_and_calls_tools() {
    let http = Arc::new(QueueHttp::default());
    http.push_search("https://source.test/a", "Source A");
    let supervisor = supervisor(http, true);

    let tools = supervisor.list_tools("r-code-research").await.unwrap();
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec!["search_sources", "fetch_source", "deep_research"]
    );
    assert!(tools.iter().all(|tool| tool.read_only));

    let outcome = supervisor
        .call_tool(
            "r-code-research",
            "search_sources",
            json!({"query": "test", "limit": 3}),
        )
        .await
        .unwrap();
    assert!(!outcome.is_error);
    assert!(outcome.content.contains("https://source.test/a"));
    assert_eq!(
        supervisor.status_snapshot()[0].state,
        McpServerState::Running
    );

    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn deep_research_returns_deduplicated_evidence_without_a_conclusion() {
    let http = Arc::new(QueueHttp::default());
    http.push_search("https://source.test/shared", "Shared");
    http.push_search("https://source.test/shared", "Shared again");
    let supervisor = supervisor(http, true);

    let outcome = supervisor
        .call_tool(
            "r-code-research",
            "deep_research",
            json!({"queries": ["first", "second"], "max_sources": 12}),
        )
        .await
        .unwrap();

    assert!(!outcome.is_error);
    assert_eq!(
        outcome
            .content
            .matches("https://source.test/shared")
            .count(),
        1
    );
    assert!(outcome.content.contains("not_performed"));
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn user_disabled_builtin_never_connects_and_can_be_toggled_live() {
    let http = Arc::new(QueueHttp::default());
    let supervisor = supervisor(http, false);

    let outcome = supervisor
        .call_tool("r-code-research", "search_sources", json!({"query": "x"}))
        .await
        .unwrap();
    assert!(outcome.is_error);
    assert_eq!(outcome.metadata.unwrap()["reason"], "disabled");
    assert_eq!(
        supervisor.status_snapshot()[0].state,
        McpServerState::Disabled
    );
}
