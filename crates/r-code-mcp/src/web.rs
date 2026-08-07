use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use chrono::Utc;
use futures::{future::join_all, StreamExt};
use hermes_core::{ToolCallOutcome, ToolHost, ToolSource, ToolSpec};
use serde_json::{json, Value};
use thiserror::Error;
use url::Url;

use crate::{WebFetchResult, WebLimits, WebSearchProvider, WebSearchResult, WebSource};

const USER_AGENT: &str = concat!(
    "R-Code/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com)"
);

#[derive(Debug, Error)]
pub enum WebError {
    #[error("URL must use http or https")]
    UnsupportedScheme,
    #[error("URL must not contain embedded credentials")]
    EmbeddedCredentials,
    #[error("URL has no valid host")]
    MissingHost,
    #[error("URL resolves to a blocked network address")]
    BlockedAddress,
    #[error("DNS lookup failed")]
    DnsLookup,
    #[error("web request timed out")]
    Timeout,
    #[error("web request failed")]
    RequestFailed,
    #[error("web response exceeded the redirect limit")]
    RedirectLimit,
    #[error("web redirect is missing a valid location")]
    InvalidRedirect,
    #[error("web service returned HTTP {0}")]
    HttpStatus(u16),
    #[error("web response MIME type is not supported: {0}")]
    UnsupportedMime(String),
    #[error("web response could not be decoded")]
    Decode,
    #[error("search query must contain 1 to 500 characters")]
    InvalidQuery,
    #[error("search provider credential is not configured")]
    MissingSearchCredential,
}

#[derive(Clone)]
pub struct WebHttpRequest {
    pub url: Url,
    pub headers: BTreeMap<String, String>,
    pub approved_addresses: Vec<IpAddr>,
    pub timeout: Duration,
    pub max_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct WebHttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub truncated: bool,
}

#[async_trait]
pub trait DnsResolver: Send + Sync {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, WebError>;
}

#[derive(Default)]
pub struct SystemDnsResolver;

#[async_trait]
impl DnsResolver for SystemDnsResolver {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, WebError> {
        let addresses = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| WebError::DnsLookup)?;
        let mut result = addresses.map(|address| address.ip()).collect::<Vec<_>>();
        result.sort();
        result.dedup();
        if result.is_empty() {
            return Err(WebError::DnsLookup);
        }
        Ok(result)
    }
}

#[async_trait]
pub trait WebHttpAdapter: Send + Sync {
    async fn get(&self, request: WebHttpRequest) -> Result<WebHttpResponse, WebError>;
}

#[derive(Default)]
pub struct ReqwestWebHttpAdapter;

#[async_trait]
impl WebHttpAdapter for ReqwestWebHttpAdapter {
    async fn get(&self, request: WebHttpRequest) -> Result<WebHttpResponse, WebError> {
        let host = request.url.host_str().ok_or(WebError::MissingHost)?;
        let port = request
            .url
            .port_or_known_default()
            .ok_or(WebError::MissingHost)?;
        let sockets = request
            .approved_addresses
            .iter()
            .copied()
            .map(|address| SocketAddr::new(address, port))
            .collect::<Vec<_>>();
        let client = reqwest::Client::builder()
            .timeout(request.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .pool_max_idle_per_host(0)
            .resolve_to_addrs(host, &sockets)
            .build()
            .map_err(|_| WebError::RequestFailed)?;
        let mut builder = client
            .get(request.url.clone())
            .header("User-Agent", USER_AGENT);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        let response = builder.send().await.map_err(map_reqwest_error)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
            })
            .collect();
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        let mut truncated = false;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(map_reqwest_error)?;
            let remaining = request.max_bytes.saturating_sub(body.len());
            if chunk.len() > remaining {
                body.extend_from_slice(&chunk[..remaining]);
                truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
            if body.len() == request.max_bytes {
                truncated = true;
                break;
            }
        }
        Ok(WebHttpResponse {
            status,
            headers,
            body,
            truncated,
        })
    }
}

fn map_reqwest_error(error: reqwest::Error) -> WebError {
    if error.is_timeout() {
        WebError::Timeout
    } else {
        WebError::RequestFailed
    }
}

