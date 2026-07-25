//! ControlDoor - 外部 CLI 控制门 [doc-05 §6]
//!
//! Unix Socket 上的 HTTP 服务，供外部 CLI（claude、codex）控制终端。
//! 门将 HTTP 请求翻译为相同的 Tool Gateway + Permission Engine 路径。
//!
//! ## 协议
//! - HTTP over Unix Socket（mode 0600）
//! - Token 认证必需（`Authorization: Bearer <token>`）
//!
//! ## 路由
//! - `GET    /v1/terminals`            -> terminal.list
//! - `POST   /v1/terminals`            -> terminal.create
//! - `POST   /v1/terminals/{id}/send`  -> terminal.send
//! - `POST   /v1/terminals/{id}/wait`  -> terminal.wait
//! - `GET    /v1/terminals/{id}/read`  -> terminal.read
//! - `DELETE /v1/terminals/{id}/kill`  -> terminal.kill
//!
//! ## 失败语义 [doc-05 §6.2]
//! - 403 bad/missing token
//! - 404 unknown target
//! - 409 depth/self-control refusal
//! - 429 over budget
//! - 501 orchestration disabled / unknown route

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use r_code_core::error::ProductError;
use r_code_terminal::{SendOptions, TerminalControlService, WaitMode, WaitResult};
use tokio::sync::Mutex;

/// 默认每分钟最大 send 数 [doc-05 §4]。
const DEFAULT_MAX_SENDS_PER_MIN: u32 = 30;

/// ControlDoor - Unix Socket HTTP service for external CLI access.
///
/// External CLIs (claude, codex) connect to this door to control terminals.
/// The door translates HTTP requests to the same Tool Gateway + Permission Engine.
///
/// Protocol: HTTP over Unix Socket (mode 0600).
/// Token authentication required (R_CODE_CTL_TOKEN env var).
pub struct ControlDoor {
    socket_path: PathBuf,
    token: String,
    control_service: Arc<TerminalControlService>,
    /// 每分钟最大 send 数（预算跟踪）[doc-05 §4]
    max_sends_per_min: u32,
    /// send 时间戳（用于滑动窗口预算）
    send_timestamps: Arc<Mutex<Vec<Instant>>>,
}

impl ControlDoor {
    /// Create a new ControlDoor.
    /// socket_path: `<userData>/ctl.sock`
    /// token: random, generated per launch, never persisted to disk
    pub fn new(
        socket_path: PathBuf,
        token: String,
        control_service: Arc<TerminalControlService>,
    ) -> Self {
        Self {
            socket_path,
            token,
            control_service,
            max_sends_per_min: DEFAULT_MAX_SENDS_PER_MIN,
            send_timestamps: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// 使用自定义 send 预算创建（用于测试）。
    #[cfg(test)]
    fn with_budget(
        socket_path: PathBuf,
        token: String,
        control_service: Arc<TerminalControlService>,
        max_sends_per_min: u32,
    ) -> Self {
        Self {
            socket_path,
            token,
            control_service,
            max_sends_per_min,
            send_timestamps: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Start the HTTP server on the Unix Socket.
    /// All requests must include `Authorization: Bearer <token>` header.
    ///
    /// 连接串行处理（控制门请求频率低，无需并发）。`TerminalManager` 内部
    /// 的 `PtySystem` 不是 `Sync`，因此本 future 不是 `Send`——应在主任务
    /// 或 `LocalSet` 上运行，不要 `tokio::spawn`。
    #[cfg(unix)]
    pub async fn serve(self) -> Result<(), ProductError> {
        use tokio::net::UnixListener;

        // 移除残留的 socket 文件
        let _ = std::fs::remove_file(&self.socket_path);

        let listener = UnixListener::bind(&self.socket_path)
            .map_err(|e| ProductError::IpcError(format!("ctl.sock bind failed: {e}")))?;

        // 设置 mode 0600 [doc-05 §6.1]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.socket_path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| ProductError::IpcError(format!("ctl.sock chmod failed: {e}")))?;
        }

        let arc = Arc::new(self);

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    // 串行处理：控制门请求频率低，且 PtySystem 非 Sync 无法 spawn
                    if let Err(e) = handle_connection(stream, &arc).await {
                        tracing::warn!("ctl.sock connection error: {e}");
                    }
                }
                Err(e) => {
                    tracing::warn!("ctl.sock accept failed: {e}");
                    continue;
                }
            }
        }
    }

