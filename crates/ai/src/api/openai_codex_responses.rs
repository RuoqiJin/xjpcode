//! 对应上游 `packages/ai/src/api/openai-codex-responses.ts`（只接 SSE 这一半）。
//!
//! GPT 订阅（ChatGPT Plus/Pro）的请求：`POST {baseUrl}/codex/responses`，请求体用 zstd 压，
//! 事件流是 Responses 的 SSE。鉴权是 OAuth 的 access token；账号 id 从令牌里的
//! `chatgpt_account_id` 取出来放进 `chatgpt-account-id`。
//!
//! 与上游不同的：
//! - WebSocket 那条路（上游 909–1638 行）这一轮没有搬，`transport` 只按 SSE 走。
//! - HTTP 用 `reqwest`，SSE 切帧用 `utils/sse.rs`。
//! - 取消用 crate 自己的 `AbortSignal`。

use std::collections::BTreeSet;
use std::sync::Arc;

use base64::Engine;
use futures::StreamExt;
use serde_json::{json, Map, Value};

use crate::api::openai_responses_shared::{
    configuration_update_effort, convert_responses_messages, convert_responses_tools,
    create_output, declared_tools, get_responses_tool_support, process_responses_stream,
    resolve_reasoning_effort, ConvertResponsesMessagesOptions, ConvertResponsesToolsOptions,
    OpenAIResponsesStreamOptions,
};
use crate::auth::{ModelsError, ModelsErrorCode};
use crate::models::{calculate_cost, clamp_thinking_level};
use crate::providers::model_schema::{ModelThinkingLevel, ThinkingLevel};
use crate::types::{
    AssistantMessage, AssistantMessageEvent, Model, ProviderEnv, ProviderHeaders,
    ProviderResponse, SimpleStreamOptions, StopReason, StreamOptions, TranscriptContext,
};
use crate::utils::abort::AbortSignal;
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::identity::{get_identity_headers, merge_client_identity};
use crate::utils::sse::SseDecoder;
use crate::utils::transcript::{resolve_transcript, resolve_transcript_tools};

const DEFAULT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";
const DEFAULT_MAX_RETRIES: u32 = 0;
const BASE_DELAY_MS: u64 = 1000;
const REQUEST_COMPRESSION_ZSTD_LEVEL: i32 = 3;
const CODEX_TOOL_CALL_PROVIDERS: [&str; 3] = ["openai", "openai-codex", "opencode"];
/// Responses 接口不接受小于 16 的 `max_output_tokens`。
const MIN_OUTPUT_TOKENS: u64 = 16;
/// 用 `configuration_update` 项报思考档时，`reasoning.effort` 固定写这个。
const CONFIGURATION_UPDATE_REQUEST_EFFORT: &str = "medium";

/// pi: `OpenAICodexResponsesOptions extends StreamOptions`
#[derive(Debug, Clone, Default)]
pub struct OpenAICodexResponsesOptions {
    pub api_key: Option<String>,
    pub env: Option<ProviderEnv>,
    pub on_payload: crate::types::Callback<crate::types::OnPayloadFn>,
    pub on_response: crate::types::Callback<crate::types::OnResponseFn>,
    pub on_provider_stream_event: crate::types::Callback<crate::types::OnProviderStreamEventFn>,
    pub headers: Option<ProviderHeaders>,
    pub identity: Option<crate::utils::identity::ClientIdentity>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
    pub temperature: Option<f64>,
    pub sampling_params: Option<crate::types::SamplingParams>,
    pub max_tokens: Option<u64>,
    pub transport: Option<crate::types::Transport>,
    pub cache_retention: Option<crate::providers::model_schema::CacheRetention>,
    pub session_id: Option<String>,
    pub disable_continuation: Option<bool>,
    pub websocket_connect_timeout_ms: Option<u64>,
    pub metadata: Option<std::collections::BTreeMap<String, Value>>,
    // 自有字段
    pub reasoning_effort: Option<ModelThinkingLevel>,
    pub reasoning_summary: Option<String>,
    pub text_verbosity: Option<String>,
    pub tool_choice: Option<String>,
}

