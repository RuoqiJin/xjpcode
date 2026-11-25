//! 对应上游 `src/core/compaction/compaction.ts`：上下文压缩的纯函数、准备与摘要生成。
//!
//! 上游的 token 估算用 `chars / 4`（与 `pi-ai` 的 `estimate.ts` 的 `3.5` 是两个口径），这里照
//! 压缩这一份。条目类型、投影与字段名沿用 `session_manager.rs`。

use std::sync::Arc;

use serde_json::{json, Value};
use xjpcode_agent::{AgentMessage, StreamFn, ThinkingLevel};
use xjpcode_ai::providers::model_schema::CacheRetention;
use xjpcode_ai::utils::retry::{retry_assistant_call, RetryCallbacks, RetryPolicy};
use xjpcode_ai::{
    content_text, normalize_context, AssistantContentBlock, Context, Message, Model, ProviderEnv,
    ProviderHeaders, SimpleStreamOptions, StopReason, TextContent, ToolResultContentBlock,
    TranscriptContext, Usage, UserContent, UserContentBlock, UserMessage, UserRole,
};

use crate::core::messages::convert_to_llm;
use crate::core::session_manager::{
    build_session_projection, session_entry_to_context_messages, SessionEntry, SessionProjection,
};
use crate::core::compaction::utils::{
    compute_file_lists, create_file_ops, extract_file_ops_from_message, format_file_operations,
    serialize_conversation, FileOperations, SUMMARIZATION_SYSTEM_PROMPT,
};

/// pi: `CompactionDetails`
pub type CompactionDetails = Value;

/// pi: `CompactionResult`
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionResult {
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: u64,
    pub estimated_tokens_after: Option<u64>,
    pub usage: Option<Usage>,
    pub details: Option<Value>,
}

/// pi: `CompactionSettings`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: u64,
    pub keep_recent_tokens: u64,
}

/// pi: `DEFAULT_COMPACTION_SETTINGS`
pub const DEFAULT_COMPACTION_SETTINGS: CompactionSettings = CompactionSettings {
    enabled: true,
    reserve_tokens: 16384,
    keep_recent_tokens: 20000,
};

/// pi: `calculateContextTokens`
pub fn calculate_context_tokens(usage: &Usage) -> u64 {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage.input + usage.output + usage.cache_read + usage.cache_write
    }
}

fn get_assistant_usage(message: &AgentMessage) -> Option<&Usage> {
    match message {
        AgentMessage::Assistant(assistant)
            if !matches!(assistant.stop_reason, StopReason::Aborted | StopReason::Error)
                && calculate_context_tokens(&assistant.usage) > 0 =>
        {
            Some(&assistant.usage)
        }
        _ => None,
    }
}

/// pi: `getLastAssistantUsage`
pub fn get_last_assistant_usage(entries: &[SessionEntry]) -> Option<Usage> {
    for entry in entries.iter().rev() {
        if let SessionEntry::Message(message) = entry {
            if let Some(usage) = get_assistant_usage(&message.message) {
                return Some(usage.clone());
            }
        }
    }
    None
}

/// pi: `ContextUsageEstimate`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsageEstimate {
    pub tokens: u64,
    pub usage_tokens: u64,
    pub trailing_tokens: u64,
    pub last_usage_index: Option<usize>,
}

fn get_last_assistant_usage_info(messages: &[AgentMessage]) -> Option<(Usage, usize)> {
    for (index, message) in messages.iter().enumerate().rev() {
        if let Some(usage) = get_assistant_usage(message) {
            return Some((usage.clone(), index));
        }
    }
    None
}

/// 图片按多少个字符估（上游压缩用的常量）。
const ESTIMATED_IMAGE_CHARS: usize = 4800;

fn js_length(text: &str) -> usize {
    text.encode_utf16().count()
}

fn estimate_text_and_image_chars(content: &UserContent) -> usize {
    match content {
        UserContent::Text(text) => js_length(text),
        UserContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| match block {
                UserContentBlock::Text(text) => js_length(&text.text),
                UserContentBlock::Image(_) => ESTIMATED_IMAGE_CHARS,
            })
            .sum(),
    }
}

fn estimate_tool_result_chars(content: &[ToolResultContentBlock]) -> usize {
    content
        .iter()
        .map(|block| match block {
            ToolResultContentBlock::Text(text) => js_length(&text.text),
            ToolResultContentBlock::Image(_) => ESTIMATED_IMAGE_CHARS,
        })
        .sum()
}