    /// 检查并记录 send 预算。返回 false 表示超预算。
    async fn check_send_budget(&self) -> bool {
        if self.max_sends_per_min == 0 {
            return false;
        }
        let mut timestamps = self.send_timestamps.lock().await;
        let now = Instant::now();
        let one_min_ago = now - Duration::from_secs(60);
        timestamps.retain(|&t| t > one_min_ago);
        if timestamps.len() >= self.max_sends_per_min as usize {
            return false;
        }
        timestamps.push(now);
        true
    }

    /// 路由 HTTP 请求到对应的 terminal control service 方法。
    async fn route(&self, request: &HttpRequest) -> HttpResponse {
        // 验证 token [doc-05 §6.1]
        match &request.token {
            Some(t) if t == &self.token => {}
            _ => return HttpResponse::forbidden("bad or missing token"),
        }

        // 解析路径
        let path = request.path.trim_start_matches('/');
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

        match (request.method.as_str(), segments.as_slice()) {
            ("GET", ["v1", "terminals"]) => match self.control_service.list().await {
                Ok(terminals) => {
                    let body =
                        serde_json::to_string(&terminals).unwrap_or_else(|_| "[]".to_string());
                    HttpResponse::ok(&body)
                }
                Err(e) => error_to_response(&e),
            },

            ("POST", ["v1", "terminals"]) => {
                let req: CreateRequest = match parse_body(&request.body) {
                    Some(r) => r,
                    None => return HttpResponse::bad_request("invalid create body"),
                };
                match self
                    .control_service
                    .create(&req.shell, std::path::Path::new(&req.working_dir), req.env)
                    .await
                {
                    Ok(id) => {
                        let body = serde_json::json!({ "id": id }).to_string();
                        HttpResponse::created(&body)
                    }
                    Err(e) => error_to_response(&e),
                }
            }

            ("POST", ["v1", "terminals", id, "send"]) => {
                // 预算检查 [doc-05 §4]
                if !self.check_send_budget().await {
                    return HttpResponse::too_many_requests("send budget exceeded");
                }
                let req: SendRequest = match parse_body(&request.body) {
                    Some(r) => r,
                    None => return HttpResponse::bad_request("invalid send body"),
                };
                match self
                    .control_service
                    .send(
                        id,
                        req.caller_terminal_id.as_deref(),
                        SendOptions {
                            text: req.text,
                            press_enter: req.press_enter.unwrap_or(false),
                        },
                    )
                    .await
                {
                    Ok(()) => HttpResponse::ok("{\"ok\":true}"),
                    Err(e) => error_to_response(&e),
                }
            }

            ("POST", ["v1", "terminals", id, "wait"]) => {
                let req: WaitRequest = match parse_body(&request.body) {
                    Some(r) => r,
                    None => return HttpResponse::bad_request("invalid wait body"),
                };
                let mode = match req.mode.as_str() {
                    "exit_code" => WaitMode::ExitCode,
                    "quiet" => WaitMode::Quiet {
                        quiet_ms: req.quiet_ms.unwrap_or(1000),
                    },
                    "pattern" => WaitMode::Pattern {
                        pattern: req.pattern.clone().unwrap_or_default(),
                    },
                    _ => return HttpResponse::bad_request("invalid wait mode"),
                };
                match self.control_service.wait(id, mode, req.timeout_ms).await {
                    Ok(result) => {
                        let body = wait_result_to_json(&result).to_string();
                        HttpResponse::ok(&body)
                    }
                    Err(e) => error_to_response(&e),
                }
            }

            ("GET", ["v1", "terminals", id, "read"]) => match self.control_service.read(id).await {
                Ok(text) => {
                    let body = serde_json::json!({ "text": text }).to_string();
                    HttpResponse::ok(&body)
                }
                Err(e) => error_to_response(&e),
            },

            ("DELETE", ["v1", "terminals", id, "kill"]) => {
                let req: KillRequest = parse_body(&request.body).unwrap_or_default();
                match self.control_service.kill(id, req.caller_is_agent).await {
                    Ok(()) => HttpResponse::ok("{\"ok\":true}"),
                    Err(e) => error_to_response(&e),
                }
            }

            _ => HttpResponse::not_implemented("unknown route"),
        }
    }
}