impl OpenAICodexResponsesOptions {
    pub fn from_stream_options(options: StreamOptions) -> Self {
        Self {
            api_key: options.api_key,
            env: options.env,
            on_payload: options.on_payload,
            on_response: options.on_response,
            on_provider_stream_event: options.on_provider_stream_event,
            headers: options.headers,
            identity: options.identity,
            timeout_ms: options.timeout_ms,
            max_retries: options.max_retries,
            max_retry_delay_ms: options.max_retry_delay_ms,
            temperature: options.temperature,
            sampling_params: options.sampling_params,
            max_tokens: options.max_tokens,
            transport: options.transport,
            cache_retention: options.cache_retention,
            session_id: options.session_id,
            disable_continuation: options.disable_continuation,
            websocket_connect_timeout_ms: options.websocket_connect_timeout_ms,
            metadata: options.metadata,
            ..Self::default()
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

fn error_message(error: impl std::fmt::Display) -> ModelsError {
    ModelsError::new(ModelsErrorCode::Stream, error.to_string())
}

/// 上游 `getConfigurationUpdateEffort`：模型支持 `configuration_update` 时返回要写进项的 effort。
fn get_configuration_update_effort(
    model: &Model,
    options: Option<&OpenAICodexResponsesOptions>,
) -> Option<String> {
    let supports_mid_convo_effort = model
        .compat
        .as_ref()
        .and_then(|compat| compat.supports_mid_convo_effort)
        .unwrap_or(false);
    if !supports_mid_convo_effort {
        return None;
    }
    let effort = options.and_then(|options| options.reasoning_effort)?;
    Some(resolve_reasoning_effort(model, effort))
}

/// pi: `resolveCodexUrl`
pub fn resolve_codex_url(base_url: Option<&str>) -> String {
    let raw = base_url
        .filter(|base_url| !base_url.trim().is_empty())
        .unwrap_or(DEFAULT_CODEX_BASE_URL);
    let normalized = raw.trim_end_matches('/');
    if normalized.ends_with("/codex/responses") {
        normalized.to_string()
    } else if normalized.ends_with("/codex") {
        format!("{normalized}/responses")
    } else {
        format!("{normalized}/codex/responses")
    }
}

/// pi: `extractAccountId`——从 JWT 的 `https://api.openai.com/auth` 声明里取 `chatgpt_account_id`。
pub fn extract_account_id(token: &str) -> Result<String, ModelsError> {
    let mut parts = token.split('.');
    let (Some(_header), Some(payload), Some(_signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(error_message("Failed to extract accountId from token"));
    };
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(payload))
        .map_err(|_| error_message("Failed to extract accountId from token"))?;
    let json: Value = serde_json::from_slice(&decoded)
        .map_err(|_| error_message("Failed to extract accountId from token"))?;
    json.get(JWT_CLAIM_PATH)
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .filter(|account_id| !account_id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| error_message("Failed to extract accountId from token"))
}

fn build_request_body(
    model: &Model,
    context: &TranscriptContext,
    options: Option<&OpenAICodexResponsesOptions>,
    cache_session_id: Option<&str>,
    update_effort: Option<&str>,
) -> Value {
    let supports_strict_mode = model
        .compat
        .as_ref()
        .and_then(|compat| compat.supports_strict_mode)
        .unwrap_or(true);
    let supports_grammar_tools = model
        .compat
        .as_ref()
        .and_then(|compat| compat.supports_open_ai_grammar_tools)
        .unwrap_or(false);
    let supports_additional_tools = model
        .compat
        .as_ref()
        .and_then(|compat| compat.supports_additional_tools)
        .unwrap_or(false);
    let supports_tool_search = model
        .compat
        .as_ref()
        .and_then(|compat| compat.supports_tool_search)
        .unwrap_or(false);
    let supports_mid_convo_system_messages = model
        .compat
        .as_ref()
        .and_then(|compat| compat.supports_mid_convo_system_messages)
        .unwrap_or(false);
    let transcript_tools = resolve_transcript_tools(
        &context.messages,
        get_responses_tool_support(
            supports_mid_convo_system_messages,
            supports_additional_tools,
            supports_tool_search,
        ),
    );
    let allowed: BTreeSet<String> = CODEX_TOOL_CALL_PROVIDERS
        .iter()
        .map(|provider| provider.to_string())
        .collect();
    let input = convert_responses_messages(
        model,
        context,
        &allowed,
        Some(&ConvertResponsesMessagesOptions {
            supports_mid_convo_system_messages,
            supports_additional_tools,
            supports_tool_search,
            tool_options: Some(ConvertResponsesToolsOptions {
                strict: None,
                supports_strict_mode,
                supports_open_ai_grammar_tools: supports_grammar_tools,
            }),
            configuration_update_effort: update_effort.map(str::to_string),
        }),
    );

    let mut body = Map::new();
    body.insert("model".to_string(), json!(model.id));
    body.insert("store".to_string(), json!(false));
    body.insert("stream".to_string(), json!(true));
    body.insert("input".to_string(), Value::Array(input));
    body.insert(
        "text".to_string(),
        json!({ "verbosity": options.and_then(|options| options.text_verbosity.clone()).unwrap_or_else(|| "low".to_string()) }),
    );
    body.insert(
        "include".to_string(),
        json!(["reasoning.encrypted_content"]),
    );
    if let Some(session_id) = cache_session_id {
        body.insert("prompt_cache_key".to_string(), json!(session_id));
    }
    body.insert(
        "tool_choice".to_string(),
        json!(options
            .and_then(|options| options.tool_choice.clone())
            .unwrap_or_else(|| "auto".to_string())),
    );
    body.insert("parallel_tool_calls".to_string(), json!(true));

    if let Some(max_tokens) = options.and_then(|options| options.max_tokens) {
        body.insert(
            "max_output_tokens".to_string(),
            json!(max_tokens.max(MIN_OUTPUT_TOKENS)),
        );
    }
    if let Some(temperature) = options.and_then(|options| options.temperature) {
        body.insert("temperature".to_string(), json!(temperature));
    }
    if !transcript_tools.request_tools.is_empty() {
        body.insert(
            "tools".to_string(),
            Value::Array(convert_responses_tools(
                &transcript_tools.request_tools,
                Some(&ConvertResponsesToolsOptions {
                    strict: None,
                    supports_strict_mode,
                    supports_open_ai_grammar_tools: supports_grammar_tools,
                }),
            )),
        );
    }

    let reasoning_summary = options
        .and_then(|options| options.reasoning_summary.clone())
        .unwrap_or_else(|| "auto".to_string());
    match options.and_then(|options| options.reasoning_effort) {
        Some(level) => {
            let effort = if update_effort.is_some() {
                CONFIGURATION_UPDATE_REQUEST_EFFORT.to_string()
            } else {
                resolve_reasoning_effort(model, level)
            };
            body.insert(
                "reasoning".to_string(),
                json!({ "effort": effort, "summary": reasoning_summary }),
            );
        }
        None => {
            let off_mapped = model
                .thinking_level_map
                .as_ref()
                .map(|map| map.contains_key(&ModelThinkingLevel::Off))
                .unwrap_or(false);
            let off_is_null = model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(&ModelThinkingLevel::Off))
                .map(|value| value.is_none())
                .unwrap_or(false);
            if model.reasoning && !off_is_null {
                let _ = off_mapped;
                body.insert(
                    "reasoning".to_string(),
                    json!({ "effort": resolve_reasoning_effort(model, ModelThinkingLevel::Off) }),
                );
            }
        }
    }
    Value::Object(body)
}

fn is_terminal_rate_limit_error(error_text: &str) -> bool {
    let lower = error_text.to_lowercase();
    [
        "go usagelimit error",
        "freeusagelimit error",
        "monthly usage limit reached",
        "available balance",
        "insufficient_quota",
        "out of budget",
        "quota exceeded",
        "billing",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || error_text.contains("GoUsageLimitError")
        || error_text.contains("FreeUsageLimitError")
}

fn is_retryable_error(status: u16, error_text: &str) -> bool {
    if status == 429 && is_terminal_rate_limit_error(error_text) {
        return false;
    }
    if matches!(status, 429 | 500 | 502 | 503 | 504) {
        return true;
    }
    let lower = error_text.to_lowercase();
    lower.contains("rate limit")
        || lower.contains("rate_limit")
        || lower.contains("overloaded")
        || lower.contains("service unavailable")
        || lower.contains("upstream connect")
        || lower.contains("connection refused")
}

fn build_base_headers(
    defaults: &[(&str, &str)],
    identity_headers: &std::collections::BTreeMap<String, String>,
    additional_headers: Option<&ProviderHeaders>,
    account_id: &str,
    token: &str,
) -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
    let mut headers = HeaderMap::new();
    for (key, value) in defaults {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(key.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
    for (key, value) in identity_headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(key.to_lowercase().as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
    for (key, value) in additional_headers.unwrap_or(&ProviderHeaders::new()) {
        let Ok(name) = HeaderName::from_bytes(key.to_lowercase().as_bytes()) else {
            continue;
        };
        match value {
            None => {
                headers.remove(name);
            }
            Some(value) => {
                if let Ok(value) = HeaderValue::from_str(value) {
                    headers.insert(name, value);
                }
            }
        }
    }
    if let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}")) {
        headers.insert("authorization", value);
    }
    if let Ok(value) = HeaderValue::from_str(account_id) {
        headers.insert("chatgpt-account-id", value);
    }
    headers
}

fn build_sse_headers(
    identity_headers: &std::collections::BTreeMap<String, String>,
    additional_headers: Option<&ProviderHeaders>,
    account_id: &str,
    token: &str,
    session_id: Option<&str>,
) -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderValue, ACCEPT, CONTENT_TYPE};
    let mut headers = build_base_headers(
        &[("originator", "pi")],
        identity_headers,
        additional_headers,
        account_id,
        token,
    );
    headers.insert("openai-beta", HeaderValue::from_static("responses=experimental"));
    headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Some(session_id) = session_id {
        if let Ok(value) = HeaderValue::from_str(session_id) {
            headers.insert("session-id", value.clone());
            headers.insert("x-client-request-id", value);
        }
    }
    headers
}

async fn parse_error_response(status: u16, raw: &str, status_text: &str) -> (String, Option<String>) {
    let mut message = if raw.is_empty() {
        status_text.to_string()
    } else {
        raw.to_string()
    };
    let mut friendly = None;
    if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
        if let Some(error) = parsed.get("error") {
            let code = error
                .get("code")
                .and_then(Value::as_str)
                .or_else(|| error.get("type").and_then(Value::as_str))
                .unwrap_or("");
            let lower = code.to_lowercase();
            if lower.contains("usage_limit_reached")
                || lower.contains("usage_not_included")
                || lower.contains("rate_limit_exceeded")
                || status == 429
            {
                let plan = error
                    .get("plan_type")
                    .and_then(Value::as_str)
                    .map(|plan| format!(" ({} plan)", plan.to_lowercase()))
                    .unwrap_or_default();
                let resets_at = error.get("resets_at").and_then(Value::as_f64);
                let minutes = resets_at.map(|resets_at| {
                    let now = now_ms() as f64;
                    (((resets_at * 1000.0 - now) / 60000.0).round()).max(0.0) as i64
                });
                let when = minutes
                    .map(|minutes| format!(" Try again in ~{minutes} min."))
                    .unwrap_or_default();
                friendly = Some(
                    format!("You have hit your ChatGPT usage limit{plan}.{when}")
                        .trim()
                        .to_string(),
                );
            }
            if let Some(error_message) = error.get("message").and_then(Value::as_str) {
                message = error_message.to_string();
            } else if let Some(friendly) = &friendly {
                message = friendly.clone();
            }
        }
    }
    (message, friendly)
}

