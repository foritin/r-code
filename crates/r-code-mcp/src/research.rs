use std::{collections::BTreeSet, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::{stream, StreamExt};
use agent_contract::ToolCallOutcome;
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
        ToolAnnotations,
    },
    service::{RequestContext, RoleServer, RunningService},
    ServerHandler, ServiceExt,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::sync::Mutex;

use crate::{
    client::RmcpSession, McpClientError, McpClientSession, McpToolDescriptor, RmcpConnector,
    WebClient, WebSource,
};

const RESEARCH_CLOSE_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_RESEARCH_QUERIES: usize = 5;
const MAX_RESEARCH_SOURCES: usize = 12;
const RESEARCH_CONCURRENCY: usize = 3;

#[derive(Debug, Deserialize)]
struct SearchSourcesRequest {
    query: String,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct FetchSourceRequest {
    url: String,
    max_chars: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct DeepResearchRequest {
    queries: Vec<String>,
    max_sources: Option<usize>,
}

#[derive(Debug, Serialize)]
struct ResearchEvidencePacket {
    queries: Vec<String>,
    sources: Vec<WebSource>,
    errors: Vec<ResearchQueryError>,
    retrieved_at: String,
    synthesis: &'static str,
}

#[derive(Debug, Serialize)]
struct ResearchQueryError {
    query: String,
    error: String,
}

#[derive(Clone)]
pub struct ResearchServer {
    web: Arc<WebClient>,
}

impl ResearchServer {
    pub fn new(web: Arc<WebClient>) -> Self {
        Self { web }
    }

    async fn search_sources(&self, request: SearchSourcesRequest) -> Result<String, String> {
        let result = self
            .web
            .search(&request.query, request.limit)
            .await
            .map_err(|error| error.to_string())?;
        serde_json::to_string_pretty(&result)
            .map_err(|_| "failed to serialize search result".to_string())
    }

    async fn fetch_source(&self, request: FetchSourceRequest) -> Result<String, String> {
        let result = self
            .web
            .fetch(&request.url, request.max_chars)
            .await
            .map_err(|error| error.to_string())?;
        serde_json::to_string_pretty(&result)
            .map_err(|_| "failed to serialize fetched source".to_string())
    }

    async fn deep_research(&self, request: DeepResearchRequest) -> Result<String, String> {
        let queries = request
            .queries
            .into_iter()
            .map(|query| query.trim().to_string())
            .filter(|query| !query.is_empty())
            .take(MAX_RESEARCH_QUERIES)
            .collect::<Vec<_>>();
        if queries.is_empty() {
            return Err("at least one non-empty query is required".to_string());
        }
        let max_sources = request
            .max_sources
            .unwrap_or(MAX_RESEARCH_SOURCES)
            .clamp(1, MAX_RESEARCH_SOURCES);
        let mut batches = stream::iter(queries.iter().cloned().enumerate())
            .map(|(index, query)| {
                let web = self.web.clone();
                async move {
                    let result = web.search(&query, Some(5)).await;
                    (index, query, result)
                }
            })
            .buffer_unordered(RESEARCH_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        batches.sort_by_key(|(index, _, _)| *index);

        let mut seen = BTreeSet::new();
        let mut sources = Vec::new();
        let mut errors = Vec::new();
        for (_, query, result) in batches {
            match result {
                Ok(result) => {
                    for source in result.sources {
                        if seen.insert(source.url.clone()) {
                            sources.push(source);
                            if sources.len() == max_sources {
                                break;
                            }
                        }
                    }
                }
                Err(error) => errors.push(ResearchQueryError {
                    query,
                    error: error.to_string(),
                }),
            }
            if sources.len() == max_sources {
                break;
            }
        }
        let packet = ResearchEvidencePacket {
            queries,
            sources,
            errors,
            retrieved_at: chrono::Utc::now().to_rfc3339(),
            synthesis: "not_performed; the calling agent must synthesize the cited evidence",
        };
        serde_json::to_string_pretty(&packet)
            .map_err(|_| "failed to serialize research evidence".to_string())
    }

    fn tool_definitions() -> Vec<Tool> {
        vec![
            research_tool(
                "search_sources",
                "Search the public web and return bounded source metadata with citations.",
                json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "minLength": 1, "maxLength": 500},
                        "limit": {"type": ["integer", "null"], "minimum": 1, "maximum": 10}
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }),
            ),
            research_tool(
                "fetch_source",
                "Fetch one public source with SSRF, redirect, MIME and size protections.",
                json!({
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "minLength": 1},
                        "max_chars": {"type": ["integer", "null"], "minimum": 1, "maximum": 80000}
                    },
                    "required": ["url"],
                    "additionalProperties": false
                }),
            ),
            research_tool(
                "deep_research",
                "Run bounded parallel searches and return an evidence packet without conclusions.",
                json!({
                    "type": "object",
                    "properties": {
                        "queries": {
                            "type": "array",
                            "items": {"type": "string", "minLength": 1, "maxLength": 500},
                            "minItems": 1,
                            "maxItems": 5
                        },
                        "max_sources": {"type": ["integer", "null"], "minimum": 1, "maximum": 12}
                    },
                    "required": ["queries"],
                    "additionalProperties": false
                }),
            ),
        ]
    }
}

