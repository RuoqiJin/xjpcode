//! 对应上游 `packages/ai/src/api/openai-responses-shared.ts`。
//!
//! OpenAI Responses 这一族（`openai-responses` / `openai-codex-responses`）共用的两件事：
//! 把对话记录换成 `input` 项、把服务端事件流收成 `AssistantMessageEvent`。
//!
//! 这一轮只接 GPT 订阅（`openai-codex-responses`）走到的分支：grammar（custom）工具没有搬，
//! 其余按上游实现（见 `PORTING.md` 差异表）。

use std::collections::{BTreeSet, HashMap};

use serde_json::{json, Map, Value};

use crate::api::constrained_sampling::{
    get_json_schema_tool_parameters, resolve_json_schema_strict_sampling,
};
use crate::models::calculate_cost;
use crate::providers::model_schema::{ModelThinkingLevel, ThinkingLevel};
use crate::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, ImageContent, JsonObject,
    Message, Model, StopReason, TextContent, ThinkingContent, Tool, ToolCall, ToolResultContentBlock,
    TranscriptContext, Usage, UsageCost,
};
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::streaming_json::ToolCallArgumentsStream;
use crate::utils::text::{get_system_message_text, render_system_message_update};
use crate::utils::transcript::{
    get_declared_tools, resolve_transcript, resolve_transcript_tools, TranscriptToolSupport,
};

