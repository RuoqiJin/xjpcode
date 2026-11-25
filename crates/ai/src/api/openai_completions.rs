//! 对应上游 `packages/ai/src/api/openai-completions.ts`。
//!
//! 这一条链：拼请求 → 发 HTTP → 增量读 SSE → 把每个 chunk 归成 `AssistantMessageEvent` →
//! 收尾（finish_reason、用量、错误、取消）。DeepSeek 官方接口就走这一条（`providers/deepseek.ts`）。
//!
//! 与上游不同的地方（汇总见 `PORTING.md` 差异表 #110–#115）：
//! - HTTP 与 SSE 由 `reqwest` 与 `utils/sse.rs` 自己做（上游由 `openai` 包代劳）。
//! - 只接 DeepSeek 与「未知 OpenAI 兼容端点」走到的分支；其余 provider 的思考格式、grammar
//!   工具、`reasoning_details`、prompt cache、重试与注入的 `fetch` 没有接。
//! - `partial` 按值放进事件（里程碑一的决定），不是上游那个继续增长的同一个对象。

use std::collections::BTreeMap;
use std::sync::OnceLock;

use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::models::{calculate_cost, clamp_thinking_level};
use crate::providers::compat_schema::{
    CacheControlFormat, MaxTokensField, PromptCacheKeyMode, SessionAffinityFormat, ThinkingFormat,
    ThinkingTokenBudgetField,
};
use crate::providers::model_schema::{ModelInputModality, ModelThinkingLevel, ThinkingLevel};
use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, JsonObject,
    Message, Model, ProviderEnv, ProviderHeaders, SamplingParams, SimpleStreamOptions, StopReason,
    StreamOptions, TextContent, ThinkingBudgets, ThinkingContent, Tool, ToolCall, ToolChoice,
    ToolResultContentBlock, TranscriptContext, Usage, UsageCost, UserContent, UserContentBlock,
};
use crate::utils::abort::AbortSignal;
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::error_body::{format_provider_error, http_error, plain_error, transport_error};
use crate::utils::identity::get_identity_headers;
use crate::utils::sse::SseDecoder;
use crate::utils::streaming_json::ToolCallArgumentsStream;
use crate::utils::text::{get_system_message_text, render_system_message_update};
use crate::utils::transcript::{resolve_transcript, resolve_transcript_tools, TranscriptToolSupport};

/// pi: `OpenAICompletionsOptions extends StreamOptions`
///
/// 字段重复是 Rust 没有接口继承的代价（差异表 #19）。
#[derive(Debug, Clone, Default)]
pub struct OpenAICompletionsOptions {
    // 来自 StreamOptions（含 ProviderRequestOptions）
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
    pub sampling_params: Option<SamplingParams>,
    pub max_tokens: Option<u64>,
    pub transport: Option<crate::types::Transport>,
    pub cache_retention: Option<crate::providers::model_schema::CacheRetention>,
    pub session_id: Option<String>,
    pub disable_continuation: Option<bool>,
    pub websocket_connect_timeout_ms: Option<u64>,
    pub metadata: Option<BTreeMap<String, Value>>,
    // 自有字段
    pub tool_choice: Option<ToolChoice>,
    pub reasoning_effort: Option<ThinkingLevel>,
    pub thinking_budgets: Option<ThinkingBudgets>,
}

impl OpenAICompletionsOptions {
    /// 把 `StreamOptions` 的字段铺进来，行为等价于上游的 `extends`。
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

/// 上游 `ResolvedOpenAICompletionsCompat`：`getCompat` 之后每一项都有确定的取值。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedOpenAICompletionsCompat {
    pub supports_store: bool,
    pub supports_developer_role: bool,
    pub supports_reasoning_effort: bool,
    pub supports_usage_in_streaming: bool,
    pub supports_finish_reason: bool,
    pub max_tokens_field: MaxTokensField,
    pub requires_tool_result_name: bool,
    pub requires_assistant_after_tool_result: bool,
    pub requires_non_null_assistant_content: bool,
    pub requires_tools_with_tool_history: bool,
    pub requires_thinking_as_text: bool,
    pub requires_reasoning_content_on_assistant_messages: bool,
    pub thinking_format: ThinkingFormat,
    pub zai_tool_stream: bool,
    pub supports_thinking_token_budget: bool,
    pub thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
    pub supports_strict_mode: bool,
    pub supports_open_ai_grammar_tools: bool,
    pub supports_mid_convo_system_messages: bool,
    pub supports_mid_convo_tool_additions: bool,
    pub cache_control_format: Option<CacheControlFormat>,
    pub send_session_affinity_headers: bool,
    pub session_affinity_format: SessionAffinityFormat,
    pub supports_long_cache_retention: bool,
    pub prompt_cache_key_mode: Option<PromptCacheKeyMode>,
    pub vllm_priority: Option<f64>,
}

/// pi: `detectCompat`——按 provider 名与 baseUrl 猜一家的兼容设置。
pub fn detect_compat(model: &Model) -> ResolvedOpenAICompletionsCompat {
    let provider = model.provider.as_str();
    let base_url = model.base_url.as_str();
    let lower_base_url = base_url.to_lowercase();

    let is_zai = provider == "zai"
        || provider == "zai-coding-cn"
        || base_url.contains("api.z.ai")
        || base_url.contains("open.bigmodel.cn");
    let is_together = provider == "together"
        || base_url.contains("api.together.ai")
        || base_url.contains("api.together.xyz");
    let is_moonshot =
        provider == "moonshotai" || provider == "moonshotai-cn" || base_url.contains("api.moonshot.");
    let is_openrouter = provider == "openrouter" || base_url.contains("openrouter.ai");
    let is_cloudflare_workers_ai =
        provider == "cloudflare-workers-ai" || base_url.contains("api.cloudflare.com");
    let is_cloudflare_ai_gateway =
        provider == "cloudflare-ai-gateway" || base_url.contains("gateway.ai.cloudflare.com");
    let is_nvidia = provider == "nvidia" || base_url.contains("integrate.api.nvidia.com");
    let is_ant_ling = provider == "ant-ling" || base_url.contains("api.ant-ling.com");
    let is_cerebras = provider == "cerebras" || base_url.contains("cerebras.ai");
    let is_deepseek = provider == "deepseek" || lower_base_url.contains("deepseek.com");

    let is_non_standard = is_nvidia
        || is_cerebras
        || provider == "xai"
        || base_url.contains("api.x.ai")
        || is_together
        || base_url.contains("chutes.ai")
        || is_deepseek
        || is_zai
        || is_moonshot
        || provider == "opencode"
        || base_url.contains("opencode.ai")
        || is_cloudflare_workers_ai
        || is_cloudflare_ai_gateway
        || is_ant_ling;

    let use_max_tokens = base_url.contains("chutes.ai")
        || is_deepseek
        || is_moonshot
        || is_cloudflare_ai_gateway
        || is_together
        || is_nvidia
        || is_ant_ling
        || is_zai;

    let is_grok = provider == "xai" || base_url.contains("api.x.ai");
    let is_openai = provider == "openai" || base_url.contains("api.openai.com");
    let cache_control_format =
        if provider == "openrouter" && model.id.starts_with("anthropic/") {
            Some(CacheControlFormat::Anthropic)
        } else {
            None
        };

    ResolvedOpenAICompletionsCompat {
        supports_store: !is_non_standard,
        supports_developer_role: is_openai,
        supports_reasoning_effort: !is_grok
            && !is_zai
            && !is_moonshot
            && !is_together
            && !is_cloudflare_ai_gateway
            && !is_nvidia
            && !is_ant_ling,
        supports_usage_in_streaming: true,
        supports_finish_reason: true,
        max_tokens_field: if use_max_tokens {
            MaxTokensField::MaxTokens
        } else {
            MaxTokensField::MaxCompletionTokens
        },
        requires_tool_result_name: false,
        requires_assistant_after_tool_result: false,
        requires_non_null_assistant_content: false,
        requires_tools_with_tool_history: false,
        requires_thinking_as_text: false,
        requires_reasoning_content_on_assistant_messages: is_deepseek,
        thinking_format: if is_deepseek {
            ThinkingFormat::Deepseek
        } else if is_zai {
            ThinkingFormat::Zai
        } else if is_together {
            ThinkingFormat::Together
        } else if is_ant_ling {
            ThinkingFormat::AntLing
        } else if is_openrouter {
            ThinkingFormat::Openrouter
        } else {
            ThinkingFormat::Openai
        },
        zai_tool_stream: false,
        supports_thinking_token_budget: false,
        thinking_token_budget_field: None,
        // OpenAI 兼容本身不代表支持 strict JSON-schema 工具。
        supports_strict_mode: false,
        supports_open_ai_grammar_tools: false,
        supports_mid_convo_system_messages: false,
        supports_mid_convo_tool_additions: false,
        cache_control_format,
        send_session_affinity_headers: is_openrouter,
        session_affinity_format: if is_openrouter {
            SessionAffinityFormat::Openrouter
        } else {
            SessionAffinityFormat::Openai
        },
        supports_long_cache_retention: !(is_together
            || is_cloudflare_workers_ai
            || is_cloudflare_ai_gateway
            || is_nvidia
            || is_ant_ling),
        prompt_cache_key_mode: None,
        vllm_priority: None,
    }
}