fn json_length(value: &Value) -> usize {
    js_length(&serde_json::to_string(value).unwrap_or_default())
}

fn custom_content(message: &xjpcode_agent::CustomAgentMessage) -> Option<String> {
    message
        .payload
        .get("content")
        .map(|value| match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
}

fn custom_summary(message: &xjpcode_agent::CustomAgentMessage) -> Option<String> {
    message
        .payload
        .get("summary")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn ceil_div_chars(chars: usize) -> u64 {
    ((chars as f64) / 4.0).ceil() as u64
}

/// pi: `estimateTokens`——按字符数 / 4 估（偏保守）。
pub fn estimate_tokens(message: &AgentMessage) -> u64 {
    match message {
        AgentMessage::System(system) => {
            let mut chars = match &system.content {
                xjpcode_ai::SystemContent::Text(text) => js_length(text),
                xjpcode_ai::SystemContent::Blocks(blocks) => {
                    blocks.iter().map(|block| js_length(&block.text)).sum()
                }
            };
            if let Some(sections) = &system.sections {
                for section in sections.values().flatten() {
                    chars += js_length(section);
                }
            }
            if let Some(tools_added) = &system.tools_added {
                chars += json_length(&serde_json::to_value(tools_added).unwrap_or(Value::Null));
            }
            ceil_div_chars(chars)
        }
        AgentMessage::User(user) => ceil_div_chars(estimate_text_and_image_chars(&user.content)),
        AgentMessage::Assistant(assistant) => {
            let mut chars = 0usize;
            for block in &assistant.content {
                match block {
                    AssistantContentBlock::Text(text) => chars += js_length(&text.text),
                    AssistantContentBlock::Thinking(thinking) => {
                        chars += js_length(&thinking.thinking)
                    }
                    AssistantContentBlock::ToolCall(call) => {
                        chars += js_length(&call.name);
                        chars += json_length(&Value::Object(call.arguments.clone()));
                    }
                }
            }
            ceil_div_chars(chars)
        }
        AgentMessage::ToolResult(result) => {
            ceil_div_chars(estimate_tool_result_chars(&result.content))
        }
        AgentMessage::Custom(custom) => match custom.role.as_str() {
            "custom" => custom_content(custom)
                .map(|content| ceil_div_chars(js_length(&content)))
                .unwrap_or(0),
            "compactionSummary" | "branchSummary" => custom_summary(custom)
                .map(|summary| ceil_div_chars(js_length(&summary)))
                .unwrap_or(0),
            "bashExecution" => {
                let command = custom
                    .payload
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let output = custom
                    .payload
                    .get("output")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                ceil_div_chars(js_length(command) + js_length(output))
            }
            _ => 0,
        },
    }
}

/// pi: `estimateContextTokens`
pub fn estimate_context_tokens(messages: &[AgentMessage]) -> ContextUsageEstimate {
    let Some((usage, index)) = get_last_assistant_usage_info(messages) else {
        let tokens: u64 = messages.iter().map(estimate_tokens).sum();
        return ContextUsageEstimate {
            tokens,
            usage_tokens: 0,
            trailing_tokens: tokens,
            last_usage_index: None,
        };
    };
    let usage_tokens = calculate_context_tokens(&usage);
    let trailing_tokens: u64 = messages[index + 1..].iter().map(estimate_tokens).sum();
    ContextUsageEstimate {
        tokens: usage_tokens + trailing_tokens,
        usage_tokens,
        trailing_tokens,
        last_usage_index: Some(index),
    }
}

/// pi: `estimateProjectedContextTokens`——不轻信更晚的编辑或压缩之前记下的用量。
pub fn estimate_projected_context_tokens(
    projection: &SessionProjection,
    branch_entries: &[SessionEntry],
) -> ContextUsageEstimate {
    let estimate = estimate_context_tokens(&projection.messages);
    if let Some(last_usage_index) = estimate.last_usage_index {
        let mut projected_index = 0;
        let mut usage_entry_id: Option<&str> = None;
        for entry in &projection.entries {
            let next_index = projected_index + entry.messages.len();
            if last_usage_index < next_index {
                usage_entry_id = Some(entry.source_entry.id());
                break;
            }
            projected_index = next_index;
        }
        let usage_entry_index = usage_entry_id
            .and_then(|id| branch_entries.iter().position(|entry| entry.id() == id))
            .map(|index| index as isize)
            .unwrap_or(-1);
        let latest_invalidating_index = branch_entries
            .iter()
            .rposition(|entry| matches!(entry, SessionEntry::ContextEdit(_) | SessionEntry::Compaction(_)))
            .map(|index| index as isize)
            .unwrap_or(-1);
        if usage_entry_index > latest_invalidating_index {
            return estimate;
        }
    }

    let system_messages: Vec<Message> = projection
        .messages
        .iter()
        .filter_map(|message| match message {
            AgentMessage::System(system) => Some(Message::System(system.clone())),
            _ => None,
        })
        .collect();
    let current_system = xjpcode_ai::get_current_system_message(&system_messages);
    let mut tokens = current_system
        .as_ref()
        .map(|message| estimate_tokens(&AgentMessage::System(message.clone())))
        .unwrap_or(0);
    for message in &projection.messages {
        if message.role() != "system" {
            tokens += estimate_tokens(message);
        }
    }
    ContextUsageEstimate {
        tokens,
        usage_tokens: 0,
        trailing_tokens: tokens,
        last_usage_index: None,
    }
}

/// pi: `shouldCompact`
pub fn should_compact(
    context_tokens: u64,
    context_window: u64,
    settings: &CompactionSettings,
) -> bool {
    if !settings.enabled {
        return false;
    }
    context_tokens > context_window.saturating_sub(settings.reserve_tokens)
}

fn is_cut_point_message(message: &AgentMessage) -> bool {
    matches!(
        message.role(),
        "user" | "assistant" | "bashExecution" | "custom" | "branchSummary" | "compactionSummary"
    )
}

fn is_turn_start_message(message: &AgentMessage) -> bool {
    matches!(
        message.role(),
        "user" | "bashExecution" | "custom" | "branchSummary" | "compactionSummary"
    )
}

fn is_turn_start_entry(entry: &SessionEntry) -> bool {
    if matches!(entry, SessionEntry::Compaction(_)) {
        return false;
    }
    session_entry_to_context_messages(entry)
        .iter()
        .any(is_turn_start_message)
}

/// pi: `findTurnStartIndex`
pub fn find_turn_start_index(entries: &[SessionEntry], entry_index: usize, start_index: usize) -> Option<usize> {
    for index in (start_index..=entry_index).rev() {
        if is_turn_start_entry(&entries[index]) {
            return Some(index);
        }
    }
    None
}

/// pi: `CutPointResult`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutPointResult {
    pub first_kept_entry_index: usize,
    pub turn_start_index: Option<usize>,
    pub is_split_turn: bool,
}

/// pi: `findCutPoint`——从新往旧累加，到了 `keepRecentTokens` 就在最近的合法切点切。
pub fn find_cut_point(
    entries: &[SessionEntry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: u64,
) -> CutPointResult {
    let mut cut_points: Vec<usize> = Vec::new();
    for index in start_index..end_index {
        let entry = &entries[index];
        if matches!(entry, SessionEntry::Compaction(_)) {
            continue;
        }
        if session_entry_to_context_messages(entry)
            .iter()
            .any(is_cut_point_message)
        {
            cut_points.push(index);
        }
    }

    let Some(first_cut_point) = cut_points.first().copied() else {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: None,
            is_split_turn: false,
        };
    };

    let mut accumulated_tokens = 0u64;
    let mut cut_index = first_cut_point;
    for index in (start_index..end_index).rev() {
        let message_tokens: u64 = session_entry_to_context_messages(&entries[index])
            .iter()
            .map(estimate_tokens)
            .sum();
        if message_tokens == 0 {
            continue;
        }
        accumulated_tokens += message_tokens;
        if accumulated_tokens >= keep_recent_tokens {
            cut_index = cut_points
                .iter()
                .copied()
                .find(|candidate| *candidate >= index)
                .unwrap_or_else(|| *cut_points.last().expect("cut point"));
            break;
        }
    }

    while cut_index > start_index {
        let previous = &entries[cut_index - 1];
        if matches!(previous, SessionEntry::Compaction(_))
            || !session_entry_to_context_messages(previous).is_empty()
        {
            break;
        }
        cut_index -= 1;
    }

    let cut_entry = &entries[cut_index];
    let starts_turn = is_turn_start_entry(cut_entry);
    let turn_start_index = if starts_turn {
        None
    } else {
        find_turn_start_index(entries, cut_index, start_index)
    };
    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !starts_turn && turn_start_index.is_some(),
    }
}

