use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use hermes_core::ToolHost;
use r_code_mcp::{
    is_blocked_ip, DnsResolver, WebClient, WebError, WebHttpAdapter, WebHttpRequest,
    WebHttpResponse, WebLimits, WebSearchConfiguration, WebSearchProvider, WebToolHost,
};
use serde_json::json;

struct FakeDns {
    addresses: HashMap<String, Vec<IpAddr>>,
}

#[async_trait]
impl DnsResolver for FakeDns {
    async fn resolve(&self, host: &str, _port: u16) -> Result<Vec<IpAddr>, WebError> {
        self.addresses.get(host).cloned().ok_or(WebError::DnsLookup)
    }
}

#[derive(Default)]
struct FakeHttp {
    requests: Mutex<Vec<WebHttpRequest>>,
    responses: Mutex<VecDeque<Result<WebHttpResponse, WebError>>>,
}

impl FakeHttp {
    fn push(&self, response: WebHttpResponse) {
        self.responses.lock().unwrap().push_back(Ok(response));
    }

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

#[async_trait]
impl WebHttpAdapter for FakeHttp {
    async fn get(&self, request: WebHttpRequest) -> Result<WebHttpResponse, WebError> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(WebError::RequestFailed))
    }
}

fn response(status: u16, content_type: &str, body: &str) -> WebHttpResponse {
    WebHttpResponse {
        status,
        headers: BTreeMap::from([("content-type".to_string(), content_type.to_string())]),
        body: body.as_bytes().to_vec(),
        truncated: false,
    }
}

fn client(
    search: WebSearchConfiguration,
    dns: HashMap<String, Vec<IpAddr>>,
    http: Arc<FakeHttp>,
) -> WebClient {
    WebClient::with_adapters(
        search,
        WebLimits {
            timeout_ms: 500,
            max_bytes: 4_096,
            max_chars: 2_000,
            max_redirects: 2,
            max_results: 5,
        },
        Arc::new(FakeDns { addresses: dns }),
        http,
    )
}

fn public_dns(hosts: &[&str]) -> HashMap<String, Vec<IpAddr>> {
    hosts
        .iter()
        .map(|host| {
            (
                (*host).to_string(),
                vec![IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))],
            )
        })
        .collect()
}

#[test]
fn private_metadata_and_documentation_ranges_are_blocked() {
    for address in [
        "0.0.0.0",
        "10.0.0.1",
        "100.64.0.1",
        "127.0.0.1",
        "169.254.169.254",
        "172.16.0.1",
        "192.168.0.1",
        "192.0.2.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "::",
        "::1",
        "fc00::1",
        "fe80::1",
        "2001:db8::1",
        "::ffff:127.0.0.1",
        "::127.0.0.1",
        "::10.0.0.1",
        "::169.254.169.254",
        "::192.0.2.1",
    ] {
        assert!(
            is_blocked_ip(address.parse().unwrap()),
            "address should be blocked: {address}"
        );
    }
    assert!(!is_blocked_ip("93.184.216.34".parse().unwrap()));
    assert!(!is_blocked_ip(
        "2606:2800:220:1:248:1893:25c8:1946".parse().unwrap()
    ));
    assert!(!is_blocked_ip("::93.184.216.34".parse().unwrap()));
}

#[tokio::test]
async fn private_dns_answer_prevents_any_request() {
    let http = Arc::new(FakeHttp::default());
    let client = client(
        WebSearchConfiguration::jina(None),
        HashMap::from([(
            "example.test".to_string(),
            vec![
                "93.184.216.34".parse().unwrap(),
                "127.0.0.1".parse().unwrap(),
            ],
        )]),
        http.clone(),
    );

    assert!(matches!(
        client.fetch("https://example.test/", None).await,
        Err(WebError::BlockedAddress)
    ));
    assert_eq!(http.request_count(), 0);
}

#[tokio::test]
async fn ipv4_compatible_private_dns_answer_prevents_any_request() {
    let http = Arc::new(FakeHttp::default());
    let client = client(
        WebSearchConfiguration::jina(None),
        HashMap::from([(
            "example.test".to_string(),
            vec!["::127.0.0.1".parse().unwrap()],
        )]),
        http.clone(),
    );

    assert!(matches!(
        client.fetch("https://example.test/", None).await,
        Err(WebError::BlockedAddress)
    ));
    assert_eq!(http.request_count(), 0);
}

#[tokio::test]
async fn redirect_target_is_resolved_and_revalidated() {
    let http = Arc::new(FakeHttp::default());
    http.push(WebHttpResponse {
        status: 302,
        headers: BTreeMap::from([(
            "location".to_string(),
            "http://metadata.test/latest".to_string(),
        )]),
        body: Vec::new(),
        truncated: false,
    });
    let client = client(
        WebSearchConfiguration::jina(None),
        HashMap::from([
            (
                "public.test".to_string(),
                vec!["93.184.216.34".parse().unwrap()],
            ),
            (
                "metadata.test".to_string(),
                vec!["169.254.169.254".parse().unwrap()],
            ),
        ]),
        http.clone(),
    );

    assert!(matches!(
        client.fetch("https://public.test/start", None).await,
        Err(WebError::BlockedAddress)
    ));
    assert_eq!(http.request_count(), 1);
}