/// pi: `stream`
pub fn stream(
    model: Model,
    context: TranscriptContext,
    options: Option<OpenAICodexResponsesOptions>,
    signal: Option<AbortSignal>,
) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::new();
    let task_stream = stream.clone();
    tokio::spawn(async move {
        run_stream(model, context, options, signal, task_stream).await;
    });
    stream
}

async fn run_stream(
    model: Model,
    context: TranscriptContext,
    options: Option<OpenAICodexResponsesOptions>,
    signal: Option<AbortSignal>,
    stream: AssistantMessageEventStream,
) {
    let mut output = create_output(&model);
    let update_effort = get_configuration_update_effort(&model, options.as_ref());
    if let Some(effort) = &update_effort {
        output.provider_thinking_level = Some(effort.clone());
    }

    let result: Result<(), ModelsError> = async {
        let api_key = options
            .as_ref()
            .and_then(|options| options.api_key.clone())
            .filter(|api_key| !api_key.is_empty())
            .ok_or_else(|| error_message(format!("No API key for provider: {}", model.provider)))?;
        let account_id = extract_account_id(&api_key)?;

        let normalized_context = resolve_transcript(&context, Some(
            model
                .compat
                .as_ref()
                .and_then(|compat| compat.supports_mid_convo_system_messages)
                .unwrap_or(false),
        ));
        let cache_session_id = match options
            .as_ref()
            .and_then(|options| options.cache_retention)
        {
            Some(crate::providers::model_schema::CacheRetention::None) => None,
            _ => options
                .as_ref()
                .and_then(|options| options.session_id.clone()),
        };
        let mut body = build_request_body(
            &model,
            &normalized_context,
            options.as_ref(),
            cache_session_id.as_deref(),
            update_effort.as_deref(),
        );
        if let Some(on_payload) = options
            .as_ref()
            .and_then(|options| options.on_payload.arc())
        {
            if let Some(next) = on_payload(body.clone(), &model).await {
                body = next;
            }
        }

        let identity = merge_client_identity(&[options
            .as_ref()
            .and_then(|options| options.identity.as_ref())]);
        let identity_headers = get_identity_headers(&model, identity.as_ref());
        let headers = build_sse_headers(
            &identity_headers,
            options.as_ref().and_then(|options| options.headers.as_ref()),
            &account_id,
            &api_key,
            cache_session_id.as_deref(),
        );

        let body_json = body.to_string();
        let compressed = zstd::bulk::compress(
            body_json.as_bytes(),
            REQUEST_COMPRESSION_ZSTD_LEVEL,
        )
        .ok();
        let (body_bytes, content_encoding) = match compressed {
            Some(compressed) => (compressed, Some("zstd")),
            None => (body_json.clone().into_bytes(), None),
        };

        let client = crate::api::openai_completions::shared_http_client();
        let request_timeout = options
            .as_ref()
            .and_then(|options| options.timeout_ms)
            .map(std::time::Duration::from_millis);

        let max_retries = options
            .as_ref()
            .and_then(|options| options.max_retries)
            .unwrap_or(DEFAULT_MAX_RETRIES);
        let mut response: Option<reqwest::Response> = None;
        let mut last_error: Option<ModelsError> = None;
        for attempt in 0..=max_retries {
            if signal.as_ref().is_some_and(|signal| signal.aborted()) {
                return Err(error_message("Request was aborted"));
            }
            let mut request = client
                .post(resolve_codex_url(Some(&model.base_url)))
                .headers(headers.clone())
                .body(body_bytes.clone());
            if let Some(timeout) = request_timeout {
                request = request.timeout(timeout);
            }
            if let Some(content_encoding) = content_encoding {
                request = request.header("content-encoding", content_encoding);
            }
            match request.send().await {
                Ok(next) => {
                    if let Some(on_response) =
                        options.as_ref().and_then(|options| options.on_response.arc())
                    {
                        let recorded = ProviderResponse {
                            status: next.status().as_u16(),
                            headers: next
                                .headers()
                                .iter()
                                .map(|(key, value)| {
                                    (
                                        key.as_str().to_string(),
                                        value.to_str().unwrap_or_default().to_string(),
                                    )
                                })
                                .collect(),
                        };
                        on_response(&recorded, &model).await;
                    }
                    if next.status().is_success() {
                        response = Some(next);
                        break;
                    }
                    let status = next.status().as_u16();
                    let status_text = next.status().canonical_reason().unwrap_or("").to_string();
                    let raw = next.text().await.unwrap_or_default();
                    if attempt < max_retries && is_retryable_error(status, &raw) {
                        last_error = Some(error_message(format!("{status}: {raw}")));
                        tokio::time::sleep(std::time::Duration::from_millis(
                            BASE_DELAY_MS * 2u64.pow(attempt),
                        ))
                        .await;
                        continue;
                    }
                    let (message, friendly) =
                        parse_error_response(status, &raw, &status_text).await;
                    return Err(error_message(friendly.unwrap_or(message)));
                }
                Err(error) => {
                    last_error = Some(error_message(error));
                    if let Some(signal) = &signal {
                        if signal.aborted() {
                            return Err(error_message("Request was aborted"));
                        }
                    }
                    if attempt < max_retries {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            BASE_DELAY_MS * 2u64.pow(attempt),
                        ))
                        .await;
                        continue;
                    }
                }
            }
        }
        let response = match response {
            Some(response) => response,
            None => {
                return Err(last_error.unwrap_or_else(|| error_message("Failed after retries")))
            }
        };

        stream.push(AssistantMessageEvent::Start {
            partial: output.clone(),
        });

        // 读 SSE：切帧、解 JSON、交给事件处理。
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let reader_stream = stream.clone();
        let reader_signal = signal.clone();
        let reader_model = model.clone();
        let reader_on_event = options
            .as_ref()
            .and_then(|options| options.on_provider_stream_event.arc());
        let reader = tokio::spawn(async move {
            let mut decoder = SseDecoder::new();
            let mut bytes = response.bytes_stream();
            loop {
                if let Some(signal) = &reader_signal {
                    if signal.aborted() {
                        let _ = tx.send(Err(error_message("Request was aborted")));
                        return;
                    }
                }
                let chunk = match bytes.next().await {
                    Some(Ok(chunk)) => chunk,
                    Some(Err(error)) => {
                        let _ = tx.send(Err(error_message(error)));
                        return;
                    }
                    None => break,
                };
                for event in decoder.push_bytes(&chunk) {
                    let data = event.data.trim();
                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }
                    let parsed: Result<Value, _> = serde_json::from_str(data);
                    match parsed {
                        Ok(value) => {
                            if let Some(callback) = &reader_on_event {
                                callback(&value, &reader_model).await;
                            }
                            if tx.send(Ok(value)).is_err() {
                                return;
                            }
                        }
                        Err(error) => {
                            let _ = tx.send(Err(error_message(format!(
                                "Invalid Codex SSE JSON: {error}"
                            ))));
                            return;
                        }
                    }
                }
            }
            for event in decoder.finish() {
                let data = event.data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(data) {
                    if let Some(callback) = &reader_on_event {
                        callback(&value, &reader_model).await;
                    }
                    if tx.send(Ok(value)).is_err() {
                        return;
                    }
                }
            }
            let _ = &reader_stream;
        });

        let options_for_events = OpenAIResponsesStreamOptions {
            apply_service_tier_pricing: None,
        };
        let processed =
            process_responses_stream(rx, &mut output, &stream, &model, &options_for_events).await;
        // 收到结束事件就收工（上游 mapCodexEvents 也在这里 return）：
        // 不能等连接关，有的服务发完结束事件还挂着这条连接。
        reader.abort();
        let _ = reader.await;
        processed?;

        if signal.as_ref().is_some_and(|signal| signal.aborted()) {
            return Err(error_message("Request was aborted"));
        }
        match output.stop_reason {
            StopReason::Pending => {
                return Err(error_message("Codex stream ended without a stop reason"))
            }
            StopReason::Error | StopReason::Aborted => {
                return Err(error_message(
                    output
                        .error_message
                        .clone()
                        .unwrap_or_else(|| "An unknown error occurred".to_string()),
                ))
            }
            _ => {}
        }
        let reason = output.stop_reason;
        stream.push(AssistantMessageEvent::Done {
            reason,
            message: output.clone(),
        });
        Ok(())
    }
    .await;

    if let Err(error) = result {
        output.stop_reason = if signal.as_ref().is_some_and(|signal| signal.aborted()) {
            StopReason::Aborted
        } else {
            StopReason::Error
        };
        output.error_message = Some(error.message.clone());
        stream.push(AssistantMessageEvent::Error {
            reason: output.stop_reason,
            error: output,
        });
    }
    stream.end(None);
}