// ============================================================================
// 摘要提示词
// ============================================================================

const SUMMARIZATION_PROMPT: &str = r#"The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or "(none)" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

const UPDATE_SUMMARIZATION_INSTRUCTIONS: &str = r#"Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from "In Progress" to "Done" when completed
- UPDATE "Next Steps" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

const UPDATE_SUMMARIZATION_PROMPT: &str = "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.";

const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = r#"The messages above are earlier context from an ongoing conversation. Later messages are stored separately and do not need to be reconstructed.

Create a concise checkpoint of the user's request and the progress shown above. This checkpoint will be placed before the later messages so the conversation can continue with the necessary context.

## Original Request
[What did the user ask for?]

## Progress So Far
- [Key decisions and work completed in these messages]

## Context Needed to Continue
- [Information from these messages needed to understand the later work]

Only summarize information explicitly present above. Do not infer or recreate later messages."#;

/// pi: `getSummarizationFailure`
pub fn get_summarization_failure(response: &xjpcode_ai::AssistantMessage, label: &str) -> Option<String> {
    match response.stop_reason {
        StopReason::Error => Some(format!(
            "{label} failed: {}",
            response
                .error_message
                .clone()
                .unwrap_or_else(|| "Unknown error".to_string())
        )),
        StopReason::Length => Some(format!(
            "{label} failed: generation hit the token cap and the summary is incomplete"
        )),
        _ => None,
    }
}