/// 上游用 `event.output_index` 做 Map 键；服务端省略它时是 `undefined`（同一个槽），
/// 所以缺字段的用一个固定的哨兵键，别把事件丢掉（假的 Codex 接口就会省略它）。
fn event_output_index(event: &Value) -> usize {
    event
        .get("output_index")
        .and_then(Value::as_u64)
        .map(|index| index as usize)
        .unwrap_or(usize::MAX)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

pub use crate::utils::hash::short_hash;

/// 上游的 `sanitizeSurrogates`：Rust 的字符串永远是合法 UTF-8，没有落单的代理项，原样返回。
pub fn sanitize_surrogates(text: &str) -> String {
    text.to_string()
}

fn encode_text_signature(id: &str, phase: Option<&str>) -> String {
    let mut payload = Map::new();
    payload.insert("v".to_string(), json!(1));
    payload.insert("id".to_string(), json!(id));
    if let Some(phase) = phase {
        payload.insert("phase".to_string(), json!(phase));
    }
    Value::Object(payload).to_string()
}

fn parse_text_signature(signature: Option<&str>) -> Option<(String, Option<String>)> {
    let signature = signature?;
    if signature.starts_with('{') {
        if let Ok(parsed) = serde_json::from_str::<Value>(signature) {
            if parsed.get("v").and_then(Value::as_u64) == Some(1) {
                if let Some(id) = parsed.get("id").and_then(Value::as_str) {
                    let phase = parsed
                        .get("phase")
                        .and_then(Value::as_str)
                        .filter(|phase| *phase == "commentary" || *phase == "final_answer")
                        .map(str::to_string);
                    return Some((id.to_string(), phase));
                }
            }
        }
    }
    Some((signature.to_string(), None))
}

/// pi: `ConvertResponsesToolsOptions`
#[derive(Debug, Clone)]
pub struct ConvertResponsesToolsOptions {
    pub strict: Option<bool>,
    pub supports_strict_mode: bool,
    pub supports_open_ai_grammar_tools: bool,
}

impl Default for ConvertResponsesToolsOptions {
    fn default() -> Self {
        Self {
            strict: None,
            supports_strict_mode: true,
            supports_open_ai_grammar_tools: false,
        }
    }
}

/// pi: `convertResponsesTools`
pub fn convert_responses_tools(
    tools: &[Tool],
    options: Option<&ConvertResponsesToolsOptions>,
) -> Vec<Value> {
    let default_strict = options.and_then(|options| options.strict).unwrap_or(false);
    let supports_strict_mode = options
        .map(|options| options.supports_strict_mode)
        .unwrap_or(true);

    tools
        .iter()
        .map(|tool| {
            // grammar（custom）工具这一轮没有搬；照上游的退路当普通函数工具发。
            let constrained_strict = resolve_json_schema_strict_sampling(tool, supports_strict_mode)
                .ok()
                .flatten();
            let strict = constrained_strict.unwrap_or(default_strict);
            let mut function = Map::new();
            function.insert("type".to_string(), json!("function"));
            function.insert("name".to_string(), json!(tool.name));
            function.insert("description".to_string(), json!(tool.description));
            function.insert(
                "parameters".to_string(),
                get_json_schema_tool_parameters(tool, Some(strict)),
            );
            if supports_strict_mode {
                function.insert("strict".to_string(), json!(strict));
            }
            Value::Object(function)
        })
        .collect()
}

/// pi: `ConvertResponsesMessagesOptions`
#[derive(Debug, Clone, Default)]
pub struct ConvertResponsesMessagesOptions {
    pub supports_mid_convo_system_messages: bool,
    pub supports_additional_tools: bool,
    pub supports_tool_search: bool,
    pub tool_options: Option<ConvertResponsesToolsOptions>,
    /// 生成这次回答用的 effort；设了就改用 `configuration_update` 项报思考档，
    /// 而不是每次都改 `reasoning.effort`（那会让提示词缓存失效）。
    pub configuration_update_effort: Option<String>,
}

/// pi: `getResponsesToolSupport`
pub fn get_responses_tool_support(
    supports_mid_convo_system_messages: bool,
    supports_additional_tools: bool,
    supports_tool_search: bool,
) -> TranscriptToolSupport {
    if supports_mid_convo_system_messages && supports_additional_tools {
        TranscriptToolSupport::Changes
    } else if supports_tool_search {
        TranscriptToolSupport::Additions
    } else {
        TranscriptToolSupport::None
    }
}

fn render_incremental_tools_notice(unavailable: &BTreeSet<String>) -> String {
    let notice = "This is an incremental tools update. Previously declared tools remain available unless explicitly marked unavailable. A tool declared again here is available again, and its latest definition replaces the earlier one.";
    if unavailable.is_empty() {
        notice.to_string()
    } else {
        let names: Vec<&str> = unavailable.iter().map(String::as_str).collect();
        format!("{notice}\nStill unavailable: {}.", names.join(", "))
    }
}

fn render_tool_removal_notice(names: &[String]) -> String {
    format!(
        "The following tools are no longer available. Do not call them:\n{}",
        names
            .iter()
            .map(|name| format!("- {name}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn normalize_id_part(part: &str) -> String {
    let sanitized: String = part
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let normalized: String = sanitized.chars().take(64).collect();
    normalized.trim_end_matches('_').to_string()
}

fn build_foreign_responses_item_id(item_id: &str) -> String {
    let normalized = format!("fc_{}", short_hash(item_id));
    normalized.chars().take(64).collect()
}

fn normalize_codex_tool_call_id(
    id: &str,
    model: &Model,
    source: &AssistantMessage,
    allowed_tool_call_providers: &BTreeSet<String>,
) -> String {
    if !allowed_tool_call_providers.contains(&model.provider) {
        return normalize_id_part(id);
    }
    let Some((call_id, item_id)) = id.split_once('|') else {
        return normalize_id_part(id);
    };
    let normalized_call_id = normalize_id_part(call_id);
    let is_foreign = source.provider != model.provider || source.api != model.api;
    let mut normalized_item_id = if is_foreign {
        build_foreign_responses_item_id(item_id)
    } else {
        normalize_id_part(item_id)
    };
    if !normalized_item_id.starts_with("fc_") {
        normalized_item_id = normalize_id_part(&format!("fc_{normalized_item_id}"));
    }
    format!("{normalized_call_id}|{normalized_item_id}")
}

fn convert_tool_result_output(model: &Model, content: &[ToolResultContentBlock]) -> Value {
    let text_result = content
        .iter()
        .filter_map(|block| match block {
            ToolResultContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let images: Vec<&ImageContent> = content
        .iter()
        .filter_map(|block| match block {
            ToolResultContentBlock::Image(image) => Some(image),
            _ => None,
        })
        .collect();
    let has_text = !text_result.trim().is_empty();
    let supports_images = model.input.iter().any(|modality| {
        *modality == crate::providers::model_schema::ModelInputModality::Image
    });

    if images.is_empty() || !supports_images {
        return json!(sanitize_surrogates(if has_text {
            &text_result
        } else if images.is_empty() {
            "(no tool output)"
        } else {
            "(see attached image)"
        }));
    }

    let mut output: Vec<Value> = Vec::new();
    if has_text {
        output.push(json!({ "type": "input_text", "text": sanitize_surrogates(&text_result) }));
    }
    for image in images {
        output.push(json!({
            "type": "input_image",
            "detail": "auto",
            "image_url": format!("data:{};base64,{}", image.mime_type, image.data),
        }));
    }
    Value::Array(output)
}

/// pi: `convertResponsesMessages`
pub fn convert_responses_messages(
    model: &Model,
    context: &TranscriptContext,
    allowed_tool_call_providers: &BTreeSet<String>,
    options: Option<&ConvertResponsesMessagesOptions>,
) -> Vec<Value> {
    let options = options.cloned().unwrap_or_default();
    let normalized_context =
        resolve_transcript(context, Some(options.supports_mid_convo_system_messages));
    let mut messages: Vec<Value> = Vec::new();

    let normalize = |id: &str, target: &Model, source: &AssistantMessage| {
        normalize_codex_tool_call_id(id, target, source, allowed_tool_call_providers)
    };
    let transformed_messages = crate::api::transform_messages::transform_messages(
        &normalized_context.messages,
        model,
        Some(&normalize),
    );

    let tool_support = get_responses_tool_support(
        options.supports_mid_convo_system_messages,
        options.supports_additional_tools,
        options.supports_tool_search,
    );
    let transcript_tools = resolve_transcript_tools(&normalized_context.messages, tool_support);
    let mut declared_additional_tools = false;
    let mut unavailable_tools: BTreeSet<String> = BTreeSet::new();
    let instructions_role = if model.reasoning
        && model
            .compat
            .as_ref()
            .and_then(|compat| compat.supports_developer_role)
            .unwrap_or(true)
    {
        "developer"
    } else {
        "system"
    };

    let mut msg_index = 0usize;
    let mut source_index = 0usize;
    let mut system_index = 0usize;
    for message in &transformed_messages {
        let is_leading_system_message = source_index == 0 && matches!(message, Message::System(_));
        source_index += 1;
        match message {
            Message::System(system) => {
                let anchors_tool_changes = system_index >= transcript_tools.folded_system_messages;
                system_index += 1;
                let added_tools: Vec<Tool> = system.tools_added.clone().unwrap_or_default();
                let removed_tools: Vec<String> = system
                    .tools_removed
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|tool| tool.name)
                    .collect();
                let mut parts: Vec<String> = Vec::new();
                if anchors_tool_changes && !added_tools.is_empty() {
                    for tool in &added_tools {
                        unavailable_tools.remove(&tool.name);
                    }
                    match tool_support {
                        TranscriptToolSupport::Changes => {
                            // 上游把「增量工具声明」这句单独放在 additional_tools 之前，
                            // 不是和系统提示的变化并在一起。
                            if declared_additional_tools {
                                messages.push(json!({
                                    "role": instructions_role,
                                    "content": render_incremental_tools_notice(&unavailable_tools),
                                }));
                            }
                            declared_additional_tools = true;
                            messages.push(json!({
                                "type": "additional_tools",
                                "role": "developer",
                                "tools": convert_responses_tools(&added_tools, options.tool_options.as_ref()),
                            }));
                        }
                        TranscriptToolSupport::Additions => {
                            let names: Vec<String> =
                                added_tools.iter().map(|tool| tool.name.clone()).collect();
                            let call_id = format!(
                                "pi_tool_load_{}",
                                short_hash(&format!(
                                    "system:{msg_index}:{}",
                                    names.join(",")
                                ))
                            );
                            messages.push(json!({
                                "type": "tool_search_call",
                                "call_id": call_id,
                                "execution": "client",
                                "status": "completed",
                                "arguments": { "query": names.join(" "), "limit": names.len() },
                            }));
                            messages.push(json!({
                                "type": "tool_search_output",
                                "call_id": call_id,
                                "execution": "client",
                                "status": "completed",
                                "tools": convert_responses_tools(&added_tools, options.tool_options.as_ref()),
                            }));
                        }
                        TranscriptToolSupport::None => {}
                    }
                }
                parts.push(if is_leading_system_message {
                    get_system_message_text(system)
                } else {
                    render_system_message_update(system)
                });
                if anchors_tool_changes {
                    let added: BTreeSet<&String> =
                        added_tools.iter().map(|tool| &tool.name).collect();
                    let removed: Vec<String> = removed_tools
                        .into_iter()
                        .filter(|name| !added.contains(name))
                        .collect();
                    if !removed.is_empty() {
                        parts.push(render_tool_removal_notice(&removed));
                        for name in removed {
                            unavailable_tools.insert(name);
                        }
                    }
                }
                let text = parts
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if !text.is_empty() {
                    messages.push(json!({
                        "role": instructions_role,
                        "content": sanitize_surrogates(&text),
                    }));
                }
            }
            Message::User(user) => match &user.content {
                crate::types::UserContent::Text(text) => {
                    messages.push(json!({
                        "role": "user",
                        "content": [{
                            "type": "input_text",
                            "text": sanitize_surrogates(text.as_str()),
                        }],
                    }));
                }
                crate::types::UserContent::Blocks(blocks) => {
                    let content: Vec<Value> = blocks
                        .iter()
                        .map(|block| match block {
                            crate::types::UserContentBlock::Text(text) => json!({
                                "type": "input_text",
                                "text": sanitize_surrogates(&text.text),
                            }),
                            crate::types::UserContentBlock::Image(image) => json!({
                                "type": "input_image",
                                "detail": "auto",
                                "image_url": format!("data:{};base64,{}", image.mime_type, image.data),
                            }),
                        })
                        .collect();
                    if !content.is_empty() {
                        messages.push(json!({ "role": "user", "content": content }));
                    }
                }
            },
            Message::Assistant(assistant) => {
                let mut output: Vec<Value> = Vec::new();
                let is_same_provider_and_api =
                    assistant.provider == model.provider && assistant.api == model.api;
                let is_same_model = is_same_provider_and_api && assistant.model == model.id;
                let is_different_model = is_same_provider_and_api && assistant.model != model.id;
                let mut text_block_index = 0usize;
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Thinking(thinking) => {
                            if let Some(signature) = &thinking.thinking_signature {
                                if let Ok(item) = serde_json::from_str::<Value>(signature) {
                                    output.push(item);
                                }
                            }
                        }
                        AssistantContentBlock::Text(text) => {
                            let parsed = parse_text_signature(text.text_signature.as_deref());
                            let fallback = if text_block_index == 0 {
                                format!("msg_pi_{msg_index}")
                            } else {
                                format!("msg_pi_{msg_index}_{text_block_index}")
                            };
                            text_block_index += 1;
                            let mut msg_id = parsed
                                .as_ref()
                                .map(|(id, _)| id.clone())
                                .unwrap_or(fallback);
                            if msg_id.chars().count() > 64 {
                                msg_id = format!("msg_{}", short_hash(&msg_id));
                            }
                            let mut item = Map::new();
                            item.insert("type".to_string(), json!("message"));
                            item.insert("role".to_string(), json!("assistant"));
                            item.insert(
                                "content".to_string(),
                                json!([{
                                    "type": "output_text",
                                    "text": sanitize_surrogates(&text.text),
                                    "annotations": [],
                                }]),
                            );
                            item.insert("status".to_string(), json!("completed"));
                            item.insert("id".to_string(), json!(msg_id));
                            if let Some((_, Some(phase))) = parsed {
                                item.insert("phase".to_string(), json!(phase));
                            }
                            output.push(Value::Object(item));
                        }
                        AssistantContentBlock::ToolCall(tool_call) => {
                            let (call_id, item_id_raw) = match tool_call.id.split_once('|') {
                                Some((call_id, item_id)) => {
                                    (call_id.to_string(), Some(item_id.to_string()))
                                }
                                None => (tool_call.id.clone(), None),
                            };
                            let mut item_id = item_id_raw;
                            if is_different_model
                                || !item_id.as_deref().is_some_and(|id| id.starts_with("fc_"))
                            {
                                item_id = None;
                            }
                            let mut item = Map::new();
                            item.insert("type".to_string(), json!("function_call"));
                            if let Some(item_id) = item_id {
                                item.insert("id".to_string(), json!(item_id));
                            }
                            item.insert("call_id".to_string(), json!(call_id));
                            item.insert("name".to_string(), json!(tool_call.name));
                            item.insert(
                                "arguments".to_string(),
                                json!(Value::Object(tool_call.arguments.clone()).to_string()),
                            );
                            if is_same_model {
                                if let Some(namespace) = &tool_call.namespace {
                                    item.insert("namespace".to_string(), json!(namespace));
                                }
                            }
                            output.push(Value::Object(item));
                        }
                    }
                }
                if output.is_empty() {
                    msg_index += 1;
                    continue;
                }
                if let Some(_effort) = &options.configuration_update_effort {
                    if is_same_model {
                        if let Some(level) = &assistant.provider_thinking_level {
                            messages.push(json!({
                                "type": "configuration_update",
                                "reasoning": { "effort": level },
                            }));
                        }
                    }
                }
                messages.extend(output);
            }
            Message::ToolResult(result) => {
                let call_id = result
                    .tool_call_id
                    .split_once('|')
                    .map(|(call_id, _)| call_id.to_string())
                    .unwrap_or_else(|| result.tool_call_id.clone());
                messages.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": convert_tool_result_output(model, &result.content),
                }));
            }
        }
        if !is_leading_system_message {
            msg_index += 1;
        }
    }
    if let Some(effort) = &options.configuration_update_effort {
        messages.push(json!({
            "type": "configuration_update",
            "reasoning": { "effort": effort },
        }));
    }
    messages
}

