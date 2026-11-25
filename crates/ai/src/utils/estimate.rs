//! 对应上游 `packages/ai/src/utils/estimate.ts`。
//!
//! 估算上下文用量：最近一条成功的 assistant 消息报过的用量可以当真，它之后的消息按字符数估。
//! `simple-options.ts` 用它给 `max_tokens` 留出输入余量。
//!
//! 字符串长度按 JS 的 `string.length`（UTF-16 单元）数，与上游同一个口径。

use serde::Serialize;

use crate::types::{
    AssistantContentBlock, AssistantMessage, Message, StopReason, ToolResultContentBlock,
    TranscriptContext, Usage, UserContent, UserContentBlock,
};
use crate::utils::text::get_system_message_text;

/// 每多少个字符算一个 token（上游的 `CHARS_PER_TOKEN`）。
const CHARS_PER_TOKEN: f64 = 3.5;

/// 一张图片按多少个字符计（上游的 `ESTIMATED_IMAGE_CHARS`）。
const ESTIMATED_IMAGE_CHARS: usize = 4800;

/// pi: `ContextUsageEstimate`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsageEstimate {
    /// 估算出的上下文总 token 数。
    pub tokens: u64,
    /// 最近一条适用的 assistant 用量块报出的 token 数。
    pub usage_tokens: u64,
    /// 那条之后的估算 token 数。
    pub trailing_tokens: u64,
    /// 提供用量的那条消息下标；没有就是 `None`。
    pub last_usage_index: Option<usize>,
}

/// pi: `calculateContextTokens`
pub fn calculate_context_tokens(usage: &Usage) -> u64 {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage.input + usage.output + usage.cache_read + usage.cache_write
    }
}

fn js_length(text: &str) -> usize {
    text.encode_utf16().count()
}

fn ceil_div(chars: usize) -> u64 {
    (chars as f64 / CHARS_PER_TOKEN).ceil() as u64
}

/// pi: `estimateTextTokens`
pub fn estimate_text_tokens(text: &str) -> u64 {
    ceil_div(js_length(text))
}

fn json_length(value: &impl Serialize) -> usize {
    serde_json::to_string(value)
        .map(|text| js_length(&text))
        .unwrap_or(0)
}

/// pi: `estimateTextAndImageContentTokens`——文字按字符数、图片按固定字符数。
pub fn estimate_text_and_image_content_tokens(content: &(impl EstimatedContent + ?Sized)) -> u64 {
    ceil_div(content.estimated_chars())
}

/// 能估算字符数的内容：user 的 `string | (Text|Image)[]`、toolResult 的 `(Text|Image)[]`。
pub trait EstimatedContent {
    fn estimated_chars(&self) -> usize;
}

impl EstimatedContent for UserContent {
    fn estimated_chars(&self) -> usize {
        match self {
            UserContent::Text(text) => js_length(text),
            UserContent::Blocks(blocks) => blocks.iter().map(|block| block.estimated_chars()).sum(),
        }
    }
}

impl EstimatedContent for UserContentBlock {
    fn estimated_chars(&self) -> usize {
        match self {
            UserContentBlock::Text(block) => js_length(&block.text),
            UserContentBlock::Image(_) => ESTIMATED_IMAGE_CHARS,
        }
    }
}

impl EstimatedContent for [ToolResultContentBlock] {
    fn estimated_chars(&self) -> usize {
        self.iter()
            .map(|block| match block {
                ToolResultContentBlock::Text(block) => js_length(&block.text),
                ToolResultContentBlock::Image(_) => ESTIMATED_IMAGE_CHARS,
            })
            .sum()
    }
}

/// pi: `estimateMessageTokens`
pub fn estimate_message_tokens(message: &Message) -> u64 {
    match message {
        Message::System(message) => {
            estimate_text_tokens(&get_system_message_text(message))
                + estimate_tools_tokens(message.tools_added.as_deref())
                + message
                    .tools_removed
                    .as_deref()
                    .map(|references| estimate_text_tokens(&serde_json::to_string(references).unwrap_or_default()))
                    .unwrap_or(0)
        }
        Message::User(message) => estimate_text_and_image_content_tokens(&message.content),
        Message::ToolResult(message) => estimate_text_and_image_content_tokens(message.content.as_slice()),
        Message::Assistant(message) => {
            let mut chars = 0usize;
            for block in &message.content {
                match block {
                    AssistantContentBlock::Text(block) => chars += js_length(&block.text),
                    AssistantContentBlock::Thinking(block) => chars += js_length(&block.thinking),
                    AssistantContentBlock::ToolCall(block) => {
                        chars += js_length(&block.name);
                        chars += json_length(&block.arguments);
                    }
                }
            }
            ceil_div(chars)
        }
    }
}

/// pi: `estimateToolsTokens`——工具表按 `JSON.stringify` 的长度估。
fn estimate_tools_tokens(tools: Option<&[crate::types::Tool]>) -> u64 {
    match tools {
        None | Some([]) => 0,
        Some(tools) => estimate_text_tokens(&serde_json::to_string(tools).unwrap_or_default()),
    }
}

fn message_timestamp(message: &Message) -> u64 {
    match message {
        Message::System(message) => message.timestamp,
        Message::User(message) => message.timestamp,
        Message::Assistant(message) => message.timestamp,
        Message::ToolResult(message) => message.timestamp,
    }
}

/// pi: `getLastAssistantUsageInfo`
fn get_last_assistant_usage_info(messages: &[Message]) -> Option<(&AssistantMessage, usize)> {
    let mut latest_prefix_timestamp = f64::NEG_INFINITY;
    let mut usage_info: Option<(&AssistantMessage, usize)> = None;
    for (index, message) in messages.iter().enumerate() {
        if let Message::Assistant(assistant) = message {
            // 这条响应之后又插进了更新的前缀消息（例如压缩摘要）时，它的用量不能描述当前前缀。
            let usage_applies_to_prefix = assistant.timestamp as f64 >= latest_prefix_timestamp;
            if usage_applies_to_prefix
                && !matches!(assistant.stop_reason, StopReason::Aborted | StopReason::Error)
                && calculate_context_tokens(&assistant.usage) > 0
            {
                usage_info = Some((assistant, index));
            }
        }
        latest_prefix_timestamp = latest_prefix_timestamp.max(message_timestamp(message) as f64);
    }
    usage_info
}

/// pi: `estimateContextTokens`
pub fn estimate_context_tokens(context: &TranscriptContext) -> ContextUsageEstimate {
    let messages = &context.messages;
    if let Some((assistant, index)) = get_last_assistant_usage_info(messages) {
        let usage_tokens = calculate_context_tokens(&assistant.usage);
        let mut trailing_tokens = 0;
        for message in &messages[index + 1..] {
            trailing_tokens += estimate_message_tokens(message);
        }
        return ContextUsageEstimate {
            tokens: usage_tokens + trailing_tokens,
            usage_tokens,
            trailing_tokens,
            last_usage_index: Some(index),
        };
    }
    let tokens: u64 = messages.iter().map(estimate_message_tokens).sum();
    ContextUsageEstimate {
        tokens,
        usage_tokens: 0,
        trailing_tokens: tokens,
        last_usage_index: None,
    }
}