fn create_summarization_options(
    model: &Model,
    api_key: Option<String>,
    headers: Option<ProviderHeaders>,
    env: Option<ProviderEnv>,
    thinking_level: Option<ThinkingLevel>,
) -> SimpleStreamOptions {
    let mut options = SimpleStreamOptions {
        api_key,
        headers,
        env,
        ..SimpleStreamOptions::default()
    };
    if model.reasoning {
        if let Some(level) = thinking_level {
            options.reasoning = xjpcode_agent::reasoning_from_thinking_level(level);
        }
    }
    options
}

/// pi: `completeSummarization`——一次独立的摘要请求，临时故障按策略重试。
pub async fn complete_summarization(
    model: Model,
    context: TranscriptContext,
    options: SimpleStreamOptions,
    signal: Option<xjpcode_ai::AbortSignal>,
    stream_fn: &StreamFn,
    retry: Option<&RetryPolicy>,
    callbacks: &RetryCallbacks,
) -> xjpcode_ai::AssistantMessage {
    let mut request_options = options;
    request_options.cache_retention = Some(CacheRetention::None);
    if request_options.session_id.is_none() {
        request_options.session_id = Some(uuid::Uuid::now_v7().to_string());
    }
    let stream_fn = Arc::clone(stream_fn);
    let model_for_call = model.clone();
    let context_for_call = context.clone();
    let options_for_call = request_options.clone();
    let signal_for_call = signal.clone();
    let produce = move || {
        let stream_fn = Arc::clone(&stream_fn);
        let model = model_for_call.clone();
        let context = context_for_call.clone();
        let options = options_for_call.clone();
        let signal = signal_for_call.clone();
        Box::pin(async move {
            let stream = stream_fn(
                model,
                context,
                xjpcode_agent::StreamFnOptions {
                    options,
                    signal,
                },
            );
            stream.result().await
        }) as xjpcode_ai::BoxFuture<'static, xjpcode_ai::AssistantMessage>
    };
    retry_assistant_call(produce, retry, signal.as_ref(), callbacks).await
}

fn build_summarization_context(prompt_text: String) -> TranscriptContext {
    normalize_context(Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_string()),
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(
                prompt_text,
            ))]),
            timestamp: xjpcode_agent::now_ms(),
        })],
        tools: None,
    })
}