pub struct WebSearchConfiguration {
    provider: WebSearchProvider,
    credential: Option<String>,
}

impl WebSearchConfiguration {
    pub fn jina(credential: Option<String>) -> Self {
        Self {
            provider: WebSearchProvider::Jina,
            credential,
        }
    }

    pub fn brave(credential: String) -> Self {
        Self {
            provider: WebSearchProvider::Brave,
            credential: Some(credential),
        }
    }
}

pub struct WebClient {
    resolver: Arc<dyn DnsResolver>,
    http: Arc<dyn WebHttpAdapter>,
    search: WebSearchConfiguration,
    limits: WebLimits,
}

impl WebClient {
    pub fn new(search: WebSearchConfiguration) -> Self {
        Self::with_adapters(
            search,
            WebLimits::default(),
            Arc::new(SystemDnsResolver),
            Arc::new(ReqwestWebHttpAdapter),
        )
    }

    pub fn with_adapters(
        search: WebSearchConfiguration,
        limits: WebLimits,
        resolver: Arc<dyn DnsResolver>,
        http: Arc<dyn WebHttpAdapter>,
    ) -> Self {
        Self {
            resolver,
            http,
            search,
            limits,
        }
    }

    pub async fn fetch(
        &self,
        raw_url: &str,
        max_chars: Option<usize>,
    ) -> Result<WebFetchResult, WebError> {
        let response = self
            .get_following_safe_redirects(raw_url, BTreeMap::new())
            .await?;
        if !(200..300).contains(&response.status) {
            return Err(WebError::HttpStatus(response.status));
        }
        let content_type = response
            .headers
            .get("content-type")
            .map(|value| {
                value
                    .split(';')
                    .next()
                    .unwrap_or(value)
                    .trim()
                    .to_ascii_lowercase()
            })
            .unwrap_or_else(|| "text/plain".to_string());
        let body_text = String::from_utf8_lossy(&response.body);
        let title = if content_type == "text/html" {
            extract_html_title(&body_text)
        } else {
            None
        };
        let decoded = match content_type.as_str() {
            "text/html" => {
                html2text::from_read(response.body.as_slice(), 100).map_err(|_| WebError::Decode)?
            }
            "application/json" => serde_json::from_slice::<Value>(&response.body)
                .map(|value| {
                    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
                })
                .map_err(|_| WebError::Decode)?,
            value if value.starts_with("text/") => body_text.into_owned(),
            _ => return Err(WebError::UnsupportedMime(content_type)),
        };
        let character_limit = max_chars
            .unwrap_or(self.limits.max_chars)
            .min(self.limits.max_chars);
        let (content, character_truncated) = truncate_chars(decoded, character_limit);
        Ok(WebFetchResult {
            url: response.url.to_string(),
            content_type,
            title,
            content,
            retrieved_at: Utc::now().to_rfc3339(),
            truncated: response.truncated || character_truncated,
        })
    }

    pub async fn search(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> Result<WebSearchResult, WebError> {
        let query = query.trim();
        if query.is_empty() || query.chars().count() > 500 {
            return Err(WebError::InvalidQuery);
        }
        let limit = limit.unwrap_or(5).clamp(1, self.limits.max_results);
        match self.search.provider {
            WebSearchProvider::Jina => self.search_jina(query, limit).await,
            WebSearchProvider::Brave => self.search_brave(query, limit).await,
        }
    }

    async fn search_jina(&self, query: &str, limit: usize) -> Result<WebSearchResult, WebError> {
        let mut url = Url::parse("https://s.jina.ai/").expect("static Jina search URL");
        url.query_pairs_mut().append_pair("q", query);
        let mut headers = BTreeMap::from([("accept".to_string(), "application/json".to_string())]);
        if let Some(credential) = self.search.credential.as_deref() {
            headers.insert("authorization".to_string(), format!("Bearer {credential}"));
        }
        let response = self
            .get_following_safe_redirects(url.as_str(), headers)
            .await?;
        let value = parse_json_response(response)?;
        let rows = value
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(WebSearchResult {
            query: query.to_string(),
            provider: WebSearchProvider::Jina,
            sources: parse_sources(&rows, limit, "description", "date"),
        })
    }

    async fn search_brave(&self, query: &str, limit: usize) -> Result<WebSearchResult, WebError> {
        let credential = self
            .search
            .credential
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or(WebError::MissingSearchCredential)?;
        let mut url = Url::parse("https://api.search.brave.com/res/v1/web/search")
            .expect("static Brave search URL");
        url.query_pairs_mut()
            .append_pair("q", query)
            .append_pair("count", &limit.to_string());
        let headers = BTreeMap::from([
            ("accept".to_string(), "application/json".to_string()),
            ("x-subscription-token".to_string(), credential.to_string()),
        ]);
        let response = self
            .get_following_safe_redirects(url.as_str(), headers)
            .await?;
        let value = parse_json_response(response)?;
        let rows = value
            .pointer("/web/results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(WebSearchResult {
            query: query.to_string(),
            provider: WebSearchProvider::Brave,
            sources: parse_sources(&rows, limit, "description", "page_age"),
        })
    }