/// pi: `getCompat`——自动判断打底，`model.compat` 逐项覆盖。
pub fn get_compat(model: &Model) -> ResolvedOpenAICompletionsCompat {
    let detected = detect_compat(model);
    let Some(compat) = &model.compat else {
        return detected;
    };
    let pick = |overridden: Option<bool>, detected: bool| overridden.unwrap_or(detected);
    ResolvedOpenAICompletionsCompat {
        supports_store: pick(compat.supports_store, detected.supports_store),
        supports_developer_role: pick(compat.supports_developer_role, detected.supports_developer_role),
        supports_reasoning_effort: pick(
            compat.supports_reasoning_effort,
            detected.supports_reasoning_effort,
        ),
        supports_usage_in_streaming: pick(
            compat.supports_usage_in_streaming,
            detected.supports_usage_in_streaming,
        ),
        supports_finish_reason: pick(compat.supports_finish_reason, detected.supports_finish_reason),
        max_tokens_field: compat.max_tokens_field.unwrap_or(detected.max_tokens_field),
        requires_tool_result_name: pick(
            compat.requires_tool_result_name,
            detected.requires_tool_result_name,
        ),
        requires_assistant_after_tool_result: pick(
            compat.requires_assistant_after_tool_result,
            detected.requires_assistant_after_tool_result,
        ),
        requires_non_null_assistant_content: pick(
            compat.requires_non_null_assistant_content,
            detected.requires_non_null_assistant_content,
        ),
        requires_tools_with_tool_history: pick(
            compat.requires_tools_with_tool_history,
            detected.requires_tools_with_tool_history,
        ),
        requires_thinking_as_text: pick(
            compat.requires_thinking_as_text,
            detected.requires_thinking_as_text,
        ),
        requires_reasoning_content_on_assistant_messages: pick(
            compat.requires_reasoning_content_on_assistant_messages,
            detected.requires_reasoning_content_on_assistant_messages,
        ),
        thinking_format: compat.thinking_format.unwrap_or(detected.thinking_format),
        zai_tool_stream: pick(compat.zai_tool_stream, detected.zai_tool_stream),
        supports_thinking_token_budget: pick(
            compat.supports_thinking_token_budget,
            detected.supports_thinking_token_budget,
        ),
        thinking_token_budget_field: compat
            .thinking_token_budget_field
            .or(detected.thinking_token_budget_field),
        supports_strict_mode: pick(compat.supports_strict_mode, detected.supports_strict_mode),
        supports_open_ai_grammar_tools: pick(
            compat.supports_open_ai_grammar_tools,
            detected.supports_open_ai_grammar_tools,
        ),
        supports_mid_convo_system_messages: pick(
            compat.supports_mid_convo_system_messages,
            detected.supports_mid_convo_system_messages,
        ),
        supports_mid_convo_tool_additions: pick(
            compat.supports_mid_convo_tool_additions,
            detected.supports_mid_convo_tool_additions,
        ),
        cache_control_format: compat.cache_control_format.or(detected.cache_control_format),
        send_session_affinity_headers: pick(
            compat.send_session_affinity_headers,
            detected.send_session_affinity_headers,
        ),
        session_affinity_format: compat
            .session_affinity_format
            .unwrap_or(detected.session_affinity_format),
        supports_long_cache_retention: pick(
            compat.supports_long_cache_retention,
            detected.supports_long_cache_retention,
        ),
        prompt_cache_key_mode: compat.prompt_cache_key_mode.or(detected.prompt_cache_key_mode),
        vllm_priority: compat.vllm_priority.or(detected.vllm_priority),
    }
}

/// pi: `hasHeader`
fn has_header(headers: &Option<ProviderHeaders>, name: &str) -> bool {
    let Some(headers) = headers else {
        return false;
    };
    let expected = name.to_lowercase();
    headers.iter().any(|(key, value)| {
        key.to_lowercase() == expected
            && value
                .as_ref()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
    })
}

/// pi: `getClientApiKey`
fn get_client_api_key(
    provider: &str,
    api_key: Option<&str>,
    headers: &Option<ProviderHeaders>,
) -> Result<String, String> {
    if let Some(api_key) = api_key {
        return Ok(api_key.to_string());
    }
    if has_header(headers, "authorization") || has_header(headers, "cf-aig-authorization") {
        return Ok("unused".to_string());
    }
    Err(format!("No API key for provider: {provider}"))
}

/// pi: `hasToolHistory`
fn has_tool_history(messages: &[Message]) -> bool {
    messages.iter().any(|message| match message {
        Message::ToolResult(_) => true,
        Message::Assistant(assistant) => assistant
            .content
            .iter()
            .any(|block| matches!(block, AssistantContentBlock::ToolCall(_))),
        _ => false,
    })
}

/// 用 JS 的 `JSON.stringify` 写工具参数：键序与数字写法都要与上游一致。
fn stringify_json(value: &Value) -> String {
    xjpcode_schema::js::stringify(value)
}