/// pi: `OpenAIResponsesStreamOptions`
#[derive(Default)]
pub struct OpenAIResponsesStreamOptions {
    pub apply_service_tier_pricing:
        Option<Box<dyn Fn(&mut Usage, Option<&str>) + Send + Sync>>,
}

#[derive(Debug)]
enum ToolCallInput {
    Json(ToolCallArgumentsStream),
}

#[derive(Debug)]
enum SlotKind {
    Thinking { content_index: usize },
    Text { content_index: usize },
    ToolCall { content_index: usize, input: ToolCallInput },
}

impl SlotKind {
    fn content_index(&self) -> usize {
        match self {
            Self::Thinking { content_index }
            | Self::Text { content_index }
            | Self::ToolCall { content_index, .. } => *content_index,
        }
    }
}

pub fn create_output(model: &Model) -> AssistantMessage {
    AssistantMessage {
        role: crate::types::AssistantRole::Assistant,
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
    }
}

fn item_text_list(item: &Value, field: &str) -> String {
    item.get(field)
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default()
}

fn create_slot(
    index: usize,
    item: &Value,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    slots: &mut HashMap<usize, SlotKind>,
    unfinished_tool_calls: &mut BTreeSet<usize>,
) {
    match item.get("type").and_then(Value::as_str) {
        Some("reasoning") => {
            output
                .content
                .push(AssistantContentBlock::Thinking(ThinkingContent {
                    r#type: crate::types::ThinkingContentType::Thinking,
                    thinking: String::new(),
                    thinking_signature: None,
                    redacted: None,
                }));
            let content_index = output.content.len() - 1;
            slots.insert(index, SlotKind::Thinking { content_index });
            stream.push(AssistantMessageEvent::ThinkingStart {
                content_index,
                partial: output.clone(),
            });
        }
        Some("message") => {
            output
                .content
                .push(AssistantContentBlock::Text(TextContent::new("")));
            let content_index = output.content.len() - 1;
            slots.insert(index, SlotKind::Text { content_index });
            if item.get("phase").and_then(Value::as_str) == Some("final_answer") {
                output.stop_reason = StopReason::Stop;
            }
            stream.push(AssistantMessageEvent::TextStart {
                content_index,
                partial: output.clone(),
            });
        }
        Some("function_call") => {
            let call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let mut arguments = ToolCallArgumentsStream::new();
            let initial = item.get("arguments").and_then(Value::as_str).unwrap_or("");
            let parsed: JsonObject = if initial.is_empty() {
                JsonObject::new()
            } else {
                (*arguments.append(initial)).clone()
            };
            let mut block = ToolCall::new(format!("{call_id}|{item_id}"), name, parsed);
            if let Some(namespace) = item.get("namespace").and_then(Value::as_str) {
                block.namespace = Some(namespace.to_string());
            }
            output.content.push(AssistantContentBlock::ToolCall(block));
            let content_index = output.content.len() - 1;
            slots.insert(
                index,
                SlotKind::ToolCall {
                    content_index,
                    input: ToolCallInput::Json(arguments),
                },
            );
            // 记内容下标（不是事件下标）：没带 output_index 的并行调用会共用同一个键，
            // 收尾时要认得出每一条（上游也是按内容块记的）。
            unfinished_tool_calls.insert(content_index);
            stream.push(AssistantMessageEvent::ToolcallStart {
                content_index,
                partial: output.clone(),
            });
        }
        _ => {}
    }
}