/// pi: `streamSimple`
pub fn stream_simple(
    model: Model,
    context: TranscriptContext,
    options: Option<SimpleStreamOptions>,
    signal: Option<AbortSignal>,
) -> AssistantMessageEventStream {
    let api_key = options
        .as_ref()
        .and_then(|options| options.api_key.clone())
        .filter(|key| !key.is_empty());
    let Some(api_key) = api_key else {
        let stream = AssistantMessageEventStream::new();
        let mut output = create_output(&model);
        output.stop_reason = StopReason::Error;
        output.error_message = Some(format!("No API key for provider: {}", model.provider));
        stream.push(AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: output,
        });
        stream.end(None);
        return stream;
    };
    let base = crate::api::simple_options::build_base_options(
        &model,
        &context,
        options.as_ref(),
        Some(&api_key),
    );
    let clamped_reasoning = options
        .as_ref()
        .and_then(|options| options.reasoning)
        .map(|reasoning| {
            clamp_thinking_level(
                &model,
                crate::api::simple_options::thinking_level_to_model_level(reasoning),
            )
        });
    let reasoning_effort = match clamped_reasoning {
        Some(level) if level != ModelThinkingLevel::Off => Some(level),
        _ if model.reasoning => Some(ModelThinkingLevel::Off),
        _ => None,
    };
    let openai_options = OpenAICodexResponsesOptions {
        tool_choice: options
            .as_ref()
            .and_then(|options| options.tool_choice)
            .map(|choice| match choice {
                crate::types::ToolChoice::Auto => "auto".to_string(),
                crate::types::ToolChoice::None => "none".to_string(),
            }),
        reasoning_effort,
        ..OpenAICodexResponsesOptions::from_stream_options(base)
    };
    stream(model, context, Some(openai_options), signal)
}