/// 上游到处用的 `Date.now()`：Unix 毫秒。
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// pi: `convertTools` 中的 function 分支。
///
/// grammar 工具没有接（DeepSeek 不用，见差异表 #110）；`strict` 与严格 schema 改写走
/// `api::constrained_sampling`。
pub fn convert_tools(
    tools: &[Tool],
    compat: &ResolvedOpenAICompletionsCompat,
) -> Result<Vec<Value>, String> {
    let mut converted: Vec<Value> = Vec::new();
    for tool in tools {
        let (parameters, strict) =
            crate::api::constrained_sampling::strict_tool_parameters(tool, compat.supports_strict_mode)?;
        let mut function = serde_json::Map::new();
        function.insert("name".to_string(), Value::String(tool.name.clone()));
        function.insert(
            "description".to_string(),
            Value::String(tool.description.clone()),
        );
        function.insert("parameters".to_string(), parameters);
        if compat.supports_strict_mode {
            function.insert("strict".to_string(), Value::Bool(strict));
        }
        let mut declaration = serde_json::Map::new();
        declaration.insert("type".to_string(), Value::String("function".to_string()));
        declaration.insert("function".to_string(), Value::Object(function));
        converted.push(Value::Object(declaration));
    }
    Ok(converted)
}

/// pi: `convertMessages` 里的 `normalizeToolCallId`。
fn normalize_chat_completions_tool_call_id(id: &str, model: &Model) -> String {
    fn sanitize(part: &str) -> String {
        part.chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                    ch
                } else {
                    '_'
                }
            })
            .collect()
    }
    if let Some((call_id, item_id)) = id.split_once('|') {
        let call = sanitize(call_id);
        let item = sanitize(item_id);
        let combined = if item.is_empty() {
            call
        } else {
            format!("{call}_{item}")
        };
        if combined.chars().count() <= 40 {
            return combined;
        }
        let hash: String = crate::utils::hash::short_hash(id).chars().take(8).collect();
        let prefix_len = 40usize.saturating_sub(hash.len() + 1).max(1);
        let prefix: String = call_id
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                    ch
                } else {
                    '_'
                }
            })
            .take(prefix_len)
            .collect();
        return format!("{prefix}_{hash}");
    }
    if model.provider == "openai" && id.chars().count() > 40 {
        return id.chars().take(40).collect();
    }
    id.to_string()
}

/// pi: `convertMessages`。
pub fn convert_messages(
    model: &Model,
    context: &TranscriptContext,
    compat: &ResolvedOpenAICompletionsCompat,
) -> Vec<Value> {
    let normalized_context = resolve_transcript(context, Some(compat.supports_mid_convo_system_messages));
    let mut params: Vec<Value> = Vec::new();
    // pi: `normalizeToolCallId`——把 Responses 那种 `call_id|item_id` 收成 Chat Completions 认的 id。
    // 同一条回合里的多个调用可能 share call_id、只差 item_id，所以要保留 item 这一层。
    let normalize = |id: &str, target: &Model, _source: &crate::types::AssistantMessage| {
        normalize_chat_completions_tool_call_id(id, target)
    };
    let transformed_messages = crate::api::transform_messages::transform_messages(
        &normalized_context.messages,
        model,
        Some(&normalize),
    );
    let transcript_tools = resolve_transcript_tools(
        &normalized_context.messages,
        if compat.supports_mid_convo_system_messages && compat.supports_mid_convo_tool_additions {
            TranscriptToolSupport::Additions
        } else {
            TranscriptToolSupport::None
        },
    );
    let instruction_role = if model.reasoning && compat.supports_developer_role {
        "developer"
    } else {
        "system"
    };

    let mut last_role: Option<&str> = None;
    let mut system_index = 0usize;

    let mut index = 0usize;
    while index < transformed_messages.len() {
        let message = &transformed_messages[index];
        // 有些 provider 不接受工具结果后面紧跟 user 消息：插一条合成的 assistant 消息。
        if compat.requires_assistant_after_tool_result
            && last_role == Some("toolResult")
            && message.role() == "user"
        {
            params.push(json!({"role": "assistant", "content": "I have processed the tool results."}));
        }

        match message {
            Message::System(system) => {
                let added_tools = if system_index >= transcript_tools.folded_system_messages {
                    system.tools_added.clone().unwrap_or_default()
                } else {
                    Vec::new()
                };
                system_index += 1;
                if !added_tools.is_empty() {
                    if let Ok(tools) = convert_tools(&added_tools, compat) {
                        params.push(json!({"role": "system", "tools": tools}));
                    }
                }
                let text = if index == 0 {
                    get_system_message_text(system)
                } else {
                    render_system_message_update(system)
                };
                if !text.is_empty() {
                    params.push(json!({"role": instruction_role, "content": text}));
                }
            }
            Message::User(user) => match &user.content {
                UserContent::Text(text) => {
                    params.push(json!({"role": "user", "content": text}));
                }
                UserContent::Blocks(blocks) => {
                    let content: Vec<Value> = blocks
                        .iter()
                        .filter(|block| match block {
                            UserContentBlock::Text(text) => !text.text.is_empty(),
                            UserContentBlock::Image(_) => true,
                        })
                        .map(|block| match block {
                            UserContentBlock::Text(text) => json!({"type": "text", "text": text.text}),
                            UserContentBlock::Image(image) => json!({
                                "type": "image_url",
                                "image_url": {"url": format!("data:{};base64,{}", image.mime_type, image.data)},
                            }),
                        })
                        .collect();
                    if content.is_empty() {
                        last_role = Some("user");
                        index += 1;
                        continue;
                    }
                    params.push(json!({"role": "user", "content": content}));
                }
            },
            Message::Assistant(assistant) => {
                let mut assistant_message = JsonObject::new();
                assistant_message.insert("role".to_string(), json!("assistant"));
                // 有些 provider 不接受 null content。
                let empty_content = if compat.requires_assistant_after_tool_result
                    || compat.requires_non_null_assistant_content
                {
                    json!("")
                } else {
                    Value::Null
                };
                assistant_message.insert("content".to_string(), empty_content);

                let assistant_text_parts: Vec<String> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::Text(text) if !text.text.trim().is_empty() => {
                            Some(text.text.clone())
                        }
                        _ => None,
                    })
                    .collect();
                let assistant_text = assistant_text_parts.join("");

                let thinking_blocks: Vec<&ThinkingContent> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::Thinking(thinking) => Some(thinking),
                        _ => None,
                    })
                    .collect();
                let tool_calls: Vec<&ToolCall> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::ToolCall(tool_call) => Some(tool_call),
                        _ => None,
                    })
                    .collect();

                let non_empty_thinking: Vec<&&ThinkingContent> = thinking_blocks
                    .iter()
                    .filter(|block| !block.thinking.trim().is_empty())
                    .collect();
                if !non_empty_thinking.is_empty() {
                    if compat.requires_thinking_as_text {
                        let thinking_text = non_empty_thinking
                            .iter()
                            .map(|block| block.thinking.clone())
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        let mut content = vec![json!({"type": "text", "text": thinking_text})];
                        content.extend(assistant_text_parts.iter().map(|text| json!({"type": "text", "text": text})));
                        assistant_message.insert("content".to_string(), Value::Array(content));
                    } else {
                        if !assistant_text.is_empty() {
                            assistant_message.insert("content".to_string(), json!(assistant_text));
                        }
                        // 思考签名字段名（`reasoning_content` 等）原样带回。
                        let signature = non_empty_thinking[0].thinking_signature.clone();
                        if let Some(signature) = signature {
                            if matches!(
                                signature.as_str(),
                                "reasoning" | "reasoning_content" | "reasoning_text"
                            ) {
                                let joined = non_empty_thinking
                                    .iter()
                                    .map(|block| block.thinking.clone())
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                assistant_message.insert(signature, json!(joined));
                            }
                        }
                    }
                } else if !assistant_text.is_empty() {
                    assistant_message.insert("content".to_string(), json!(assistant_text));
                }

                if !tool_calls.is_empty() {
                    let calls: Vec<Value> = tool_calls
                        .iter()
                        .map(|tool_call| {
                            json!({
                                "id": tool_call.id,
                                "type": "function",
                                "function": {
                                    "name": tool_call.name,
                                    "arguments": stringify_json(&Value::Object(tool_call.arguments.clone())),
                                },
                            })
                        })
                        .collect();
                    assistant_message.insert("tool_calls".to_string(), Value::Array(calls));
                }
                if compat.requires_reasoning_content_on_assistant_messages
                    && model.reasoning
                    && !assistant_message.contains_key("reasoning_content")
                {
                    assistant_message.insert("reasoning_content".to_string(), json!(""));
                }

                let has_content = match assistant_message.get("content") {
                    Some(Value::String(text)) => !text.is_empty(),
                    Some(Value::Array(items)) => !items.is_empty(),
                    Some(Value::Null) | None => false,
                    Some(_) => true,
                };
                if !has_content && !assistant_message.contains_key("tool_calls") {
                    last_role = Some("assistant");
                    index += 1;
                    continue;
                }
                params.push(Value::Object(assistant_message));
            }
            Message::ToolResult(_) => {
                let mut image_blocks: Vec<Value> = Vec::new();
                let mut cursor = index;
                while cursor < transformed_messages.len()
                    && transformed_messages[cursor].role() == "toolResult"
                {
                    let Message::ToolResult(tool_result) = &transformed_messages[cursor] else {
                        break;
                    };
                    let text_result = tool_result
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            ToolResultContentBlock::Text(text) => Some(text.text.clone()),
                            ToolResultContentBlock::Image(_) => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let has_images = tool_result
                        .content
                        .iter()
                        .any(|block| matches!(block, ToolResultContentBlock::Image(_)));
                    let has_text = !text_result.trim().is_empty();
                    let tool_result_text = if has_text {
                        text_result
                    } else if has_images {
                        "(see attached image)".to_string()
                    } else {
                        "(no tool output)".to_string()
                    };
                    let mut tool_message = JsonObject::new();
                    tool_message.insert("role".to_string(), json!("tool"));
                    tool_message.insert("content".to_string(), json!(tool_result_text));
                    tool_message.insert("tool_call_id".to_string(), json!(tool_result.tool_call_id));
                    if compat.requires_tool_result_name && !tool_result.tool_name.is_empty() {
                        tool_message.insert("name".to_string(), json!(tool_result.tool_name));
                    }
                    params.push(Value::Object(tool_message));
                    if has_images && model.input.contains(&ModelInputModality::Image) {
                        for block in &tool_result.content {
                            if let ToolResultContentBlock::Image(image) = block {
                                image_blocks.push(json!({
                                    "type": "image_url",
                                    "image_url": {"url": format!("data:{};base64,{}", image.mime_type, image.data)},
                                }));
                            }
                        }
                    }
                    cursor += 1;
                }
                index = cursor;
                if image_blocks.is_empty() {
                    last_role = Some("toolResult");
                } else {
                    if compat.requires_assistant_after_tool_result {
                        params.push(json!({"role": "assistant", "content": "I have processed the tool results."}));
                    }
                    let mut content = vec![json!({"type": "text", "text": "Attached image(s) from tool result:"})];
                    content.extend(image_blocks);
                    params.push(json!({"role": "user", "content": content}));
                    last_role = Some("user");
                }
                continue;
            }
        }

        last_role = Some(message.role());
        index += 1;
    }

    params
}