fn finalize_response(
    response: &Value,
    output: &mut AssistantMessage,
    model: &Model,
    options: &OpenAIResponsesStreamOptions,
) {
    if let Some(id) = response.get("id").and_then(Value::as_str) {
        output.response_id = Some(id.to_string());
    }
    if let Some(usage) = response.get("usage").filter(|usage| !usage.is_null()) {
        let input_details = usage.get("input_tokens_details");
        let cached_tokens = input_details
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cache_write_tokens = input_details
            .and_then(|details| details.get("cache_write_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let input_tokens = usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        output.usage = Usage {
            input: input_tokens.saturating_sub(cached_tokens + cache_write_tokens),
            output: usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_read: cached_tokens,
            cache_write: cache_write_tokens,
            cache_write_1h: None,
            reasoning: usage
                .get("output_tokens_details")
                .and_then(|details| details.get("reasoning_tokens"))
                .and_then(Value::as_u64),
            total_tokens: usage
                .get("total_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cost: UsageCost::default(),
        };
    }
    calculate_cost(model, &mut output.usage);
    if let Some(apply) = &options.apply_service_tier_pricing {
        let service_tier = response
            .get("service_tier")
            .and_then(Value::as_str)
            .map(str::to_string);
        apply(&mut output.usage, service_tier.as_deref());
    }
    let status = response.get("status").and_then(Value::as_str);
    let incomplete_reason = response
        .get("incomplete_details")
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str);
    output.raw_stop_reason = Some(match incomplete_reason {
        Some(reason) => format!("{}.{reason}", status.unwrap_or("")),
        None => status.unwrap_or("").to_string(),
    });
    match status {
        Some("completed") | None => {
            output.stop_reason = StopReason::Stop;
            output.error_message = None;
        }
        Some("incomplete") => {
            if incomplete_reason == Some("max_output_tokens") {
                output.stop_reason = StopReason::Length;
                output.error_message = None;
            } else {
                output.stop_reason = StopReason::Error;
                output.error_message = Some(match incomplete_reason {
                    Some(reason) => format!("Response incomplete: {reason}"),
                    None => "Response incomplete without a provider reason".to_string(),
                });
            }
        }
        Some("failed") | Some("cancelled") => {
            output.stop_reason = StopReason::Error;
        }
        Some("in_progress") | Some("queued") => {
            output.stop_reason = StopReason::Stop;
        }
        _ => {}
    }
    if output
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
        && output.stop_reason == StopReason::Stop
    {
        output.stop_reason = StopReason::ToolUse;
    }
}

/// pi: `extractCodexEventError`——外层优先，其次 `error.code` / `error.message`。
fn extract_codex_event_error(event: &Value) -> (Option<String>, Option<String>) {
    let nested = event.get("error").filter(|error| error.is_object());
    let code = event
        .get("code")
        .and_then(Value::as_str)
        .or_else(|| nested.and_then(|error| error.get("code")).and_then(Value::as_str))
        .map(str::to_string);
    let message = event
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| {
            nested
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
        })
        .map(str::to_string);
    (code, message)
}