/// pi: `generateSummaryWithUsage`
#[allow(clippy::too_many_arguments)]
pub async fn generate_summary_with_usage(
    current_messages: &[AgentMessage],
    model: Model,
    api_key: Option<String>,
    headers: Option<ProviderHeaders>,
    signal: Option<xjpcode_ai::AbortSignal>,
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
    thinking_level: Option<ThinkingLevel>,
    stream_fn: &StreamFn,
    env: Option<ProviderEnv>,
    retry: Option<&RetryPolicy>,
    callbacks: &RetryCallbacks,
    session_id: Option<String>,
) -> Result<(String, Usage), String> {
    let mut base_prompt = if previous_summary.is_some() {
        format!("{UPDATE_SUMMARIZATION_PROMPT}\n\n{UPDATE_SUMMARIZATION_INSTRUCTIONS}")
    } else {
        SUMMARIZATION_PROMPT.to_string()
    };
    if let Some(custom_instructions) = custom_instructions {
        base_prompt = format!("{base_prompt}\n\nAdditional focus: {custom_instructions}");
    }

    let llm_messages = convert_to_llm(current_messages);
    let conversation_text = serialize_conversation(&llm_messages);

    let mut prompt_text = format!("<conversation>\n{conversation_text}\n</conversation>\n\n");
    if let Some(previous_summary) = previous_summary {
        prompt_text.push_str(&format!(
            "<previous-summary>\n{previous_summary}\n</previous-summary>\n\n"
        ));
    }
    prompt_text.push_str(&base_prompt);

    let mut options = create_summarization_options(
        &model,
        api_key,
        headers,
        env,
        thinking_level,
    );
    options.session_id = session_id;
    let response = complete_summarization(
        model,
        build_summarization_context(prompt_text),
        options,
        signal,
        stream_fn,
        retry,
        callbacks,
    )
    .await;

    if let Some(failure) = get_summarization_failure(&response, "Summarization") {
        return Err(failure);
    }
    if response
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
    {
        return Err("Summarization attempted to call a tool".to_string());
    }

    Ok((
        content_text(&response.content, ""),
        response.usage.clone(),
    ))
}

// ============================================================================
// 准备与执行
// ============================================================================

/// pi: `CompactionPreparation`
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionPreparation {
    pub first_kept_entry_id: String,
    pub messages_to_summarize: Vec<AgentMessage>,
    pub turn_prefix_messages: Vec<AgentMessage>,
    pub is_split_turn: bool,
    pub tokens_before: u64,
    pub previous_summary: Option<String>,
    pub file_ops: FileOperations,
    pub settings: CompactionSettings,
}

fn projection_for_path(entries: &[SessionEntry]) -> SessionProjection {
    let by_id: std::collections::HashMap<String, SessionEntry> = entries
        .iter()
        .map(|entry| (entry.id().to_string(), entry.clone()))
        .collect();
    let leaf_id = entries.last().map(|entry| entry.id().to_string());
    build_session_projection(entries, leaf_id.as_deref(), &by_id)
}

fn projected_entry_is_intrinsically_visible(
    entry: &crate::core::session_manager::ProjectedSessionEntry,
) -> bool {
    !matches!(entry.source_entry, SessionEntry::ContextEdit(_))
        && !session_entry_to_context_messages(&entry.source_entry).is_empty()
}

fn is_projectedly_omitted(entry: &crate::core::session_manager::ProjectedSessionEntry) -> bool {
    projected_entry_is_intrinsically_visible(entry) && entry.messages.is_empty()
}

fn is_projected_turn_start(entry: &crate::core::session_manager::ProjectedSessionEntry) -> bool {
    if matches!(entry.source_entry, SessionEntry::Compaction(_)) {
        return false;
    }
    entry.messages.iter().any(is_turn_start_message)
}

fn find_projected_turn_start_index(
    entries: &[crate::core::session_manager::ProjectedSessionEntry],
    entry_index: usize,
    start_index: usize,
) -> Option<usize> {
    for index in (start_index..=entry_index).rev() {
        if is_projected_turn_start(&entries[index]) {
            return Some(index);
        }
    }
    None
}