/// pi: `buildParams`。只接 DeepSeek 与未知 OpenAI 兼容端点的分支（差异表 #110）。
pub fn build_params(
    model: &Model,
    context: &TranscriptContext,
    options: Option<&OpenAICompletionsOptions>,
    compat: &ResolvedOpenAICompletionsCompat,
) -> Result<JsonObject, String> {
    let messages = convert_messages(model, context, compat);
    let mut params = JsonObject::new();
    params.insert("model".to_string(), json!(model.id));
    params.insert("messages".to_string(), Value::Array(messages));
    params.insert("stream".to_string(), json!(true));

    if compat.supports_usage_in_streaming {
        params.insert("stream_options".to_string(), json!({"include_usage": true}));
    }
    if compat.supports_store {
        params.insert("store".to_string(), json!(false));
    }
    if let Some(max_tokens) = options.and_then(|options| options.max_tokens) {
        match compat.max_tokens_field {
            MaxTokensField::MaxTokens => {
                params.insert("max_tokens".to_string(), json!(max_tokens));
            }
            MaxTokensField::MaxCompletionTokens => {
                params.insert("max_completion_tokens".to_string(), json!(max_tokens));
            }
        }
    }
    if let Some(temperature) = options.and_then(|options| options.temperature) {
        params.insert("temperature".to_string(), json!(temperature));
    }

    let transcript_tools = resolve_transcript_tools(
        &resolve_transcript(context, Some(compat.supports_mid_convo_system_messages)).messages,
        TranscriptToolSupport::None,
    );
    if !transcript_tools.request_tools.is_empty() {
        let tools = convert_tools(&transcript_tools.request_tools, compat)?;
        params.insert("tools".to_string(), Value::Array(tools));
        if compat.zai_tool_stream {
            params.insert("tool_stream".to_string(), json!(true));
        }
    } else if compat.requires_tools_with_tool_history && has_tool_history(&context.messages) {
        params.insert("tools".to_string(), json!([]));
    }
    if let Some(tool_choice) = options.and_then(|options| options.tool_choice) {
        params.insert("tool_choice".to_string(), json!(tool_choice));
    }
    if let Some(priority) = compat.vllm_priority {
        params.insert("priority".to_string(), json!(priority));
    }

    let reasoning_effort = options.and_then(|options| options.reasoning_effort);
    let thinking_token_budget_field = compat
        .thinking_token_budget_field
        .or(if compat.supports_thinking_token_budget {
            Some(ThinkingTokenBudgetField::ThinkingTokenBudget)
        } else {
            None
        });
    let thinking_budget = resolve_clamped_thinking_budget(model, options, &params);

    if model.reasoning {
        match compat.thinking_format {
            ThinkingFormat::Deepseek => {
                let off_is_not_null = model
                    .thinking_level_map
                    .as_ref()
                    .and_then(|map| map.get(&ModelThinkingLevel::Off))
                    .map(|mapped| mapped.is_some())
                    .unwrap_or(true);
                if reasoning_effort.is_some() {
                    params.insert("thinking".to_string(), json!({"type": "enabled"}));
                } else if off_is_not_null {
                    params.insert("thinking".to_string(), json!({"type": "disabled"}));
                }
                if let (Some(effort), true) = (reasoning_effort, compat.supports_reasoning_effort) {
                    let mapped = model
                        .thinking_level_map
                        .as_ref()
                        .and_then(|map| map.get(&crate::api::simple_options::thinking_level_to_model_level(effort)))
                        .cloned()
                        .flatten();
                    params.insert(
                        "reasoning_effort".to_string(),
                        json!(mapped.unwrap_or_else(|| thinking_level_name(effort).to_string())),
                    );
                }
            }
            ThinkingFormat::Openai => {
                if let Some(effort) = reasoning_effort {
                    if compat.supports_reasoning_effort {
                        let mapped = model
                            .thinking_level_map
                            .as_ref()
                            .and_then(|map| map.get(&crate::api::simple_options::thinking_level_to_model_level(effort)))
                            .cloned()
                            .flatten();
                        params.insert(
                            "reasoning_effort".to_string(),
                            json!(mapped.unwrap_or_else(|| thinking_level_name(effort).to_string())),
                        );
                    }
                } else if model.reasoning && compat.supports_reasoning_effort {
                    if let Some(Some(off_value)) = model
                        .thinking_level_map
                        .as_ref()
                        .and_then(|map| map.get(&ModelThinkingLevel::Off))
                    {
                        params.insert("reasoning_effort".to_string(), json!(off_value));
                    }
                }
            }
            // 其余 provider 的思考格式本次没有接（差异表 #110）：请求里不写思考字段。
            _ => {}
        }
    }

    if let (Some(field), Some(budget)) = (thinking_token_budget_field, thinking_budget) {
        params.insert(field.field_name().to_string(), json!(budget));
    }

    // 最后一步：模型的采样参数覆盖上面写过的同名字段（上游同样是 `Object.assign`）。
    if let Some(sampling_params) = crate::api::simple_options::resolve_sampling_params_for_request(
        model,
        reasoning_effort,
        options.and_then(|options| options.sampling_params.as_ref()),
    ) {
        for (key, value) in sampling_params {
            params.insert(key, value);
        }
    }

    Ok(params)
}