/// pi: 把一条条服务端事件收成 `AssistantMessageEvent`。
///
/// 事件从通道里来（SSE 读流任务往里放）；通道关闭表示流结束。
pub async fn process_responses_stream(
    mut events: tokio::sync::mpsc::UnboundedReceiver<Result<Value, crate::auth::ModelsError>>,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    model: &Model,
    options: &OpenAIResponsesStreamOptions,
) -> Result<(), crate::auth::ModelsError> {
    let mut saw_terminal_response_event = false;
    let mut slots: HashMap<usize, SlotKind> = HashMap::new();
    let mut unfinished_tool_calls: BTreeSet<usize> = BTreeSet::new();

    while let Some(event) = events.recv().await {
        let event = event?;
        let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
        match event_type {
            "response.created" => {
                if let Some(id) = event
                    .get("response")
                    .and_then(|response| response.get("id"))
                    .and_then(Value::as_str)
                {
                    output.response_id = Some(id.to_string());
                }
            }
            "response.output_item.added" => {
                let Some(item) = event.get("item") else {
                    continue;
                };
                create_slot(
                    event_output_index(&event),
                    item,
                    output,
                    stream,
                    &mut slots,
                    &mut unfinished_tool_calls,
                );
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let Some(delta) = event.get("delta").and_then(Value::as_str) else {
                    continue;
                };
                let index = event_output_index(&event);
                let Some(slot) = slots.get(&index) else {
                    continue;
                };
                if !matches!(slot, SlotKind::Thinking { .. }) {
                    continue;
                }
                let content_index = slot.content_index();
                if let Some(AssistantContentBlock::Thinking(thinking)) =
                    output.content.get_mut(content_index)
                {
                    thinking.thinking.push_str(delta);
                    stream.push(AssistantMessageEvent::ThinkingDelta {
                        content_index,
                        delta: delta.to_string(),
                        partial: output.clone(),
                    });
                }
            }
            "response.reasoning_summary_part.done" => {
                let index = event_output_index(&event);
                let Some(slot) = slots.get(&index) else {
                    continue;
                };
                if !matches!(slot, SlotKind::Thinking { .. }) {
                    continue;
                }
                let content_index = slot.content_index();
                if let Some(AssistantContentBlock::Thinking(thinking)) =
                    output.content.get_mut(content_index)
                {
                    thinking.thinking.push_str("\n\n");
                    stream.push(AssistantMessageEvent::ThinkingDelta {
                        content_index,
                        delta: "\n\n".to_string(),
                        partial: output.clone(),
                    });
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let Some(delta) = event.get("delta").and_then(Value::as_str) else {
                    continue;
                };
                let index = event_output_index(&event);
                let Some(slot) = slots.get(&index) else {
                    continue;
                };
                if !matches!(slot, SlotKind::Text { .. }) {
                    continue;
                }
                let content_index = slot.content_index();
                if let Some(AssistantContentBlock::Text(text)) =
                    output.content.get_mut(content_index)
                {
                    text.text.push_str(delta);
                    stream.push(AssistantMessageEvent::TextDelta {
                        content_index,
                        delta: delta.to_string(),
                        partial: output.clone(),
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                let Some(delta) = event.get("delta").and_then(Value::as_str) else {
                    continue;
                };
                let index = event_output_index(&event);
                let Some(SlotKind::ToolCall {
                    content_index,
                    input: ToolCallInput::Json(arguments),
                }) = slots.get_mut(&index)
                else {
                    continue;
                };
                let content_index = *content_index;
                if let Some(AssistantContentBlock::ToolCall(tool_call)) =
                    output.content.get_mut(content_index)
                {
                    tool_call.arguments = (*arguments.append(delta)).clone();
                    stream.push(AssistantMessageEvent::ToolcallDelta {
                        content_index,
                        delta: delta.to_string(),
                        partial: output.clone(),
                    });
                }
            }
            "response.function_call_arguments.done" => {
                let Some(arguments_text) = event.get("arguments").and_then(Value::as_str) else {
                    continue;
                };
                let index = event_output_index(&event);
                let Some(SlotKind::ToolCall {
                    content_index,
                    input: ToolCallInput::Json(arguments),
                }) = slots.get_mut(&index)
                else {
                    continue;
                };
                let content_index = *content_index;
                let replaced = arguments.replace(arguments_text);
                if let Some(AssistantContentBlock::ToolCall(tool_call)) =
                    output.content.get_mut(content_index)
                {
                    tool_call.arguments = (*replaced.arguments).clone();
                }
                if let Some(delta) = replaced.delta.filter(|delta| !delta.is_empty()) {
                    stream.push(AssistantMessageEvent::ToolcallDelta {
                        content_index,
                        delta,
                        partial: output.clone(),
                    });
                }
            }
            "response.output_item.done" => {
                let index = event_output_index(&event);
                let Some(item) = event.get("item") else {
                    continue;
                };
                if !slots.contains_key(&index) {
                    create_slot(
                        index,
                        item,
                        output,
                        stream,
                        &mut slots,
                        &mut unfinished_tool_calls,
                    );
                }
                let Some(slot) = slots.remove(&index) else {
                    continue;
                };
                match (slot, item.get("type").and_then(Value::as_str)) {
                    (SlotKind::Thinking { content_index }, Some("reasoning")) => {
                        let summary = item_text_list(item, "summary");
                        let content_text = item_text_list(item, "content");
                        if let Some(AssistantContentBlock::Thinking(block)) =
                            output.content.get_mut(content_index)
                        {
                            if !summary.is_empty() {
                                block.thinking = summary;
                            } else if !content_text.is_empty() {
                                block.thinking = content_text;
                            }
                            block.thinking_signature = Some(item.to_string());
                            let thinking = block.thinking.clone();
                            stream.push(AssistantMessageEvent::ThinkingEnd {
                                content_index,
                                content: thinking,
                                partial: output.clone(),
                            });
                        }
                    }
                    (SlotKind::Text { content_index }, Some("message")) => {
                        let text = item
                            .get("content")
                            .and_then(Value::as_array)
                            .map(|parts| {
                                parts
                                    .iter()
                                    .map(|part| match part.get("type").and_then(Value::as_str)
                                    {
                                        Some("output_text") => {
                                            part.get("text").and_then(Value::as_str).unwrap_or("")
                                        }
                                        Some("refusal") => part
                                            .get("refusal")
                                            .and_then(Value::as_str)
                                            .unwrap_or(""),
                                        _ => "",
                                    })
                                    .collect::<Vec<_>>()
                                    .join("")
                            })
                            .unwrap_or_default();
                        let signature = encode_text_signature(
                            item.get("id").and_then(Value::as_str).unwrap_or(""),
                            item.get("phase").and_then(Value::as_str),
                        );
                        if let Some(AssistantContentBlock::Text(block)) =
                            output.content.get_mut(content_index)
                        {
                            block.text = text.clone();
                            block.text_signature = Some(signature);
                        }
                        if item.get("phase").and_then(Value::as_str) == Some("final_answer") {
                            output.stop_reason = StopReason::Stop;
                        }
                        stream.push(AssistantMessageEvent::TextEnd {
                            content_index,
                            content: text,
                            partial: output.clone(),
                        });
                    }
                    (
                        SlotKind::ToolCall {
                            content_index,
                            input: ToolCallInput::Json(mut arguments),
                        },
                        Some("function_call"),
                    ) => {
                        if let Some(final_arguments) =
                            item.get("arguments").and_then(Value::as_str)
                        {
                            arguments.replace(final_arguments);
                        }
                        let parsed = arguments.finish();
                        let mut tool_call = None;
                        if let Some(AssistantContentBlock::ToolCall(block)) =
                            output.content.get_mut(content_index)
                        {
                            block.arguments = parsed.arguments;
                            block.arguments_error = parsed.arguments_error;
                            if let Some(namespace) = item.get("namespace").and_then(Value::as_str) {
                                block.namespace = Some(namespace.to_string());
                            }
                            tool_call = Some(block.clone());
                        }
                        unfinished_tool_calls.remove(&content_index);
                        if let Some(tool_call) = tool_call {
                            stream.push(AssistantMessageEvent::ToolcallEnd {
                                content_index,
                                tool_call,
                                partial: output.clone(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            "response.completed" | "response.incomplete" | "response.done" => {
                saw_terminal_response_event = true;
                let response = event.get("response").cloned().unwrap_or(Value::Null);
                finalize_response(&response, output, model, options);
                // 结束事件就是这一轮的终点；后续 SSE 帧（乃至连接一直不关）都不再等。
                break;
            }
            "error" => {
                // pi: `extractCodexEventError`——错误可以写在外层，也可以写在 `error` 里面。
                let (code, message) = extract_codex_event_error(&event);
                let detail = message
                    .or(code)
                    .unwrap_or_else(|| serde_json::to_string(&event).unwrap_or_default());
                return Err(crate::auth::ModelsError::new(
                    crate::auth::ModelsErrorCode::Stream,
                    format!("Codex error: {detail}"),
                ));
            }
            "response.failed" => {
                let response = event.get("response").cloned().unwrap_or(Value::Null);
                output.raw_stop_reason = response
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let message = response
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Codex response failed");
                return Err(crate::auth::ModelsError::new(
                    crate::auth::ModelsErrorCode::Stream,
                    message,
                ));
            }
            _ => {}
        }
    }

    if !saw_terminal_response_event {
        return Err(crate::auth::ModelsError::login(
            "OpenAI Responses stream ended before a terminal response event",
        ));
    }
    if output.stop_reason == StopReason::ToolUse {
        if let Some(index) = unfinished_tool_calls.iter().next() {
            let description = match &output.content[*index] {
                AssistantContentBlock::ToolCall(tool_call) => {
                    format!("{} ({})", tool_call.name, tool_call.id)
                }
                _ => "an unfinished tool call".to_string(),
            };
            return Err(crate::auth::ModelsError::login(format!(
                "OpenAI Responses stream completed with an unfinished tool call: {description}"
            )));
        }
    }
    Ok(())
}

/// pi: `resolveReasoningEffort`——请求的思考档换成 provider 原生 effort。
pub fn resolve_reasoning_effort(model: &Model, level: ModelThinkingLevel) -> String {
    let mapped = model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(&level))
        .cloned()
        .flatten();
    if let Some(mapped) = mapped {
        return mapped;
    }
    if level == ModelThinkingLevel::Off {
        return "none".to_string();
    }
    match level {
        ModelThinkingLevel::Minimal => "minimal",
        ModelThinkingLevel::Low => "low",
        ModelThinkingLevel::Medium => "medium",
        ModelThinkingLevel::High => "high",
        ModelThinkingLevel::Xhigh => "xhigh",
        ModelThinkingLevel::Max => "max",
        ModelThinkingLevel::Off => "none",
    }
    .to_string()
}

/// 上游事件的 `providerThinkingLevel` 字段在这里也是 `String`（provider 原生 effort 原值）。
pub fn configuration_update_effort(
    model: &Model,
    reasoning: Option<ThinkingLevel>,
    supports_mid_convo_effort: bool,
) -> Option<String> {
    if !supports_mid_convo_effort {
        return None;
    }
    let reasoning = reasoning?;
    Some(resolve_reasoning_effort(model, reasoning.into()))
}

impl From<ThinkingLevel> for ModelThinkingLevel {
    fn from(level: ThinkingLevel) -> Self {
        match level {
            ThinkingLevel::Minimal => Self::Minimal,
            ThinkingLevel::Low => Self::Low,
            ThinkingLevel::Medium => Self::Medium,
            ThinkingLevel::High => Self::High,
            ThinkingLevel::Xhigh => Self::Xhigh,
            ThinkingLevel::Max => Self::Max,
        }
    }
}

/// 上游 `getDeclaredTools` 的再导出（请求构建处要拿工具清单）。
pub fn declared_tools(messages: &[crate::types::Message]) -> Vec<Tool> {
    get_declared_tools(messages)
}