fn find_projected_cut_point(
    entries: &[crate::core::session_manager::ProjectedSessionEntry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: u64,
) -> CutPointResult {
    let mut cut_points: Vec<usize> = Vec::new();
    for index in start_index..end_index {
        let entry = &entries[index];
        if !matches!(entry.source_entry, SessionEntry::Compaction(_))
            && entry.messages.iter().any(is_cut_point_message)
        {
            cut_points.push(index);
        }
    }
    let Some(first_cut_point) = cut_points.first().copied() else {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: None,
            is_split_turn: false,
        };
    };

    let mut accumulated_tokens = 0u64;
    let mut exceeded_budget = false;
    let mut cut_index = first_cut_point;
    for index in (start_index..end_index).rev() {
        let message_tokens: u64 = entries[index].messages.iter().map(estimate_tokens).sum();
        if message_tokens == 0 {
            continue;
        }
        accumulated_tokens += message_tokens;
        if accumulated_tokens >= keep_recent_tokens {
            exceeded_budget = true;
            cut_index = cut_points
                .iter()
                .copied()
                .find(|candidate| *candidate >= index)
                .unwrap_or_else(|| *cut_points.last().expect("cut point"));
            break;
        }
    }

    let suffix = &entries[cut_index + 1..end_index];
    let omitted_suffix_ids: std::collections::HashSet<&str> = suffix
        .iter()
        .filter(|entry| is_projectedly_omitted(entry))
        .map(|entry| entry.source_entry.id())
        .collect();
    let has_external_replacement = suffix.iter().any(|entry| match &entry.source_entry {
        SessionEntry::ContextEdit(edit) => {
            edit.replacement.is_some() && !omitted_suffix_ids.contains(edit.target_id.as_str())
        }
        _ => false,
    });
    let is_recovery_omission_suffix = exceeded_budget
        && !has_external_replacement
        && suffix.iter().any(|entry| match &entry.source_entry {
            SessionEntry::Message(message) => {
                matches!(message.message, AgentMessage::Assistant(_)) && is_projectedly_omitted(entry)
            }
            _ => false,
        })
        && suffix.iter().all(|entry| {
            !matches!(entry.source_entry, SessionEntry::Compaction(_))
                && (!projected_entry_is_intrinsically_visible(entry)
                    || is_projectedly_omitted(entry))
        });
    if is_recovery_omission_suffix {
        cut_index += 1;
    }

    while cut_index > start_index {
        let previous = &entries[cut_index - 1];
        if matches!(previous.source_entry, SessionEntry::Compaction(_))
            || !previous.messages.is_empty()
        {
            break;
        }
        cut_index -= 1;
    }

    let starts_turn = is_projected_turn_start(&entries[cut_index]);
    let turn_start_index = if starts_turn {
        None
    } else {
        find_projected_turn_start_index(entries, cut_index, start_index)
    };
    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !starts_turn && turn_start_index.is_some(),
    }
}

fn get_messages_from_projected_entry_for_compaction(
    entry: &crate::core::session_manager::ProjectedSessionEntry,
) -> Vec<AgentMessage> {
    if matches!(entry.source_entry, SessionEntry::Compaction(_)) {
        return Vec::new();
    }
    entry
        .messages
        .iter()
        .filter(|message| message.role() != "system")
        .cloned()
        .collect()
}

fn extract_file_operations(
    messages: &[AgentMessage],
    entries: &[SessionEntry],
    prev_compaction_index: Option<usize>,
) -> FileOperations {
    let mut ops = create_file_ops();
    if let Some(index) = prev_compaction_index {
        if let SessionEntry::Compaction(compaction) = &entries[index] {
            if !compaction.from_hook.unwrap_or(false) {
                if let Some(details) = &compaction.details {
                    if let Some(read_files) = details.get("readFiles").and_then(Value::as_array) {
                        for file in read_files.iter().filter_map(Value::as_str) {
                            ops.read.insert(file.to_string());
                        }
                    }
                    if let Some(modified_files) = details.get("modifiedFiles").and_then(Value::as_array)
                    {
                        for file in modified_files.iter().filter_map(Value::as_str) {
                            ops.edited.insert(file.to_string());
                        }
                    }
                }
            }
        }
    }
    for message in messages {
        extract_file_ops_from_message(message, &mut ops);
    }
    ops
}