/// 上游的 `options?.reasoningEffort` 字符串名。
fn thinking_level_name(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

/// pi: `resolveClampedThinkingBudget`
fn resolve_clamped_thinking_budget(
    model: &Model,
    options: Option<&OpenAICompletionsOptions>,
    params: &JsonObject,
) -> Option<u64> {
    let effort = options?.reasoning_effort?;
    if !model.reasoning {
        return None;
    }
    let ceiling = params
        .get("max_tokens")
        .or(params.get("max_completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(model.max_tokens);
    let budget = crate::api::simple_options::clamp_thinking_budget_to_answer_room(
        crate::api::simple_options::thinking_budget_for_level(
            effort,
            options?.thinking_budgets.as_ref(),
        ),
        ceiling,
    );
    (budget > 0).then_some(budget)
}

// ---------------------------------------------------------------------------
// 流式组装
// ---------------------------------------------------------------------------

/// SSE 里一个 `chat.completion.chunk` 用到的字段（其余忽略）。
#[derive(Debug, Default, Deserialize)]
struct Chunk {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<Value>,
    #[serde(default)]
    choices: Option<Vec<Choice>>,
}

#[derive(Debug, Default, Deserialize)]
struct Choice {
    #[serde(default)]
    delta: Option<Delta>,
    #[serde(default)]
    finish_reason: Option<Value>,
    #[serde(default)]
    usage: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
struct Delta {
    #[serde(default)]
    content: Option<Value>,
    #[serde(default)]
    reasoning_content: Option<Value>,
    #[serde(default)]
    reasoning: Option<Value>,
    #[serde(default)]
    reasoning_text: Option<Value>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Debug, Default, Deserialize)]
struct ToolCallDelta {
    #[serde(default)]
    index: Option<u64>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FunctionDelta>,
}

#[derive(Debug, Default, Deserialize)]
struct FunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// 一条流式响应的组装状态。
///
/// 上游把这些状态放在 `stream()` 的闭包里；Rust 侧抽成结构体，测试可以直接喂 chunk。
pub struct ChatCompletionsStreamState {
    model: Model,
    compat: ResolvedOpenAICompletionsCompat,
    /// 正在组装的 assistant 消息。
    pub output: AssistantMessage,
    text_block: Option<usize>,
    thinking_block: Option<usize>,
    /// 工具调用块：stream index → content index，id → content index。
    tool_call_by_index: BTreeMap<u64, usize>,
    tool_call_by_id: BTreeMap<String, usize>,
    /// content index → 该工具调用正在拼的参数。
    tool_arguments: BTreeMap<usize, ToolCallArgumentsStream>,
    has_finish_reason: bool,
}

impl ChatCompletionsStreamState {
    pub fn new(model: Model) -> Self {
        let compat = get_compat(&model);
        let output = AssistantMessage {
            role: AssistantRole::Assistant,
            content: Vec::new(),
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                cache_write_1h: None,
                reasoning: None,
                total_tokens: 0,
                cost: UsageCost::default(),
            },
            stop_reason: StopReason::Pending,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: now_ms(),
            duration_ms: None,
        };
        Self {
            model,
            compat,
            output,
            text_block: None,
            thinking_block: None,
            tool_call_by_index: BTreeMap::new(),
            tool_call_by_id: BTreeMap::new(),
            tool_arguments: BTreeMap::new(),
            has_finish_reason: false,
        }
    }

    /// 当前那条半成品消息。
    pub fn message(&self) -> &AssistantMessage {
        &self.output
    }

    pub fn has_finish_reason(&self) -> bool {
        self.has_finish_reason
    }

    fn ensure_text_block(&mut self, events: &mut Vec<AssistantMessageEvent>) -> usize {
        if let Some(index) = self.text_block {
            return index;
        }
        self.output.content.push(AssistantContentBlock::Text(TextContent::new("")));
        let index = self.output.content.len() - 1;
        self.text_block = Some(index);
        events.push(AssistantMessageEvent::TextStart {
            content_index: index,
            partial: self.output.clone(),
        });
        index
    }

    fn ensure_thinking_block(&mut self, signature: &str, events: &mut Vec<AssistantMessageEvent>) -> usize {
        if let Some(index) = self.thinking_block {
            return index;
        }
        self.output.content.push(AssistantContentBlock::Thinking(ThinkingContent {
            r#type: crate::types::ThinkingContentType::Thinking,
            thinking: String::new(),
            thinking_signature: if signature.is_empty() {
                None
            } else {
                Some(signature.to_string())
            },
            redacted: None,
        }));
        let index = self.output.content.len() - 1;
        self.thinking_block = Some(index);
        events.push(AssistantMessageEvent::ThinkingStart {
            content_index: index,
            partial: self.output.clone(),
        });
        index
    }

    /// 按上游的 `ensureToolCallBlock`：先按 index 找，再按 id 找，都没有才新建。
    fn ensure_tool_call_block(
        &mut self,
        delta: &ToolCallDelta,
        events: &mut Vec<AssistantMessageEvent>,
    ) -> usize {
        let name = delta
            .function
            .as_ref()
            .and_then(|function| function.name.clone())
            .unwrap_or_default();
        let mut index = delta
            .index
            .and_then(|stream_index| self.tool_call_by_index.get(&stream_index).copied());
        if index.is_none() {
            if let Some(id) = delta.id.as_ref() {
                index = self.tool_call_by_id.get(id).copied();
            }
        }
        let index = match index {
            Some(index) => index,
            None => {
                self.output.content.push(AssistantContentBlock::ToolCall(ToolCall {
                    r#type: crate::types::ToolCallType::ToolCall,
                    id: delta.id.clone().unwrap_or_default(),
                    name: name.clone(),
                    arguments: JsonObject::new(),
                    thought_signature: None,
                    namespace: None,
                    arguments_error: None,
                }));
                let index = self.output.content.len() - 1;
                self.tool_arguments.insert(index, ToolCallArgumentsStream::new());
                if let Some(stream_index) = delta.index {
                    self.tool_call_by_index.insert(stream_index, index);
                }
                if let Some(id) = delta.id.as_ref() {
                    self.tool_call_by_id.insert(id.clone(), index);
                }
                events.push(AssistantMessageEvent::ToolcallStart {
                    content_index: index,
                    partial: self.output.clone(),
                });
                index
            }
        };
        if let Some(stream_index) = delta.index {
            self.tool_call_by_index.insert(stream_index, index);
        }
        if let Some(id) = delta.id.as_ref() {
            self.tool_call_by_id.insert(id.clone(), index);
        }
        if let Some(AssistantContentBlock::ToolCall(tool_call)) = self.output.content.get_mut(index) {
            if tool_call.id.is_empty() {
                if let Some(id) = delta.id.as_ref() {
                    tool_call.id = id.clone();
                }
            }
            if tool_call.name.is_empty() && !name.is_empty() {
                tool_call.name = name;
            }
        }
        index
    }

    /// 吃一个 chunk（已经是 JSON 值）。返回它产生的那些事件。
    pub fn apply_chunk(&mut self, chunk: &Value) -> Vec<AssistantMessageEvent> {
        let mut events = Vec::new();
        let Ok(chunk) = serde_json::from_value::<Chunk>(chunk.clone()) else {
            return events;
        };

        if self.output.response_id.is_none() {
            self.output.response_id = chunk.id.filter(|id| !id.is_empty());
        }
        if let Some(response_model) = chunk.model.filter(|model| !model.is_empty()) {
            if response_model != self.model.id && self.output.response_model.is_none() {
                self.output.response_model = Some(response_model);
            }
        }
        if let Some(usage) = &chunk.usage {
            self.output.usage = self.parse_usage(usage);
        }
        let Some(choice) = chunk.choices.and_then(|mut choices| {
            if choices.is_empty() {
                None
            } else {
                Some(choices.remove(0))
            }
        }) else {
            return events;
        };
        // 有些 provider（Moonshot）把用量放在 choice.usage 上。
        if chunk.usage.is_none() {
            if let Some(usage) = &choice.usage {
                self.output.usage = self.parse_usage(usage);
            }
        }
        if let Some(Value::String(reason)) = &choice.finish_reason {
            if !reason.is_empty() {
                self.output.raw_stop_reason = Some(reason.clone());
                let (stop_reason, error_message) = map_stop_reason(reason);
                self.output.stop_reason = stop_reason;
                if let Some(error_message) = error_message {
                    self.output.error_message = Some(error_message);
                }
                self.has_finish_reason = true;
            }
        }

        if let Some(delta) = &choice.delta {
            // 思考先于正文：有些代理把思考与回答放在同一个 delta 里。
            // 只认第一个非空的字段，避免同一段思考被记两遍。
            let mut reasoning_field: Option<&'static str> = None;
            for (field, value) in [
                ("reasoning_content", &delta.reasoning_content),
                ("reasoning", &delta.reasoning),
                ("reasoning_text", &delta.reasoning_text),
            ] {
                if let Some(Value::String(text)) = value {
                    if !text.is_empty() {
                        reasoning_field = Some(field);
                        break;
                    }
                }
            }
            if let Some(field) = reasoning_field {
                let text = match field {
                    "reasoning_content" => delta.reasoning_content.clone(),
                    "reasoning" => delta.reasoning.clone(),
                    _ => delta.reasoning_text.clone(),
                };
                if let Some(Value::String(text)) = text {
                    if !text.is_empty() {
                        let index = self.ensure_thinking_block(field, &mut events);
                        if let Some(AssistantContentBlock::Thinking(block)) =
                            self.output.content.get_mut(index)
                        {
                            block.thinking.push_str(&text);
                        }
                        events.push(AssistantMessageEvent::ThinkingDelta {
                            content_index: index,
                            delta: text,
                            partial: self.output.clone(),
                        });
                    }
                }
            }

            if let Some(Value::String(text)) = &delta.content {
                if !text.is_empty() {
                    let index = self.ensure_text_block(&mut events);
                    if let Some(AssistantContentBlock::Text(block)) = self.output.content.get_mut(index) {
                        block.text.push_str(text);
                    }
                    events.push(AssistantMessageEvent::TextDelta {
                        content_index: index,
                        delta: text.clone(),
                        partial: self.output.clone(),
                    });
                }
            }

            if let Some(tool_calls) = &delta.tool_calls {
                for tool_call in tool_calls {
                    let index = self.ensure_tool_call_block(tool_call, &mut events);
                    let mut appended = String::new();
                    if let Some(arguments) = tool_call
                        .function
                        .as_ref()
                        .and_then(|function| function.arguments.as_ref())
                    {
                        if !arguments.is_empty() {
                            if let Some(stream) = self.tool_arguments.get_mut(&index) {
                                let updated = stream.append(arguments);
                                if let Some(AssistantContentBlock::ToolCall(block)) =
                                    self.output.content.get_mut(index)
                                {
                                    block.arguments = (*updated).clone();
                                }
                            }
                            appended = arguments.clone();
                        }
                    }
                    events.push(AssistantMessageEvent::ToolcallDelta {
                        content_index: index,
                        delta: appended,
                        partial: self.output.clone(),
                    });
                }
            }
        }

        events
    }

    /// 收尾每个打开的块：`text_end` / `thinking_end` / `toolcall_end`。
    ///
    /// 顺序按内容块的位置走（与上游遍历 `blocks` 相同）；工具调用先收参数流。
    pub fn finish_blocks(&mut self) -> Vec<AssistantMessageEvent> {
        let mut events = Vec::new();
        for index in 0..self.output.content.len() {
            match &self.output.content[index] {
                AssistantContentBlock::Text(text) => {
                    events.push(AssistantMessageEvent::TextEnd {
                        content_index: index,
                        content: text.text.clone(),
                        partial: self.output.clone(),
                    });
                }
                AssistantContentBlock::Thinking(thinking) => {
                    events.push(AssistantMessageEvent::ThinkingEnd {
                        content_index: index,
                        content: thinking.thinking.clone(),
                        partial: self.output.clone(),
                    });
                }
                AssistantContentBlock::ToolCall(_) => {
                    if let Some(stream) = self.tool_arguments.remove(&index) {
                        let parsed = stream.finish();
                        if let Some(AssistantContentBlock::ToolCall(block)) =
                            self.output.content.get_mut(index)
                        {
                            block.arguments = parsed.arguments;
                            block.arguments_error = parsed.arguments_error;
                        }
                    }
                    if let Some(AssistantContentBlock::ToolCall(tool_call)) =
                        self.output.content.get(index)
                    {
                        events.push(AssistantMessageEvent::ToolcallEnd {
                            content_index: index,
                            tool_call: tool_call.clone(),
                            partial: self.output.clone(),
                        });
                    }
                }
            }
        }
        events
    }

    /// pi: `parseChunkUsage`
    fn parse_usage(&self, raw: &Value) -> Usage {
        let number = |value: Option<&Value>| value.and_then(Value::as_f64).unwrap_or(0.0) as u64;
        let prompt_tokens = number(raw.get("prompt_tokens"));
        let cache_read_tokens = number(
            raw.get("prompt_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .or_else(|| raw.get("prompt_cache_hit_tokens"))
                .or_else(|| raw.get("cached_tokens")),
        );
        let cache_write_tokens = number(
            raw.get("prompt_tokens_details")
                .and_then(|details| details.get("cache_write_tokens")),
        );
        let input = prompt_tokens
            .saturating_sub(cache_read_tokens)
            .saturating_sub(cache_write_tokens);
        // OpenAI 的 completion_tokens 已经包含 reasoning_tokens。
        let output_tokens = number(raw.get("completion_tokens"));
        let mut usage = Usage {
            input,
            output: output_tokens,
            cache_read: cache_read_tokens,
            cache_write: cache_write_tokens,
            cache_write_1h: None,
            reasoning: Some(number(
                raw.get("completion_tokens_details")
                    .and_then(|details| details.get("reasoning_tokens"))
                    .or_else(|| raw.get("reasoning_tokens")),
            )),
            total_tokens: input + output_tokens + cache_read_tokens + cache_write_tokens,
            cost: UsageCost::default(),
        };
        calculate_cost(&self.model, &mut usage);
        usage
    }

    /// 上游的收尾判断：自然读完流之后要跑这一串。
    pub fn finalize(&mut self, signal: Option<&AbortSignal>) -> Result<(), NormalizedEnd> {
        if signal.map(|signal| signal.aborted()).unwrap_or(false) {
            return Err(NormalizedEnd::Aborted);
        }
        if self.output.stop_reason == StopReason::Aborted {
            return Err(NormalizedEnd::Aborted);
        }
        if !self.has_finish_reason && !self.compat.supports_finish_reason {
            self.output.stop_reason = if self
                .output
                .content
                .iter()
                .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
            {
                StopReason::ToolUse
            } else {
                StopReason::Stop
            };
        }
        if self.output.stop_reason == StopReason::Error {
            return Err(NormalizedEnd::Error(
                self.output
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "Provider returned an error stop reason".to_string()),
            ));
        }
        if (self.compat.supports_finish_reason && !self.has_finish_reason)
            || self.output.stop_reason == StopReason::Pending
        {
            return Err(NormalizedEnd::Error(
                "Stream ended without finish_reason".to_string(),
            ));
        }
        Ok(())
    }
}

/// 收尾时可能出现的两种失败：取消与 provider 错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizedEnd {
    Aborted,
    Error(String),
}

/// pi: `mapStopReason`
pub fn map_stop_reason(reason: &str) -> (StopReason, Option<String>) {
    match reason {
        "stop" | "end" => (StopReason::Stop, None),
        "length" => (StopReason::Length, None),
        "function_call" | "tool_calls" => (StopReason::ToolUse, None),
        "content_filter" => (
            StopReason::Error,
            Some("Provider finish_reason: content_filter".to_string()),
        ),
        "network_error" => (
            StopReason::Error,
            Some("Provider finish_reason: network_error".to_string()),
        ),
        other => (
            StopReason::Error,
            Some(format!("Provider finish_reason: {other}")),
        ),
    }
}

// ---------------------------------------------------------------------------
// HTTP 传输
// ---------------------------------------------------------------------------

/// 把 provider 的头合并起来：身份头 → 模型头 → 调用方头（`None` 表示删掉）。
fn merged_headers(
    model: &Model,
    identity: Option<&crate::utils::identity::ClientIdentity>,
    options: Option<&OpenAICompletionsOptions>,
) -> BTreeMap<String, String> {
    let mut headers = get_identity_headers(model, identity);
    if let Some(model_headers) = &model.headers {
        for (key, value) in model_headers {
            headers.insert(key.clone(), value.clone());
        }
    }
    if let Some(option_headers) = options.and_then(|options| options.headers.as_ref()) {
        for (key, value) in option_headers {
            match value {
                Some(value) => {
                    headers.insert(key.clone(), value.clone());
                }
                None => {
                    headers.remove(key);
                }
            }
        }
    }
    headers
}

fn chat_completions_url(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

/// 进程内共用的一只 HTTP 客户端（连接池跟着它走）。
///
/// 每轮新建客户端会丢掉上一条连接：到 DeepSeek 新建一条连接约 0.46 秒，
/// 小任务因此每轮多等近半秒。取消与超时仍由 `send_request` 管，不靠客户端。
pub(crate) fn shared_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// pi: `stream`。同步返回事件流，HTTP 与解析在后台任务里跑。
pub fn stream(
    model: Model,
    context: TranscriptContext,
    options: Option<OpenAICompletionsOptions>,
    signal: Option<AbortSignal>,
) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::new();
    let task_stream = stream.clone();
    tokio::spawn(async move {
        run_stream(model, context, options, signal, task_stream).await;
    });
    stream
}

/// 后台任务：发请求、读 SSE、把事件推进流里。
async fn run_stream(
    model: Model,
    context: TranscriptContext,
    options: Option<OpenAICompletionsOptions>,
    signal: Option<AbortSignal>,
    stream: AssistantMessageEventStream,
) {
    let compat = get_compat(&model);
    let mut state = ChatCompletionsStreamState::new(model.clone());

    // 上层在取消时不希望再等网络：请求与读流都挂在取消信号上。

    let api_key = match get_client_api_key(
        &model.provider,
        options
            .as_ref()
            .and_then(|options| options.api_key.as_deref()),
        &options.as_ref().and_then(|options| options.headers.clone()),
    ) {
        Ok(api_key) => api_key,
        Err(message) => {
            state.output.stop_reason = StopReason::Error;
            state.output.error_message = Some(format_provider_error(&plain_error(message), None));
            stream.push(AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: state.output.clone(),
            });
            stream.end(None);
            return;
        }
    };

    let mut params = match build_params(&model, &context, options.as_ref(), &compat) {
        Ok(params) => params,
        Err(message) => {
            state.output.stop_reason = StopReason::Error;
            state.output.error_message = Some(message);
            stream.push(AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: state.output.clone(),
            });
            stream.end(None);
            return;
        }
    };
    // `onPayload`：发送前看一眼或换掉 payload。
    if let Some(callback) = options
        .as_ref()
        .and_then(|options| options.on_payload.arc())
    {
        let payload = callback(Value::Object(params.clone()), &model).await;
        if let Some(Value::Object(next_params)) = payload {
            params = next_params;
        }
    }

    // 上游 OpenAI SDK 的 10 分钟只管「等到响应开始」；正文流再多也能收完（见差异 #115）。
    let timeout = std::time::Duration::from_millis(
        options
            .as_ref()
            .and_then(|options| options.timeout_ms)
            .unwrap_or(10 * 60 * 1000),
    );
    // 同一进程共用一只客户端：连接池留着，下一轮请求复用同一条连接。
    let client = shared_http_client();

    // 头按上游的层次合并：身份头 → 模型头 → 调用方头；调用方没给 Authorization 时用钥匙。
    let headers = merged_headers(
        &model,
        options.as_ref().and_then(|options| options.identity.as_ref()),
        options.as_ref(),
    );
    let mut request = client
        .post(chat_completions_url(&model.base_url))
        .header("Content-Type", "application/json")
        .json(&Value::Object(params));
    for (key, value) in &headers {
        request = request.header(key, value);
    }
    if !headers
        .keys()
        .any(|key| key.eq_ignore_ascii_case("authorization"))
    {
        request = request.bearer_auth(&api_key);
    }

    let response = match send_request(request, &signal, timeout).await {
        Ok(response) => response,
        Err(SendFailure::Aborted) => {
            emit_aborted(&mut state, &stream);
            return;
        }
        Err(SendFailure::TimedOut) => {
            state.output.stop_reason = StopReason::Error;
            state.output.error_message = Some("Request timed out".to_string());
            stream.push(AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: state.output.clone(),
            });
            stream.end(None);
            return;
        }
        Err(SendFailure::Transport(error)) => {
            state.output.stop_reason = StopReason::Error;
            state.output.error_message = Some(format_provider_error(
                &transport_error(&error.to_string(), &[]),
                None,
            ));
            stream.push(AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: state.output.clone(),
            });
            stream.end(None);
            return;
        }
    };

    if let Some(callback) = options
        .as_ref()
        .and_then(|options| options.on_response.arc())
    {
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.to_string(), value.to_string()))
            })
            .collect();
        callback(
            &crate::types::ProviderResponse {
                status: response.status().as_u16(),
                headers,
            },
            &model,
        )
        .await;
    }

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.ok();
        state.output.stop_reason = StopReason::Error;
        state.output.error_message =
            Some(format_provider_error(&http_error(status, body.as_deref()), None));
        stream.push(AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: state.output.clone(),
        });
        stream.end(None);
        return;
    }

    stream.push(AssistantMessageEvent::Start {
        partial: state.output.clone(),
    });

    let mut decoder = SseDecoder::new();
    let mut body = response.bytes_stream();
    let mut done = false;
    loop {
        let next = if let Some(signal) = &signal {
            tokio::select! {
                biased;
                _ = signal.cancelled() => None,
                next = body.next() => Some(next),
            }
        } else {
            Some(body.next().await)
        };
        let Some(next) = next else {
            emit_aborted(&mut state, &stream);
            return;
        };
        let Some(chunk) = next else {
            break;
        };
        let bytes = match chunk {
            Ok(bytes) => bytes,
            Err(error) => {
                if signal
                    .as_ref()
                    .map(|signal| signal.aborted())
                    .unwrap_or(false)
                {
                    emit_aborted(&mut state, &stream);
                    return;
                }
                state.output.stop_reason = StopReason::Error;
                state.output.error_message =
                    Some(format_provider_error(&transport_error(&error.to_string(), &[]), None));
                stream.push(AssistantMessageEvent::Error {
                    reason: state.output.stop_reason,
                    error: state.output.clone(),
                });
                stream.end(None);
                return;
            }
        };
        for event in decoder.push_bytes(&bytes) {
            match handle_sse_event(&event.data, &mut state, options.as_ref(), &model, &stream).await {
                Ok(true) => {
                    done = true;
                    break;
                }
                Ok(false) => {}
                Err(message) => {
                    emit_stream_error(&mut state, &stream, message);
                    return;
                }
            }
        }
        if done {
            break;
        }
    }
    if !done {
        for event in decoder.finish() {
            match handle_sse_event(&event.data, &mut state, options.as_ref(), &model, &stream).await {
                Ok(true) => break,
                Ok(false) => {}
                Err(message) => {
                    emit_stream_error(&mut state, &stream, message);
                    return;
                }
            }
        }
    }

    for event in state.finish_blocks() {
        stream.push(event);
    }
    match state.finalize(signal.as_ref()) {
        Ok(()) => {
            stream.push(AssistantMessageEvent::Done {
                reason: state.output.stop_reason,
                message: state.output.clone(),
            });
            stream.end(None);
        }
        Err(NormalizedEnd::Aborted) => emit_aborted(&mut state, &stream),
        Err(NormalizedEnd::Error(message)) => {
            state.output.stop_reason = StopReason::Error;
            state.output.error_message = Some(message);
            stream.push(AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: state.output.clone(),
            });
            stream.end(None);
        }
    }
}