/// 时间戳与成本计算的公开钩子（上游 `processResponsesStream` 里调 `calculateCost` 与 `Date.now`）。
pub fn finish_cost(model: &Model, output: &mut AssistantMessage) {
    calculate_cost(model, &mut output.usage);
}

/// 让别的模块知道这是一条 Responses 路径。
pub fn is_codex_api(api: &str) -> bool {
    api == "openai-codex-responses"
}

/// 记号：请求里用到的工具清单（测试读它，避免重复实现）。
pub fn declared_tool_names(context: &TranscriptContext) -> Vec<String> {
    declared_tools(&context.messages)
        .into_iter()
        .map(|tool| tool.name)
        .collect()
}

/// 思考档换 effort 的公开入口（测试用）。
pub fn effort_for(model: &Model, level: ThinkingLevel) -> String {
    resolve_reasoning_effort(model, level.into())
}

/// 这一条路需要的 provider 兼容字段（缺省值照上游）。
pub fn supports_mid_convo_effort(model: &Model) -> bool {
    model
        .compat
        .as_ref()
        .and_then(|compat| compat.supports_mid_convo_effort)
        .unwrap_or(false)
}

/// 事件处理里用到的 effort 计算（公开给测试）。
pub fn configuration_update_for(
    model: &Model,
    reasoning: Option<ThinkingLevel>,
) -> Option<String> {
    configuration_update_effort(model, reasoning, supports_mid_convo_effort(model))
}

/// 一个不会被压缩的请求体（`content-encoding: zstd` 不可用时的退路）。
pub fn body_without_compression(body: &Value) -> Vec<u8> {
    body.to_string().into_bytes()
}

/// 把一份请求头摊成记录（测试比较用）。
pub fn header_pairs(headers: &reqwest::header::HeaderMap) -> std::collections::BTreeMap<String, String> {
    headers
        .iter()
        .map(|(key, value)| {
            (
                key.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// 记着这个模块里没有搬的东西，避免以后误以为接上了。
pub const UNPORTED: &str = "WebSocket transport (openai-codex-responses.ts 909-1638) is not ported";

/// reqwest 的 `StatusCode` 归一（测试）。
pub fn status_is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

/// 一个空的 header 表（用来给 `build_sse_headers` 的测试传参）。
pub fn empty_headers() -> ProviderHeaders {
    ProviderHeaders::new()
}

/// `Arc` 一条请求头表的便利构造。
pub fn shared_headers(headers: ProviderHeaders) -> Arc<ProviderHeaders> {
    Arc::new(headers)
}