/// HTTP request parsed from the Unix Socket.
#[derive(Debug, Clone)]
struct HttpRequest {
    method: String,
    path: String,
    body: String,
    token: Option<String>,
}

/// HTTP response to write back.
#[derive(Debug, Clone)]
struct HttpResponse {
    status: u16,
    body: String,
}

impl HttpResponse {
    fn ok(body: &str) -> Self {
        Self {
            status: 200,
            body: body.to_string(),
        }
    }
    fn created(body: &str) -> Self {
        Self {
            status: 201,
            body: body.to_string(),
        }
    }
    fn bad_request(msg: &str) -> Self {
        Self {
            status: 400,
            body: msg.to_string(),
        }
    }
    fn forbidden(msg: &str) -> Self {
        Self {
            status: 403,
            body: msg.to_string(),
        }
    }
    fn not_found(msg: &str) -> Self {
        Self {
            status: 404,
            body: msg.to_string(),
        }
    }
    fn conflict(msg: &str) -> Self {
        Self {
            status: 409,
            body: msg.to_string(),
        }
    }
    fn too_many_requests(msg: &str) -> Self {
        Self {
            status: 429,
            body: msg.to_string(),
        }
    }
    fn not_implemented(msg: &str) -> Self {
        Self {
            status: 501,
            body: msg.to_string(),
        }
    }
}

// === 请求体类型 ===

#[derive(Debug, serde::Deserialize)]
struct CreateRequest {
    shell: String,
    working_dir: String,
    #[serde(default)]
    env: Vec<(String, String)>,
}

#[derive(Debug, serde::Deserialize)]
struct SendRequest {
    text: String,
    press_enter: Option<bool>,
    caller_terminal_id: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct WaitRequest {
    mode: String,
    quiet_ms: Option<u64>,
    pattern: Option<String>,
    timeout_ms: u64,
}

#[derive(Debug, Default, serde::Deserialize)]
struct KillRequest {
    #[serde(default)]
    caller_is_agent: bool,
}

/// 解析 JSON 请求体。空 body 返回 None。
fn parse_body<T: serde::de::DeserializeOwned>(body: &str) -> Option<T> {
    if body.trim().is_empty() {
        return None;
    }
    serde_json::from_str(body).ok()
}

/// 将 WaitResult 转为 JSON。
fn wait_result_to_json(result: &WaitResult) -> serde_json::Value {
    match result {
        WaitResult::Exited(code) => {
            serde_json::json!({ "result": "exited", "exit_code": code })
        }
        WaitResult::Quiet => serde_json::json!({ "result": "quiet" }),
        WaitResult::PatternMatched(matched) => {
            serde_json::json!({ "result": "pattern_matched", "matched": matched })
        }
        WaitResult::Timeout => serde_json::json!({ "result": "timeout" }),
        WaitResult::Cancelled => serde_json::json!({ "result": "cancelled" }),
    }
}

/// 将 ProductError 映射为 HTTP 响应 [doc-05 §6.2]。
fn error_to_response(err: &ProductError) -> HttpResponse {
    let msg = err.to_string();
    match err {
        ProductError::PermissionError(_) => {
            if msg.contains("self-control") {
                HttpResponse::conflict(&msg)
            } else {
                // agent kill / 其他权限拒绝 -> 403
                HttpResponse::forbidden(&msg)
            }
        }
        ProductError::TerminalError(_) => HttpResponse::not_found(&msg),
        _ => HttpResponse::not_found(&msg),
    }
}

/// 解析原始 HTTP 请求字节为 HttpRequest。
fn parse_request(raw: &[u8]) -> Result<HttpRequest, ProductError> {
    let text = std::str::from_utf8(raw)
        .map_err(|e| ProductError::IpcError(format!("invalid UTF-8 in request: {e}")))?;

    let (header_section, body) = text.split_once("\r\n\r\n").unwrap_or((text, ""));

    let mut lines = header_section.lines();
    let request_line = lines
        .next()
        .ok_or_else(|| ProductError::IpcError("empty HTTP request".to_string()))?;

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    // 解析 headers（查找 Authorization）
    let mut token = None;
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_lowercase();
            let value = value.trim();
            if key == "authorization" {
                if let Some(t) = value.strip_prefix("Bearer ") {
                    token = Some(t.to_string());
                }
            }
        }
    }

    Ok(HttpRequest {
        method,
        path,
        body: body.to_string(),
        token,
    })
}