/// 取消：上游先把已经开始的块逐个收尾（`*_end`），再留一条 `aborted` 的错误消息；不发 `done`。
fn emit_aborted(state: &mut ChatCompletionsStreamState, stream: &AssistantMessageEventStream) {
    for event in state.finish_blocks() {
        stream.push(event);
    }
    state.output.stop_reason = StopReason::Aborted;
    state.output.error_message = Some("Request was aborted".to_string());
    stream.push(AssistantMessageEvent::Error {
        reason: StopReason::Aborted,
        error: state.output.clone(),
    });
    stream.end(None);
}

enum SendFailure {
    Aborted,
    TimedOut,
    Transport(reqwest::Error),
}

/// 发请求并等响应头；`timeout` 只管这一段，正文流交给 `bytes_stream()` 慢慢收。
async fn send_request(
    request: reqwest::RequestBuilder,
    signal: &Option<AbortSignal>,
    timeout: std::time::Duration,
) -> Result<reqwest::Response, SendFailure> {
    if let Some(signal) = signal {
        tokio::select! {
            biased;
            _ = signal.cancelled() => Err(SendFailure::Aborted),
            _ = tokio::time::sleep(timeout) => Err(SendFailure::TimedOut),
            response = request.send() => response.map_err(SendFailure::Transport),
        }
    } else {
        tokio::select! {
            biased;
            _ = tokio::time::sleep(timeout) => Err(SendFailure::TimedOut),
            response = request.send() => response.map_err(SendFailure::Transport),
        }
    }
}