    async fn get_following_safe_redirects(
        &self,
        raw_url: &str,
        headers: BTreeMap<String, String>,
    ) -> Result<SafeResponse, WebError> {
        let mut url = Url::parse(raw_url).map_err(|_| WebError::MissingHost)?;
        let mut headers = headers;
        for redirect_count in 0..=self.limits.max_redirects {
            let approved_addresses = self.validate_and_resolve(&url).await?;
            let response = self
                .http
                .get(WebHttpRequest {
                    url: url.clone(),
                    headers: headers.clone(),
                    approved_addresses,
                    timeout: Duration::from_millis(self.limits.timeout_ms),
                    max_bytes: self.limits.max_bytes,
                })
                .await?;
            if (300..400).contains(&response.status) {
                if redirect_count == self.limits.max_redirects {
                    return Err(WebError::RedirectLimit);
                }
                let location = response
                    .headers
                    .get("location")
                    .ok_or(WebError::InvalidRedirect)?;
                let next_url = url.join(location).map_err(|_| WebError::InvalidRedirect)?;
                update_redirect_headers(
                    &mut headers,
                    &response.headers,
                    url.origin() == next_url.origin(),
                );
                url = next_url;
                continue;
            }
            return Ok(SafeResponse {
                url,
                status: response.status,
                headers: response.headers,
                body: response.body,
                truncated: response.truncated,
            });
        }
        Err(WebError::RedirectLimit)
    }

    async fn validate_and_resolve(&self, url: &Url) -> Result<Vec<IpAddr>, WebError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(WebError::UnsupportedScheme);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(WebError::EmbeddedCredentials);
        }
        let host = url.host_str().ok_or(WebError::MissingHost)?;
        if host.eq_ignore_ascii_case("localhost")
            || host.ends_with(".localhost")
            || host.ends_with(".local")
        {
            return Err(WebError::BlockedAddress);
        }
        let addresses = match url.host().ok_or(WebError::MissingHost)? {
            url::Host::Ipv4(address) => vec![IpAddr::V4(address)],
            url::Host::Ipv6(address) => vec![IpAddr::V6(address)],
            url::Host::Domain(_) => {
                self.resolver
                    .resolve(
                        host,
                        url.port_or_known_default().ok_or(WebError::MissingHost)?,
                    )
                    .await?
            }
        };
        if addresses.is_empty() || addresses.iter().any(|address| is_blocked_ip(*address)) {
            return Err(WebError::BlockedAddress);
        }
        Ok(addresses)
    }
}

/// Carry a bounded challenge cookie only across a same-origin redirect. Some documentation sites
/// issue a one-hop bot/challenge cookie and redirect back to the same URL; dropping that cookie
/// turns the hop into an artificial redirect loop. Provider credentials and cookies are always
/// removed before following a cross-origin Location.
fn update_redirect_headers(
    request_headers: &mut BTreeMap<String, String>,
    response_headers: &BTreeMap<String, String>,
    same_origin: bool,
) {
    if !same_origin {
        request_headers.retain(|name, _| {
            !matches!(
                name.to_ascii_lowercase().as_str(),
                "authorization" | "proxy-authorization" | "cookie" | "x-subscription-token"
            )
        });
        return;
    }

    let Some(cookie) = response_headers
        .get("set-cookie")
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| {
            let Some((name, _)) = value.split_once('=') else {
                return false;
            };
            !name.is_empty()
                && value.len() <= 4_096
                && value.is_ascii()
                && !value.bytes().any(|byte| byte.is_ascii_control())
        })
    else {
        return;
    };
    request_headers.insert("cookie".to_string(), cookie.to_string());
}