/// pi: `prepareCompaction`
pub fn prepare_compaction(
    path_entries: &[SessionEntry],
    settings: CompactionSettings,
) -> Option<CompactionPreparation> {
    if matches!(path_entries.last(), Some(SessionEntry::Compaction(_))) {
        return None;
    }

    let projection = projection_for_path(path_entries);
    let projected_entries = &projection.entries;
    let source_entries: Vec<SessionEntry> = projected_entries
        .iter()
        .map(|entry| entry.source_entry.clone())
        .collect();
    let prev_compaction_index = projected_entries.iter().position(|entry| {
        matches!(entry.source_entry, SessionEntry::Compaction(_)) && !entry.messages.is_empty()
    });

    let mut previous_summary = None;
    let mut boundary_start = 0;
    if let Some(index) = prev_compaction_index {
        if let SessionEntry::Compaction(compaction) = &projected_entries[index].source_entry {
            previous_summary = Some(compaction.summary.clone());
        }
        boundary_start = index + 1;
    }
    let boundary_end = projected_entries.len();
    let tokens_before = estimate_projected_context_tokens(&projection, path_entries).tokens;
    let cut_point = find_projected_cut_point(
        projected_entries,
        boundary_start,
        boundary_end,
        settings.keep_recent_tokens,
    );

    let first_kept_entry = projected_entries.get(cut_point.first_kept_entry_index)?;
    let first_kept_entry_id = first_kept_entry.source_entry.id().to_string();
    let history_end = if cut_point.is_split_turn {
        cut_point.turn_start_index.unwrap_or(cut_point.first_kept_entry_index)
    } else {
        cut_point.first_kept_entry_index
    };

    let messages_to_summarize: Vec<AgentMessage> = projected_entries[boundary_start..history_end]
        .iter()
        .flat_map(get_messages_from_projected_entry_for_compaction)
        .collect();
    let turn_prefix_messages: Vec<AgentMessage> = if cut_point.is_split_turn {
        projected_entries[cut_point
            .turn_start_index
            .unwrap_or(cut_point.first_kept_entry_index)
            ..cut_point.first_kept_entry_index]
            .iter()
            .flat_map(get_messages_from_projected_entry_for_compaction)
            .collect()
    } else {
        Vec::new()
    };

    if messages_to_summarize.is_empty() && turn_prefix_messages.is_empty() {
        return None;
    }

    let mut file_ops = extract_file_operations(&messages_to_summarize, &source_entries, prev_compaction_index);
    if cut_point.is_split_turn {
        for message in &turn_prefix_messages {
            extract_file_ops_from_message(message, &mut file_ops);
        }
    }

    Some(CompactionPreparation {
        first_kept_entry_id,
        messages_to_summarize,
        turn_prefix_messages,
        is_split_turn: cut_point.is_split_turn,
        tokens_before,
        previous_summary,
        file_ops,
        settings,
    })
}

/// pi: 两份 usage 相加。
pub fn combine_usage(left: &Usage, right: &Usage) -> Usage {
    Usage {
        input: left.input + right.input,
        output: left.output + right.output,
        cache_read: left.cache_read + right.cache_read,
        cache_write: left.cache_write + right.cache_write,
        cache_write_1h: match (left.cache_write_1h, right.cache_write_1h) {
            (None, None) => None,
            (left, right) => Some(left.unwrap_or(0) + right.unwrap_or(0)),
        },
        reasoning: match (left.reasoning, right.reasoning) {
            (None, None) => None,
            (left, right) => Some(left.unwrap_or(0) + right.unwrap_or(0)),
        },
        total_tokens: left.total_tokens + right.total_tokens,
        cost: xjpcode_ai::UsageCost {
            input: left.cost.input + right.cost.input,
            output: left.cost.output + right.cost.output,
            cache_read: left.cost.cache_read + right.cost.cache_read,
            cache_write: left.cost.cache_write + right.cost.cache_write,
            total: left.cost.total + right.cost.total,
            total_from_provider: left.cost.total_from_provider.or(right.cost.total_from_provider),
        },
    }
}

async fn generate_turn_prefix_summary(
    messages: &[AgentMessage],
    model: Model,
    api_key: Option<String>,
    headers: Option<ProviderHeaders>,
    env: Option<ProviderEnv>,
    signal: Option<xjpcode_ai::AbortSignal>,
    thinking_level: Option<ThinkingLevel>,
    stream_fn: &StreamFn,
    retry: Option<&RetryPolicy>,
    callbacks: &RetryCallbacks,
    session_id: Option<String>,
    custom_instructions: Option<&str>,
) -> Result<(String, Usage), String> {
    let llm_messages = convert_to_llm(messages);
    let conversation_text = serialize_conversation(&llm_messages);
    let mut prompt_text = format!(
        "# Conversation\n{conversation_text}\n\n# Instructions\n{TURN_PREFIX_SUMMARIZATION_PROMPT}"
    );
    if let Some(custom_instructions) = custom_instructions {
        prompt_text = format!("{prompt_text}\n\nAdditional focus: {custom_instructions}");
    }

    let mut options = create_summarization_options(&model, api_key, headers, env, thinking_level);
    options.session_id = session_id;
    let response = complete_summarization(
        model,
        build_summarization_context(prompt_text),
        options,
        signal,
        stream_fn,
        retry,
        callbacks,
    )
    .await;

    if let Some(failure) = get_summarization_failure(&response, "Turn prefix summarization") {
        return Err(failure);
    }
    if response
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
    {
        return Err("Turn prefix summarization attempted to call a tool".to_string());
    }
    Ok((
        content_text(&response.content, ""),
        response.usage.clone(),
    ))
}