impl ServerHandler for ResearchServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "R-Code's bounded evidence-gathering service. Results are sources, not an LLM conclusion.",
        )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(Self::tool_definitions()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        Self::tool_definitions()
            .into_iter()
            .find(|tool| tool.name.as_ref() == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        let result = match request.name.as_ref() {
            "search_sources" => match parse_arguments(arguments) {
                Ok(request) => self.search_sources(request).await,
                Err(error) => Err(error),
            },
            "fetch_source" => match parse_arguments(arguments) {
                Ok(request) => self.fetch_source(request).await,
                Err(error) => Err(error),
            },
            "deep_research" => match parse_arguments(arguments) {
                Ok(request) => self.deep_research(request).await,
                Err(error) => Err(error),
            },
            _ => Err(format!("unknown research tool: {}", request.name)),
        };
        Ok(match result {
            Ok(content) => CallToolResult::success(vec![ContentBlock::text(content)]).into(),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error)]).into(),
        })
    }
}

fn research_tool(name: &'static str, description: &'static str, schema: Value) -> Tool {
    let schema = schema.as_object().cloned().unwrap_or_else(Map::new);
    Tool::new(name, description, schema).with_annotations(
        ToolAnnotations::new()
            .read_only(true)
            .destructive(false)
            .idempotent(true)
            .open_world(true),
    )
}

fn parse_arguments<T: DeserializeOwned>(arguments: Value) -> Result<T, String> {
    serde_json::from_value(arguments).map_err(|error| format!("invalid tool arguments: {error}"))
}

struct ResearchSession {
    client: RmcpSession,
    server: Mutex<Option<RunningService<RoleServer, ResearchServer>>>,
}

#[async_trait]
impl McpClientSession for ResearchSession {
    async fn list_tools(&self, server_id: &str) -> Result<Vec<McpToolDescriptor>, McpClientError> {
        self.client.list_tools(server_id).await
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<ToolCallOutcome, McpClientError> {
        self.client.call_tool(name, args).await
    }

    async fn call_tool_with_abort(
        &self,
        name: &str,
        args: Value,
        abort: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<ToolCallOutcome, McpClientError> {
        self.client.call_tool_with_abort(name, args, abort).await
    }

    async fn close(&self) -> Result<(), McpClientError> {
        self.client.close().await?;
        let Some(mut server) = self.server.lock().await.take() else {
            return Ok(());
        };
        server
            .close_with_timeout(RESEARCH_CLOSE_TIMEOUT)
            .await
            .map_err(|error| McpClientError::Shutdown(error.to_string()))?;
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.client.is_closed()
    }
}

pub(crate) async fn connect_research_session(
    connector: &RmcpConnector,
    web: Arc<WebClient>,
) -> Result<Arc<dyn McpClientSession>, McpClientError> {
    let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
    let server_task = tokio::spawn(async move {
        ResearchServer::new(web)
            .serve(server_transport)
            .await
            .map_err(|error| error.to_string())
    });
    let client = match connector.serve(client_transport).await {
        Ok(client) => client,
        Err(error) => {
            server_task.abort();
            return Err(error);
        }
    };
    let server = server_task
        .await
        .map_err(|error| McpClientError::Initialize(error.to_string()))?
        .map_err(McpClientError::Initialize)?;
    Ok(Arc::new(ResearchSession {
        client,
        server: Mutex::new(Some(server)),
    }))
}