struct SafeResponse {
    url: Url,
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
    truncated: bool,
}

fn parse_json_response(response: SafeResponse) -> Result<Value, WebError> {
    if !(200..300).contains(&response.status) {
        return Err(WebError::HttpStatus(response.status));
    }
    serde_json::from_slice(&response.body).map_err(|_| WebError::Decode)
}

fn parse_sources(
    rows: &[Value],
    limit: usize,
    snippet_field: &str,
    published_field: &str,
) -> Vec<WebSource> {
    let retrieved_at = Utc::now().to_rfc3339();
    rows.iter()
        .filter_map(|row| {
            let url = row.get("url")?.as_str()?;
            let parsed = Url::parse(url).ok()?;
            if !matches!(parsed.scheme(), "http" | "https") || !parsed.username().is_empty() {
                return None;
            }
            Some(WebSource {
                title: row
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or(url)
                    .to_string(),
                url: parsed.to_string(),
                snippet: row
                    .get(snippet_field)
                    .or_else(|| row.get("content"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .chars()
                    .take(2_000)
                    .collect(),
                published_at: row
                    .get(published_field)
                    .and_then(Value::as_str)
                    .map(str::to_string),
                retrieved_at: retrieved_at.clone(),
            })
        })
        .take(limit)
        .collect()
}

fn extract_html_title(html: &str) -> Option<String> {
    let lowercase = html.to_ascii_lowercase();
    let title_start = lowercase.find("<title")?;
    let content_start = lowercase[title_start..].find('>')? + title_start + 1;
    let content_end = lowercase[content_start..].find("</title>")? + content_start;
    let title = html[content_start..content_end].trim();
    (!title.is_empty()).then(|| title.chars().take(500).collect())
}

fn truncate_chars(content: String, limit: usize) -> (String, bool) {
    let mut iterator = content.chars();
    let truncated = iterator.clone().nth(limit).is_some();
    (iterator.by_ref().take(limit).collect(), truncated)
}

pub fn is_blocked_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_blocked_ipv4(address),
        IpAddr::V6(address) => is_blocked_ipv6(address),
    }
}

fn is_blocked_ipv4(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_multicast()
        || address.is_unspecified()
        || address.is_broadcast()
        || octets[0] == 0
        || octets[0] >= 224
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
}

fn is_blocked_ipv6(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    address.is_unspecified()
        || address.is_loopback()
        || address.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || embedded_ipv4(address).is_some_and(is_blocked_ipv4)
}

/// Return an IPv4 address encoded in one of the IPv6 forms that has an
/// unambiguous direct IPv4 equivalent.
///
/// `Ipv6Addr::to_ipv4_mapped` only recognizes `::ffff:a.b.c.d`. The older,
/// still-parseable IPv4-compatible form `::a.b.c.d` reaches the same IPv4
/// endpoint but would otherwise bypass the IPv4 private-range checks.
fn embedded_ipv4(address: Ipv6Addr) -> Option<Ipv4Addr> {
    let octets = address.octets();
    let is_compatible = octets[..12].iter().all(|octet| *octet == 0);
    let is_mapped =
        octets[..10].iter().all(|octet| *octet == 0) && octets[10] == 0xff && octets[11] == 0xff;
    (is_compatible || is_mapped)
        .then(|| Ipv4Addr::new(octets[12], octets[13], octets[14], octets[15]))
}

pub struct WebToolHost {
    client: Arc<WebClient>,
}

impl WebToolHost {
    pub fn new(client: Arc<WebClient>) -> Self {
        Self { client }
    }

    fn error_outcome(error: WebError) -> ToolCallOutcome {
        ToolCallOutcome {
            content: error.to_string(),
            is_error: true,
            metadata: Some(json!({"error_code": web_error_code(&error)})),
        }
    }
}