/// pi: `compact`——按准备好的数据生成摘要，返回要写进会话条目的结果。
#[allow(clippy::too_many_arguments)]
pub async fn compact(
    preparation: &CompactionPreparation,
    model: Model,
    api_key: Option<String>,
    headers: Option<ProviderHeaders>,
    custom_instructions: Option<&str>,
    signal: Option<xjpcode_ai::AbortSignal>,
    thinking_level: Option<ThinkingLevel>,
    stream_fn: &StreamFn,
    env: Option<ProviderEnv>,
    retry: Option<&RetryPolicy>,
    callbacks: &RetryCallbacks,
    session_id: Option<String>,
) -> Result<CompactionResult, String> {
    let mut summary;
    let summary_usage;

    if preparation.is_split_turn && !preparation.turn_prefix_messages.is_empty() {
        let mut history_text = preparation
            .previous_summary
            .clone()
            .unwrap_or_else(|| "No prior history.".to_string());
        let mut history_usage: Option<Usage> = None;
        if !preparation.messages_to_summarize.is_empty() {
            let (text, usage) = generate_summary_with_usage(
                &preparation.messages_to_summarize,
                model.clone(),
                api_key.clone(),
                headers.clone(),
                signal.clone(),
                custom_instructions,
                preparation.previous_summary.as_deref(),
                thinking_level,
                stream_fn,
                env.clone(),
                retry,
                callbacks,
                session_id.clone(),
            )
            .await?;
            history_text = text;
            history_usage = Some(usage);
        }
        let (turn_text, turn_usage) = generate_turn_prefix_summary(
            &preparation.turn_prefix_messages,
            model,
            api_key,
            headers,
            env,
            signal,
            thinking_level,
            stream_fn,
            retry,
            callbacks,
            session_id,
            custom_instructions,
        )
        .await?;
        summary = format!("{history_text}\n\n---\n\n**Turn Context (split turn):**\n\n{turn_text}");
        summary_usage = match history_usage {
            Some(history_usage) => combine_usage(&history_usage, &turn_usage),
            None => turn_usage,
        };
    } else {
        let (text, usage) = generate_summary_with_usage(
            &preparation.messages_to_summarize,
            model,
            api_key,
            headers,
            signal,
            custom_instructions,
            preparation.previous_summary.as_deref(),
            thinking_level,
            stream_fn,
            env,
            retry,
            callbacks,
            session_id,
        )
        .await?;
        summary = text;
        summary_usage = usage;
    }

    let (read_files, modified_files) = compute_file_lists(&preparation.file_ops);
    summary.push_str(&format_file_operations(&read_files, &modified_files));
    if preparation.first_kept_entry_id.is_empty() {
        return Err("First kept entry has no UUID - session may need migration".to_string());
    }

    Ok(CompactionResult {
        summary,
        first_kept_entry_id: preparation.first_kept_entry_id.clone(),
        tokens_before: preparation.tokens_before,
        estimated_tokens_after: None,
        usage: Some(summary_usage),
        details: Some(json!({
            "readFiles": read_files,
            "modifiedFiles": modified_files,
        })),
    })
}

/// 只给测试：把一份已经写好的摘要装成结果。
pub fn compaction_result_with_summary(
    preparation: &CompactionPreparation,
    summary: &str,
) -> CompactionResult {
    let (read_files, modified_files) = compute_file_lists(&preparation.file_ops);
    let mut summary = summary.to_string();
    summary.push_str(&format_file_operations(&read_files, &modified_files));
    CompactionResult {
        summary,
        first_kept_entry_id: preparation.first_kept_entry_id.clone(),
        tokens_before: preparation.tokens_before,
        estimated_tokens_after: None,
        usage: None,
        details: Some(json!({
            "readFiles": read_files,
            "modifiedFiles": modified_files,
        })),
    }
}
