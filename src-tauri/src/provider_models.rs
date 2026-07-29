//! 模型目录发现。
//!
//! 设置页显式触发这条只读请求；访问密钥只用于构造请求头，不写日志、不落盘、
//! 也不随结果返回 WebView。OpenAI 兼容接口与 Anthropic Models API 都返回
//! `data[].id`，同时兼容少数网关使用的 `models`/字符串数组形状。

use std::collections::HashSet;
use std::time::Duration;

use reqwest::{Client, StatusCode, Url};
use serde::Serialize;
use serde_json::Value;

use crate::provider_catalog::{AuthStyle, Protocol};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_MODELS: usize = 2_000;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ProviderModelsResponse {
    pub models: Vec<String>,
}

/// 从 Provider 的模型目录端点读取可用模型。
pub async fn discover_models(
    base_url: &str,
    api_key: Option<&str>,
    protocol: Protocol,
    auth: AuthStyle,
) -> Result<ProviderModelsResponse, String> {
    let urls = model_list_urls(base_url, protocol)?;
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(6))
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("R-Code/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| "无法初始化模型目录请求".to_string())?;
    let key = api_key.map(str::trim).filter(|value| !value.is_empty());
    let mut retry_message = None;

    for (index, url) in urls.iter().enumerate() {
        let has_fallback = index + 1 < urls.len();
        let mut request = client.get(url.clone());
        if let Some(key) = key {
            request = match auth {
                AuthStyle::XApiKey => request.header("x-api-key", key),
                AuthStyle::Bearer => request.bearer_auth(key),
            };
        }
        if protocol == Protocol::AnthropicMessages {
            request = request.header("anthropic-version", ANTHROPIC_VERSION);
        }

        let response = request.send().await.map_err(sanitize_network_error)?;
        let status = response.status();
        if matches!(
            status,
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ) && has_fallback
        {
            retry_message = Some("服务未提供这个模型目录路径".to_string());
            continue;
        }
        if !status.is_success() {
            return Err(http_error(status));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
        {
            return Err("模型列表响应过大，已停止读取".to_string());
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| "读取模型列表响应失败".to_string())?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err("模型列表响应过大，已停止读取".to_string());
        }
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) if has_fallback => {
                retry_message = Some("模型目录返回了无法识别的数据".to_string());
                continue;
            }
            Err(_) => return Err("模型目录返回了无法识别的数据；仍可手动填写模型".to_string()),
        };
        let models = parse_model_ids(&value);
        if !models.is_empty() {
            return Ok(ProviderModelsResponse { models });
        }
        if has_fallback {
            retry_message = Some("服务返回了空的模型列表".to_string());
            continue;
        }
        return Err("服务返回了空的模型列表；仍可手动填写模型".to_string());
    }

    Err(format!(
        "{}；仍可手动填写模型",
        retry_message.unwrap_or_else(|| "服务未提供模型列表接口".to_string())
    ))
}

/// 模型目录通常与 completion endpoint 共用 API root。
///
/// OpenAI 兼容的裸域名一般走 `/v1/models`；DeepSeek 官方文档声明的是
/// `/models`，因此该域名先试无版本路径并保留 `/v1/models` 兜底。
fn model_list_urls(base_url: &str, protocol: Protocol) -> Result<Vec<Url>, String> {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("请先填写接口地址".to_string());
    }
    let base = Url::parse(trimmed).map_err(|_| "接口地址格式无效".to_string())?;
    if !matches!(base.scheme(), "http" | "https") {
        return Err("接口地址需要以 http:// 或 https:// 开头".to_string());
    }
    if !base.username().is_empty() || base.password().is_some() {
        return Err("接口地址不能包含用户名或密码".to_string());
    }

    if base.path().trim_end_matches('/').ends_with("/models") {
        return Ok(vec![base]);
    }

    let mut urls = Vec::new();
    match protocol {
        Protocol::AnthropicMessages => {
            let root = ensure_path_suffix(&base, "v1");
            urls.push(append_path(&root, "models"));
        }
        Protocol::OpenAiChat | Protocol::OpenAiResponses => {
            let has_path = !base.path().trim_matches('/').is_empty();
            if has_path {
                urls.push(append_path(&base, "models"));
            } else {
                let direct = append_path(&base, "models");
                let versioned = append_path(&ensure_path_suffix(&base, "v1"), "models");
                if base.host_str() == Some("api.deepseek.com") {
                    urls.extend([direct, versioned]);
                } else {
                    urls.extend([versioned, direct]);
                }
            }
        }
    }
    urls.dedup_by(|left, right| left.as_str() == right.as_str());
    Ok(urls)
}