#[tokio::test]
async fn fetch_converts_html_and_applies_character_limit() {
    let http = Arc::new(FakeHttp::default());
    http.push(response(
        200,
        "text/html; charset=utf-8",
        "<html><head><title>Example title</title></head><body><h1>Hello</h1><p>bounded text</p></body></html>",
    ));
    let client = client(
        WebSearchConfiguration::jina(None),
        public_dns(&["example.test"]),
        http,
    );

    let fetched = client
        .fetch("https://example.test/article", Some(10))
        .await
        .unwrap();

    assert_eq!(fetched.title.as_deref(), Some("Example title"));
    assert!(fetched.content.chars().count() <= 10);
    assert!(fetched.truncated);
    assert!(!fetched.retrieved_at.is_empty());
}

#[tokio::test]
async fn binary_mime_is_rejected() {
    let http = Arc::new(FakeHttp::default());
    http.push(response(200, "application/octet-stream", "binary"));
    let client = client(
        WebSearchConfiguration::jina(None),
        public_dns(&["example.test"]),
        http,
    );

    assert!(matches!(
        client.fetch("https://example.test/file", None).await,
        Err(WebError::UnsupportedMime(_))
    ));
}

#[tokio::test]
async fn jina_search_is_keyless_and_returns_bounded_sources() {
    let http = Arc::new(FakeHttp::default());
    http.push(response(
        200,
        "application/json",
        r#"{"data":[{"title":"One","url":"https://one.test/a","description":"first"},{"title":"Two","url":"https://two.test/b","description":"second"}]}"#,
    ));
    let client = client(
        WebSearchConfiguration::jina(None),
        public_dns(&["s.jina.ai"]),
        http.clone(),
    );

    let found = client.search("safe search", Some(1)).await.unwrap();

    assert_eq!(found.provider, WebSearchProvider::Jina);
    assert_eq!(found.sources.len(), 1);
    assert_eq!(found.sources[0].url, "https://one.test/a");
    let requests = http.requests.lock().unwrap();
    assert!(!requests[0].headers.contains_key("authorization"));
    assert_eq!(requests[0].approved_addresses.len(), 1);
}

#[tokio::test]
async fn brave_credential_is_used_only_as_a_header() {
    let http = Arc::new(FakeHttp::default());
    http.push(response(
        200,
        "application/json",
        r#"{"web":{"results":[]}}"#,
    ));
    let client = client(
        WebSearchConfiguration::brave("sentinel-brave-key".to_string()),
        public_dns(&["api.search.brave.com"]),
        http.clone(),
    );

    client.search("query", Some(2)).await.unwrap();

    let requests = http.requests.lock().unwrap();
    assert_eq!(
        requests[0]
            .headers
            .get("x-subscription-token")
            .map(String::as_str),
        Some("sentinel-brave-key")
    );
    assert!(!requests[0].url.as_str().contains("sentinel-brave-key"));
}

#[tokio::test]
async fn search_credentials_are_stripped_on_cross_origin_redirects() {
    let http = Arc::new(FakeHttp::default());
    http.push(WebHttpResponse {
        status: 302,
        headers: BTreeMap::from([(
            "location".to_string(),
            "https://redirect.test/result".to_string(),
        )]),
        body: Vec::new(),
        truncated: false,
    });
    http.push(response(
        200,
        "application/json",
        r#"{"web":{"results":[]}}"#,
    ));
    let client = client(
        WebSearchConfiguration::brave("sentinel-brave-key".to_string()),
        public_dns(&["api.search.brave.com", "redirect.test"]),
        http.clone(),
    );

    client.search("query", Some(2)).await.unwrap();

    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].headers.contains_key("x-subscription-token"));
    assert!(!requests[1].headers.contains_key("x-subscription-token"));
    assert_eq!(
        requests[1].headers.get("accept").map(String::as_str),
        Some("application/json")
    );
}

#[tokio::test]
async fn global_tool_host_exposes_fixed_native_tools_and_normal_errors() {
    let http = Arc::new(FakeHttp::default());
    let client = Arc::new(client(
        WebSearchConfiguration::jina(None),
        HashMap::new(),
        http,
    ));
    let host = WebToolHost::new(client);

    let tools = host.list_tools().await.unwrap();
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec!["web_search", "web_fetch"]
    );
    let outcome = host
        .call("web_fetch", json!({"url": "file:///etc/passwd"}))
        .await
        .unwrap();
    assert!(outcome.is_error);
    assert_eq!(outcome.metadata.unwrap()["error_code"], "unsafe_url");
}