/// 从 header 文本中提取 Content-Length。
fn extract_content_length(headers: &str) -> Option<usize> {
    for line in headers.lines() {
        if let Some((key, value)) = line.split_once(':') {
            if key.trim().eq_ignore_ascii_case("content-length") {
                return value.trim().parse().ok();
            }
        }
    }
    None
}

/// 格式化 HTTP 响应为字符串。
fn format_response(response: &HttpResponse) -> String {
    let status_text = match response.status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        429 => "Too Many Requests",
        501 => "Not Implemented",
        _ => "Internal Server Error",
    };
    format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        response.status,
        status_text,
        response.body.len(),
        response.body
    )
}

/// 处理单个连接（Unix only）。
#[cfg(unix)]
async fn handle_connection(
    mut stream: tokio::net::UnixStream,
    door: &ControlDoor,
) -> Result<(), ProductError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];

    // 读取直到 header 结束（\r\n\r\n）
    loop {
        let n = stream
            .read(&mut tmp)
            .await
            .map_err(|e| ProductError::IpcError(format!("read failed: {e}")))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 1024 * 1024 {
            return Ok(()); // 请求过大，丢弃
        }
    }

    if buf.is_empty() {
        return Ok(());
    }

    // 找到 header/body 边界
    let header_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| ProductError::IpcError("malformed HTTP request".to_string()))?;

    let header_text = std::str::from_utf8(&buf[..header_end])
        .map_err(|e| ProductError::IpcError(format!("invalid UTF-8 in headers: {e}")))?;

    let content_length = extract_content_length(header_text);

    // 如果有 Content-Length，读取完整 body
    let body_start = header_end + 4;
    if let Some(cl) = content_length {
        while buf.len() < body_start + cl {
            let n = stream
                .read(&mut tmp)
                .await
                .map_err(|e| ProductError::IpcError(format!("read body failed: {e}")))?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
    }

    let request = parse_request(&buf)?;
    let response = door.route(&request).await;
    let response_text = format_response(&response);

    stream
        .write_all(response_text.as_bytes())
        .await
        .map_err(|e| ProductError::IpcError(format!("write failed: {e}")))?;
    stream
        .flush()
        .await
        .map_err(|e| ProductError::IpcError(format!("flush failed: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_terminal::TerminalManager;
    use std::path::Path;

    fn cat_path() -> Option<&'static str> {
        #[cfg(unix)]
        {
            ["/bin/cat", "/usr/bin/cat"]
                .iter()
                .find(|p| Path::new(p).exists())
                .copied()
        }
        #[cfg(windows)]
        {
            Some("cmd.exe")
        }
        #[cfg(not(any(unix, windows)))]
        {
            None
        }
    }

    fn make_door(token: &str) -> ControlDoor {
        let manager = Arc::new(TerminalManager::new());
        let service = Arc::new(TerminalControlService::new(manager));
        ControlDoor::new(
            PathBuf::from("/tmp/test_ctl.sock"),
            token.to_string(),
            service,
        )
    }

    fn make_door_with_budget(token: &str, max_sends: u32) -> ControlDoor {
        let manager = Arc::new(TerminalManager::new());
        let service = Arc::new(TerminalControlService::new(manager));
        ControlDoor::with_budget(
            PathBuf::from("/tmp/test_ctl.sock"),
            token.to_string(),
            service,
            max_sends,
        )
    }

    fn make_request(method: &str, path: &str, token: Option<&str>, body: &str) -> HttpRequest {
        HttpRequest {
            method: method.to_string(),
            path: path.to_string(),
            body: body.to_string(),
            token: token.map(|t| t.to_string()),
        }
    }

    // === parse_request 测试 ===

    #[test]
    fn parse_request_simple_get() {
        let raw = b"GET /v1/terminals HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let req = parse_request(raw).expect("parse should succeed");
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/v1/terminals");
        assert!(req.body.is_empty());
        assert!(req.token.is_none());
    }

    #[test]
    fn parse_request_with_bearer_token() {
        let raw = b"GET /v1/terminals HTTP/1.1\r\nAuthorization: Bearer secret123\r\n\r\n";
        let req = parse_request(raw).expect("parse should succeed");
        assert_eq!(req.token.as_deref(), Some("secret123"));
    }

    #[test]
    fn parse_request_with_body() {
        let raw = b"POST /v1/terminals HTTP/1.1\r\nContent-Length: 13\r\n\r\n{\"shell\":\"sh\"}";
        let req = parse_request(raw).expect("parse should succeed");
        assert_eq!(req.method, "POST");
        assert_eq!(req.body, "{\"shell\":\"sh\"}");
    }

    #[test]
    fn parse_request_missing_token() {
        let raw = b"GET /v1/terminals HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let req = parse_request(raw).expect("parse should succeed");
        assert!(req.token.is_none());
    }

    #[test]
    fn parse_request_invalid_token_format() {
        // 非 Bearer 前缀 -> token 为 None
        let raw = b"GET /v1/terminals HTTP/1.1\r\nAuthorization: Basic abc123\r\n\r\n";
        let req = parse_request(raw).expect("parse should succeed");
        assert!(req.token.is_none());
    }

    // === format_response 测试 ===

    #[test]
    fn format_response_status_codes() {
        let cases = [
            (HttpResponse::ok("ok"), 200, "OK"),
            (HttpResponse::created("created"), 201, "Created"),
            (HttpResponse::bad_request("bad"), 400, "Bad Request"),
            (HttpResponse::forbidden("no"), 403, "Forbidden"),
            (HttpResponse::not_found("nf"), 404, "Not Found"),
            (HttpResponse::conflict("conf"), 409, "Conflict"),
            (
                HttpResponse::too_many_requests("rate"),
                429,
                "Too Many Requests",
            ),
            (HttpResponse::not_implemented("ni"), 501, "Not Implemented"),
        ];
        for (resp, status, text) in cases {
            let formatted = format_response(&resp);
            assert!(
                formatted.contains(&format!("HTTP/1.1 {status} {text}")),
                "expected status {status} {text} in: {formatted}"
            );
        }
    }

    #[test]
    fn format_response_includes_body() {
        let resp = HttpResponse::ok("{\"hello\":true}");
        let formatted = format_response(&resp);
        assert!(formatted.ends_with("{\"hello\":true}"));
        assert!(formatted.contains("Content-Length: 14"));
    }

    // === extract_content_length 测试 ===

    #[test]
    fn extract_content_length_present() {
        let headers = "POST /v1/terminals HTTP/1.1\r\nContent-Length: 42\r\nHost: x";
        assert_eq!(extract_content_length(headers), Some(42));
    }

    #[test]
    fn extract_content_length_case_insensitive() {
        let headers = "POST /x HTTP/1.1\r\ncontent-length: 99\r\nHost: x";
        assert_eq!(extract_content_length(headers), Some(99));
    }

    #[test]
    fn extract_content_length_absent() {
        let headers = "GET /v1/terminals HTTP/1.1\r\nHost: x";
        assert_eq!(extract_content_length(headers), None);
    }

    // === wait_result_to_json 测试 ===

    #[test]
    fn wait_result_json_exited() {
        let json = wait_result_to_json(&WaitResult::Exited(42));
        assert_eq!(json["result"], "exited");
        assert_eq!(json["exit_code"], 42);
    }

    #[test]
    fn wait_result_json_quiet() {
        let json = wait_result_to_json(&WaitResult::Quiet);
        assert_eq!(json["result"], "quiet");
    }

    #[test]
    fn wait_result_json_pattern() {
        let json = wait_result_to_json(&WaitResult::PatternMatched("foo".to_string()));
        assert_eq!(json["result"], "pattern_matched");
        assert_eq!(json["matched"], "foo");
    }

    #[test]
    fn wait_result_json_timeout() {
        let json = wait_result_to_json(&WaitResult::Timeout);
        assert_eq!(json["result"], "timeout");
    }

    // === Token 验证测试 ===

    #[tokio::test]
    async fn route_valid_token_passes_auth() {
        let door = make_door("valid_token");
        let req = make_request("GET", "/v1/terminals", Some("valid_token"), "");
        let resp = door.route(&req).await;
        assert_ne!(resp.status, 403, "valid token should not return 403");
        assert_eq!(resp.status, 200);
    }

    #[tokio::test]
    async fn route_invalid_token_returns_403() {
        let door = make_door("valid_token");
        let req = make_request("GET", "/v1/terminals", Some("wrong_token"), "");
        let resp = door.route(&req).await;
        assert_eq!(resp.status, 403);
        assert!(resp.body.contains("token"));
    }

    #[tokio::test]
    async fn route_missing_token_returns_403() {
        let door = make_door("valid_token");
        let req = make_request("GET", "/v1/terminals", None, "");
        let resp = door.route(&req).await;
        assert_eq!(resp.status, 403);
    }

    // === 路由测试 ===

    #[tokio::test]
    async fn route_list_returns_200() {
        let door = make_door("tok");
        let req = make_request("GET", "/v1/terminals", Some("tok"), "");
        let resp = door.route(&req).await;
        assert_eq!(resp.status, 200);
        // 空列表 -> "[]"
        assert_eq!(resp.body, "[]");
    }

    #[tokio::test]
    async fn route_create_returns_201() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let door = make_door("tok");
        let body = serde_json::json!({
            "shell": cat,
            "working_dir": std::env::temp_dir().to_string_lossy().to_string(),
            "env": []
        })
        .to_string();
        let req = make_request("POST", "/v1/terminals", Some("tok"), &body);
        let resp = door.route(&req).await;
        assert_eq!(resp.status, 201);
        assert!(resp.body.contains("\"id\""));

        // 清理：从 body 解析 id 并 kill
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp.body) {
            if let Some(id) = v["id"].as_str() {
                let kill_req = make_request(
                    "DELETE",
                    &format!("/v1/terminals/{id}/kill"),
                    Some("tok"),
                    "{}",
                );
                let _ = door.route(&kill_req).await;
            }
        }
    }

    #[tokio::test]
    async fn route_read_returns_200() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let door = make_door("tok");

        // 先创建终端
        let create_body = serde_json::json!({
            "shell": cat,
            "working_dir": std::env::temp_dir().to_string_lossy().to_string(),
            "env": []
        })
        .to_string();
        let create_req = make_request("POST", "/v1/terminals", Some("tok"), &create_body);
        let create_resp = door.route(&create_req).await;
        let id = serde_json::from_str::<serde_json::Value>(&create_resp.body)
            .ok()
            .and_then(|v| v["id"].as_str().map(|s| s.to_string()))
            .expect("create should return id");

        // read
        let read_req = make_request("GET", &format!("/v1/terminals/{id}/read"), Some("tok"), "");
        let resp = door.route(&read_req).await;
        assert_eq!(resp.status, 200);

        // 清理
        let kill_req = make_request(
            "DELETE",
            &format!("/v1/terminals/{id}/kill"),
            Some("tok"),
            "{}",
        );
        let _ = door.route(&kill_req).await;
    }

    #[tokio::test]
    async fn route_send_returns_200() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let door = make_door("tok");

        let create_body = serde_json::json!({
            "shell": cat,
            "working_dir": std::env::temp_dir().to_string_lossy().to_string(),
            "env": []
        })
        .to_string();
        let create_req = make_request("POST", "/v1/terminals", Some("tok"), &create_body);
        let create_resp = door.route(&create_req).await;
        let id = serde_json::from_str::<serde_json::Value>(&create_resp.body)
            .ok()
            .and_then(|v| v["id"].as_str().map(|s| s.to_string()))
            .expect("create should return id");

        let send_body = serde_json::json!({
            "text": "hello",
            "press_enter": false,
            "caller_terminal_id": "other_term"
        })
        .to_string();
        let send_req = make_request(
            "POST",
            &format!("/v1/terminals/{id}/send"),
            Some("tok"),
            &send_body,
        );
        let resp = door.route(&send_req).await;
        assert_eq!(resp.status, 200);

        // 清理
        let kill_req = make_request(
            "DELETE",
            &format!("/v1/terminals/{id}/kill"),
            Some("tok"),
            "{}",
        );
        let _ = door.route(&kill_req).await;
    }

    #[tokio::test]
    async fn route_wait_returns_200() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let door = make_door("tok");

        let create_body = serde_json::json!({
            "shell": cat,
            "working_dir": std::env::temp_dir().to_string_lossy().to_string(),
            "env": []
        })
        .to_string();
        let create_req = make_request("POST", "/v1/terminals", Some("tok"), &create_body);
        let create_resp = door.route(&create_req).await;
        let id = serde_json::from_str::<serde_json::Value>(&create_resp.body)
            .ok()
            .and_then(|v| v["id"].as_str().map(|s| s.to_string()))
            .expect("create should return id");

        // wait with short timeout (timeout expected since no pattern)
        let wait_body = serde_json::json!({
            "mode": "pattern",
            "pattern": "never_matches_zzz",
            "timeout_ms": 100
        })
        .to_string();
        let wait_req = make_request(
            "POST",
            &format!("/v1/terminals/{id}/wait"),
            Some("tok"),
            &wait_body,
        );
        let resp = door.route(&wait_req).await;
        assert_eq!(resp.status, 200);
        assert!(resp.body.contains("timeout"));

        // 清理
        let kill_req = make_request(
            "DELETE",
            &format!("/v1/terminals/{id}/kill"),
            Some("tok"),
            "{}",
        );
        let _ = door.route(&kill_req).await;
    }

    #[tokio::test]
    async fn route_kill_returns_200() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let door = make_door("tok");

        let create_body = serde_json::json!({
            "shell": cat,
            "working_dir": std::env::temp_dir().to_string_lossy().to_string(),
            "env": []
        })
        .to_string();
        let create_req = make_request("POST", "/v1/terminals", Some("tok"), &create_body);
        let create_resp = door.route(&create_req).await;
        let id = serde_json::from_str::<serde_json::Value>(&create_resp.body)
            .ok()
            .and_then(|v| v["id"].as_str().map(|s| s.to_string()))
            .expect("create should return id");

        let kill_req = make_request(
            "DELETE",
            &format!("/v1/terminals/{id}/kill"),
            Some("tok"),
            r#"{"caller_is_agent":false}"#,
        );
        let resp = door.route(&kill_req).await;
        assert_eq!(resp.status, 200);
    }

    // === 状态码测试 ===

    #[tokio::test]
    async fn status_404_unknown_terminal_read() {
        let door = make_door("tok");
        let req = make_request("GET", "/v1/terminals/nonexistent/read", Some("tok"), "");
        let resp = door.route(&req).await;
        assert_eq!(resp.status, 404);
    }

    #[tokio::test]
    async fn status_404_unknown_terminal_send() {
        let door = make_door("tok");
        let body = r#"{"text":"hi","press_enter":false}"#;
        let req = make_request("POST", "/v1/terminals/nonexistent/send", Some("tok"), body);
        let resp = door.route(&req).await;
        assert_eq!(resp.status, 404);
    }

    #[tokio::test]
    async fn status_409_self_control_rejection() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let door = make_door("tok");

        let create_body = serde_json::json!({
            "shell": cat,
            "working_dir": std::env::temp_dir().to_string_lossy().to_string(),
            "env": []
        })
        .to_string();
        let create_req = make_request("POST", "/v1/terminals", Some("tok"), &create_body);
        let create_resp = door.route(&create_req).await;
        let id = serde_json::from_str::<serde_json::Value>(&create_resp.body)
            .ok()
            .and_then(|v| v["id"].as_str().map(|s| s.to_string()))
            .expect("create should return id");

        // self-control: caller == target
        let send_body = serde_json::json!({
            "text": "self",
            "press_enter": false,
            "caller_terminal_id": id
        })
        .to_string();
        let send_req = make_request(
            "POST",
            &format!("/v1/terminals/{id}/send"),
            Some("tok"),
            &send_body,
        );
        let resp = door.route(&send_req).await;
        assert_eq!(resp.status, 409);

        // 清理
        let kill_req = make_request(
            "DELETE",
            &format!("/v1/terminals/{id}/kill"),
            Some("tok"),
            "{}",
        );
        let _ = door.route(&kill_req).await;
    }

    #[tokio::test]
    async fn status_403_agent_kill() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let door = make_door("tok");

        let create_body = serde_json::json!({
            "shell": cat,
            "working_dir": std::env::temp_dir().to_string_lossy().to_string(),
            "env": []
        })
        .to_string();
        let create_req = make_request("POST", "/v1/terminals", Some("tok"), &create_body);
        let create_resp = door.route(&create_req).await;
        let id = serde_json::from_str::<serde_json::Value>(&create_resp.body)
            .ok()
            .and_then(|v| v["id"].as_str().map(|s| s.to_string()))
            .expect("create should return id");

        let kill_body = r#"{"caller_is_agent":true}"#;
        let kill_req = make_request(
            "DELETE",
            &format!("/v1/terminals/{id}/kill"),
            Some("tok"),
            kill_body,
        );
        let resp = door.route(&kill_req).await;
        assert_eq!(resp.status, 403);

        // 清理
        let cleanup_req = make_request(
            "DELETE",
            &format!("/v1/terminals/{id}/kill"),
            Some("tok"),
            r#"{"caller_is_agent":false}"#,
        );
        let _ = door.route(&cleanup_req).await;
    }

    #[tokio::test]
    async fn status_429_over_budget() {
        // max_sends_per_min = 0 -> 任何 send 都超预算
        let door = make_door_with_budget("tok", 0);
        let body = r#"{"text":"hi","press_enter":false}"#;
        let req = make_request("POST", "/v1/terminals/any/send", Some("tok"), body);
        let resp = door.route(&req).await;
        assert_eq!(resp.status, 429);
    }

    #[tokio::test]
    async fn status_501_unknown_route() {
        let door = make_door("tok");
        let req = make_request("GET", "/v1/unknown", Some("tok"), "");
        let resp = door.route(&req).await;
        assert_eq!(resp.status, 501);
    }

    #[tokio::test]
    async fn status_501_unknown_method() {
        let door = make_door("tok");
        let req = make_request("PUT", "/v1/terminals", Some("tok"), "");
        let resp = door.route(&req).await;
        assert_eq!(resp.status, 501);
    }

    #[tokio::test]
    async fn route_create_bad_body_returns_400() {
        let door = make_door("tok");
        let req = make_request("POST", "/v1/terminals", Some("tok"), "not json");
        let resp = door.route(&req).await;
        assert_eq!(resp.status, 400);
    }

    #[tokio::test]
    async fn route_kill_empty_body_defaults_to_non_agent() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let door = make_door("tok");

        let create_body = serde_json::json!({
            "shell": cat,
            "working_dir": std::env::temp_dir().to_string_lossy().to_string(),
            "env": []
        })
        .to_string();
        let create_req = make_request("POST", "/v1/terminals", Some("tok"), &create_body);
        let create_resp = door.route(&create_req).await;
        let id = serde_json::from_str::<serde_json::Value>(&create_resp.body)
            .ok()
            .and_then(|v| v["id"].as_str().map(|s| s.to_string()))
            .expect("create should return id");

        // empty body -> caller_is_agent = false -> should succeed
        let kill_req = make_request(
            "DELETE",
            &format!("/v1/terminals/{id}/kill"),
            Some("tok"),
            "",
        );
        let resp = door.route(&kill_req).await;
        assert_eq!(resp.status, 200);
    }

    #[test]
    fn parse_body_empty_returns_none() {
        let result: Option<CreateRequest> = parse_body("");
        assert!(result.is_none());
    }

    #[test]
    fn parse_body_valid_json() {
        let result: Option<CreateRequest> =
            parse_body(r#"{"shell":"sh","working_dir":"/tmp","env":[]}"#);
        assert!(result.is_some());
        let req = result.unwrap();
        assert_eq!(req.shell, "sh");
        assert_eq!(req.working_dir, "/tmp");
    }
}