fn append_path(base: &Url, segment: &str) -> Url {
    let mut url = base.clone();
    let path = base.path().trim_end_matches('/');
    url.set_path(&format!("{path}/{segment}"));
    url
}

fn ensure_path_suffix(base: &Url, segment: &str) -> Url {
    if base
        .path()
        .trim_end_matches('/')
        .ends_with(&format!("/{segment}"))
    {
        base.clone()
    } else {
        append_path(base, segment)
    }
}

fn parse_model_ids(value: &Value) -> Vec<String> {
    let items = value
        .get("data")
        .or_else(|| value.get("models"))
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut seen = HashSet::new();
    items
        .iter()
        .filter_map(|item| {
            item.as_str().or_else(|| {
                item.get("id")
                    .or_else(|| item.get("name"))
                    .or_else(|| item.get("model"))
                    .and_then(Value::as_str)
            })
        })
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .filter(|id| seen.insert((*id).to_string()))
        .take(MAX_MODELS)
        .map(str::to_string)
        .collect()
}

fn sanitize_network_error(error: reqwest::Error) -> String {
    if error.is_timeout() {
        "获取模型列表超时，请检查接口地址或网络".to_string()
    } else if error.is_connect() {
        "无法连接模型服务，请检查接口地址或网络".to_string()
    } else {
        "获取模型列表时发生网络错误".to_string()
    }
}

fn http_error(status: StatusCode) -> String {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            "模型服务鉴权失败，请检查访问密钥".to_string()
        }
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED => {
            "该服务未提供模型列表接口；仍可手动填写模型".to_string()
        }
        StatusCode::TOO_MANY_REQUESTS => "模型服务请求过于频繁，请稍后重试".to_string(),
        _ => format!("获取模型列表失败（HTTP {}）", status.as_u16()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn protocol_specific_model_urls_are_stable() {
        let openai =
            model_list_urls("https://api.openai.com/v1", Protocol::OpenAiResponses).unwrap();
        assert_eq!(openai[0].as_str(), "https://api.openai.com/v1/models");

        let anthropic =
            model_list_urls("https://api.anthropic.com", Protocol::AnthropicMessages).unwrap();
        assert_eq!(anthropic[0].as_str(), "https://api.anthropic.com/v1/models");

        let deepseek = model_list_urls("https://api.deepseek.com", Protocol::OpenAiChat).unwrap();
        assert_eq!(deepseek[0].as_str(), "https://api.deepseek.com/models");
        assert_eq!(deepseek[1].as_str(), "https://api.deepseek.com/v1/models");
    }

    #[test]
    fn model_parser_accepts_common_provider_shapes() {
        assert_eq!(
            parse_model_ids(&json!({"data": [{"id": "model-b"}, {"id": "model-a"}]})),
            vec!["model-b", "model-a"]
        );
        assert_eq!(
            parse_model_ids(&json!({"models": ["one", {"name": "two"}, "one"]})),
            vec!["one", "two"]
        );
    }

    #[test]
    fn model_urls_reject_embedded_credentials() {
        let error = model_list_urls("https://user:secret@example.com/v1", Protocol::OpenAiChat)
            .unwrap_err();
        assert!(error.contains("用户名或密码"));
        assert!(!error.contains("secret"));
    }
}