/// 处理一条 SSE 的 `data`。`Ok(true)` 表示流该停了（`[DONE]`）。
///
/// 不是 JSON 的正文按错误处理：上游的 SDK 在这里 `JSON.parse` 抛错，整条流以 `error` 收尾。
async fn handle_sse_event(
    data: &str,
    state: &mut ChatCompletionsStreamState,
    options: Option<&OpenAICompletionsOptions>,
    model: &Model,
    stream: &AssistantMessageEventStream,
) -> Result<bool, String> {
    if data.trim() == "[DONE]" {
        return Ok(true);
    }
    let chunk = serde_json::from_str::<Value>(data)
        .map_err(|error| format!("Failed to parse stream chunk: {error}"))?;
    if let Some(callback) = options.and_then(|options| options.on_provider_stream_event.arc()) {
        callback(&chunk, model).await;
    }
    for event in state.apply_chunk(&chunk) {
        stream.push(event);
    }
    Ok(false)
}

/// 流中途的失败：与上游的 catch 一样，不发 `*_end`。
fn emit_stream_error(
    state: &mut ChatCompletionsStreamState,
    stream: &AssistantMessageEventStream,
    message: String,
) {
    state.output.stop_reason = StopReason::Error;
    state.output.error_message = Some(message);
    stream.push(AssistantMessageEvent::Error {
        reason: StopReason::Error,
        error: state.output.clone(),
    });
    stream.end(None);
}

/// pi: `streamSimple`——把简单选项铺开，再一次走 `stream`。
pub fn stream_simple(
    model: Model,
    context: TranscriptContext,
    options: Option<SimpleStreamOptions>,
    signal: Option<AbortSignal>,
) -> AssistantMessageEventStream {
    let base = crate::api::simple_options::build_base_options(
        &model,
        &context,
        options.as_ref(),
        options.as_ref().and_then(|options| options.api_key.as_deref()),
    );
    let clamped_reasoning = options
        .as_ref()
        .and_then(|options| options.reasoning)
        .map(|reasoning| clamp_thinking_level(&model, crate::api::simple_options::thinking_level_to_model_level(reasoning)));
    let reasoning_effort = clamped_reasoning
        .and_then(crate::api::simple_options::model_level_to_thinking_level);
    let openai_options = OpenAICompletionsOptions {
        tool_choice: options.as_ref().and_then(|options| options.tool_choice),
        reasoning_effort,
        thinking_budgets: options.as_ref().and_then(|options| options.thinking_budgets.clone()),
        ..OpenAICompletionsOptions::from_stream_options(base)
    };
    stream(model, context, Some(openai_options), signal)
}