#[async_trait]
impl ToolHost for WebToolHost {
    async fn list_tools(&self) -> hermes_core::Result<Vec<ToolSpec>> {
        Ok(vec![
            ToolSpec {
                name: "web_search".to_string(),
                description: "Search the public web and return bounded sources with URLs."
                    .to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "minLength": 1, "maxLength": 500},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 10}
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }),
                source: ToolSource::Builtin,
                requires_confirmation: false,
            },
            ToolSpec {
                name: "web_fetch".to_string(),
                description:
                    "Fetch a public HTTP(S) page with SSRF, redirect, MIME and size limits."
                        .to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "minLength": 1},
                        "max_chars": {"type": "integer", "minimum": 1, "maximum": 80000}
                    },
                    "required": ["url"],
                    "additionalProperties": false
                }),
                source: ToolSource::Builtin,
                requires_confirmation: false,
            },
        ])
    }

    async fn call(&self, name: &str, args: Value) -> hermes_core::Result<ToolCallOutcome> {
        let result = match name {
            "web_search" => {
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map(|value| value as usize);
                self.client.search(query, limit).await.and_then(|result| {
                    serde_json::to_string_pretty(&result).map_err(|_| WebError::Decode)
                })
            }
            "web_fetch" => {
                let url = args.get("url").and_then(Value::as_str).unwrap_or_default();
                let max_chars = args
                    .get("max_chars")
                    .and_then(Value::as_u64)
                    .map(|value| value as usize);
                self.client.fetch(url, max_chars).await.and_then(|result| {
                    serde_json::to_string_pretty(&result).map_err(|_| WebError::Decode)
                })
            }
            _ => return Err(hermes_core::Error::ToolNotFound(name.to_string())),
        };
        Ok(match result {
            Ok(content) => ToolCallOutcome {
                content,
                is_error: false,
                metadata: None,
            },
            Err(error) => Self::error_outcome(error),
        })
    }

    async fn call_batch(
        &self,
        calls: Vec<(String, Value)>,
    ) -> Vec<hermes_core::Result<ToolCallOutcome>> {
        join_all(
            calls
                .into_iter()
                .map(|(name, args)| async move { self.call(&name, args).await }),
        )
        .await
    }
}

fn web_error_code(error: &WebError) -> &'static str {
    match error {
        WebError::UnsupportedScheme
        | WebError::EmbeddedCredentials
        | WebError::MissingHost
        | WebError::BlockedAddress => "unsafe_url",
        WebError::DnsLookup => "dns_failed",
        WebError::Timeout => "timeout",
        WebError::RequestFailed => "request_failed",
        WebError::RedirectLimit | WebError::InvalidRedirect => "redirect_failed",
        WebError::HttpStatus(_) => "http_status",
        WebError::UnsupportedMime(_) => "unsupported_mime",
        WebError::Decode => "decode_failed",
        WebError::InvalidQuery => "invalid_query",
        WebError::MissingSearchCredential => "missing_credential",
    }
}

#[cfg(test)]
mod redirect_header_tests {
    use super::*;

    #[test]
    fn same_origin_redirect_carries_only_the_cookie_pair() {
        let mut request = BTreeMap::new();
        let response = BTreeMap::from([(
            "set-cookie".to_string(),
            "milvus_challenge=abc123; Path=/; HttpOnly; SameSite=Lax".to_string(),
        )]);

        update_redirect_headers(&mut request, &response, true);

        assert_eq!(
            request.get("cookie").map(String::as_str),
            Some("milvus_challenge=abc123")
        );
    }

    #[test]
    fn cross_origin_redirect_strips_credentials_and_cookie() {
        let mut request = BTreeMap::from([
            ("accept".to_string(), "application/json".to_string()),
            ("authorization".to_string(), "Bearer secret".to_string()),
            ("cookie".to_string(), "challenge=secret".to_string()),
            ("x-subscription-token".to_string(), "secret".to_string()),
        ]);

        update_redirect_headers(&mut request, &BTreeMap::new(), false);

        assert_eq!(
            request.get("accept").map(String::as_str),
            Some("application/json")
        );
        assert!(!request.contains_key("authorization"));
        assert!(!request.contains_key("cookie"));
        assert!(!request.contains_key("x-subscription-token"));
    }
}
