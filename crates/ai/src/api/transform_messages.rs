//! 对应上游 `packages/ai/src/api/transform-messages.ts`。
//!
//! 把对话记录整理成某一家 provider 能接受的样子：别的模型产生的思考块丢掉、不支持的图片换成
//! 一句说明、被中断的 assistant 轮跳过、没有结果的工具调用补一条 `No result provided`、
//! 夹在工具调用与结果之间的系统消息挪到结果之后。
//!
//! 上游的 `normalizeToolCallId` 还处理 OpenAI Responses 那种 `call_id|item_id` 的 id（要
//! `shortHash`）；本次不接那条路（见 `PORTING.md` 差异表 #113），id 原样保留。

use crate::types::{
    AssistantContentBlock, Message, Model, TextContent, ToolResultContentBlock, ToolResultMessage,
    ToolResultRole, UserContent, UserContentBlock,
};

/// 上游的 `Date.now()`。
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str = "(tool image omitted: model does not support images)";

/// 把一串内容里的图片换成一句占位说明；连续多张只留一句（上游的
/// `replaceImagesWithPlaceholder`）。
fn replace_images_with_placeholder(
    content: &[UserContentBlock],
    placeholder: &str,
) -> Vec<UserContentBlock> {
    let mut result = Vec::new();
    let mut previous_was_placeholder = false;
    for block in content {
        match block {
            UserContentBlock::Image(_) => {
                if !previous_was_placeholder {
                    result.push(UserContentBlock::Text(TextContent::new(placeholder)));
                }
                previous_was_placeholder = true;
            }
            UserContentBlock::Text(block) => {
                previous_was_placeholder = block.text == placeholder;
                result.push(UserContentBlock::Text(block.clone()));
            }
        }
    }
    result
}

fn replace_tool_images_with_placeholder(
    content: &[ToolResultContentBlock],
    placeholder: &str,
) -> Vec<ToolResultContentBlock> {
    let mut result = Vec::new();
    let mut previous_was_placeholder = false;
    for block in content {
        match block {
            ToolResultContentBlock::Image(_) => {
                if !previous_was_placeholder {
                    result.push(ToolResultContentBlock::Text(TextContent::new(placeholder)));
                }
                previous_was_placeholder = true;
            }
            ToolResultContentBlock::Text(block) => {
                previous_was_placeholder = block.text == placeholder;
                result.push(ToolResultContentBlock::Text(block.clone()));
            }
        }
    }
    result
}

/// pi: `downgradeUnsupportedImages`
fn downgrade_unsupported_images(messages: &[Message], model: &Model) -> Vec<Message> {
    if model
        .input
        .iter()
        .any(|modality| *modality == crate::providers::model_schema::ModelInputModality::Image)
    {
        return messages.to_vec();
    }
    messages
        .iter()
        .map(|message| match message {
            Message::User(user) => {
                let content = match &user.content {
                    UserContent::Text(text) => UserContent::Text(text.clone()),
                    UserContent::Blocks(blocks) => UserContent::Blocks(replace_images_with_placeholder(
                        blocks,
                        NON_VISION_USER_IMAGE_PLACEHOLDER,
                    )),
                };
                Message::User(crate::types::UserMessage {
                    role: user.role,
                    content,
                    timestamp: user.timestamp,
                })
            }
            Message::ToolResult(result) => Message::ToolResult(ToolResultMessage {
                content: replace_tool_images_with_placeholder(
                    &result.content,
                    NON_VISION_TOOL_IMAGE_PLACEHOLDER,
                ),
                ..result.clone()
            }),
            other => other.clone(),
        })
        .collect()
}

/// pi: `transformMessages`——`normalize_tool_call_id` 传 `None` 就是上游省略它的那条路。
pub fn transform_messages(
    messages: &[Message],
    model: &Model,
    normalize_tool_call_id: Option<&dyn Fn(&str, &Model, &crate::types::AssistantMessage) -> String>,
) -> Vec<Message> {
    let image_aware_messages = downgrade_unsupported_images(messages, model);

    // 第一趟：换图片、丢别的模型的思考块、归一工具调用 id。
    let mut tool_call_id_map: std::collections::HashMap<String, String> = Default::default();
    let transformed: Vec<Message> = image_aware_messages
        .iter()
        .map(|message| match message {
            Message::Assistant(assistant) => {
                let is_same_model = assistant.provider == model.provider
                    && assistant.api == model.api
                    && assistant.model == model.id;
                let content: Vec<AssistantContentBlock> = assistant
                    .content
                    .iter()
                    .flat_map(|block| match block {
                        AssistantContentBlock::Thinking(block) => {
                            if !is_same_model {
                                return Vec::new();
                            }
                            if block.redacted == Some(true) || block.thinking_signature.is_some() {
                                return vec![AssistantContentBlock::Thinking(block.clone())];
                            }
                            if block.thinking.trim().is_empty() {
                                return Vec::new();
                            }
                            vec![AssistantContentBlock::Thinking(block.clone())]
                        }
                        AssistantContentBlock::Text(block) => {
                            if is_same_model {
                                vec![AssistantContentBlock::Text(block.clone())]
                            } else {
                                vec![AssistantContentBlock::Text(TextContent::new(block.text.clone()))]
                            }
                        }
                        AssistantContentBlock::ToolCall(tool_call) => {
                            let mut normalized = tool_call.clone();
                            if !is_same_model && tool_call.thought_signature.is_some() {
                                normalized.thought_signature = None;
                            }
                            if !is_same_model {
                                if let Some(normalize) = normalize_tool_call_id {
                                    let normalized_id = normalize(&tool_call.id, model, assistant);
                                    if normalized_id != tool_call.id {
                                        tool_call_id_map
                                            .insert(tool_call.id.clone(), normalized_id.clone());
                                        normalized.id = normalized_id;
                                    }
                                }
                            }
                            vec![AssistantContentBlock::ToolCall(normalized)]
                        }
                    })
                    .collect();
                Message::Assistant(crate::types::AssistantMessage {
                    content,
                    ..assistant.clone()
                })
            }
            Message::ToolResult(result) => match tool_call_id_map.get(&result.tool_call_id) {
                Some(normalized_id) if normalized_id != &result.tool_call_id => {
                    Message::ToolResult(ToolResultMessage {
                        tool_call_id: normalized_id.clone(),
                        ..result.clone()
                    })
                }
                _ => message.clone(),
            },
            other => other.clone(),
        })
        .collect();

    // 第二趟：给没有结果的工具调用补一条错误结果，满足 API 的形状要求。
    let mut result: Vec<Message> = Vec::new();
    let mut pending_tool_calls: Vec<crate::types::ToolCall> = Vec::new();
    let mut existing_tool_result_ids: std::collections::HashSet<String> = Default::default();
    // 系统消息对工具调用的账目是透明的：夹在调用与结果之间的那条先压着，
    // 等（含补造的）结果都发完再发。
    let mut held_system_messages: Vec<Message> = Vec::new();

    for message in &transformed {
        match message {
            Message::Assistant(assistant) => {
                close_pending_tool_calls(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                    &mut held_system_messages,
                );
                // 失败或被中断的 assistant 轮整条跳过：它是不完整的轮次。
                if matches!(
                    assistant.stop_reason,
                    crate::types::StopReason::Error | crate::types::StopReason::Aborted
                ) {
                    continue;
                }
                let tool_calls: Vec<crate::types::ToolCall> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::ToolCall(tool_call) => Some(tool_call.clone()),
                        _ => None,
                    })
                    .collect();
                if !tool_calls.is_empty() {
                    pending_tool_calls = tool_calls;
                    existing_tool_result_ids.clear();
                }
                result.push(message.clone());
            }
            Message::ToolResult(tool_result) => {
                existing_tool_result_ids.insert(tool_result.tool_call_id.clone());
                result.push(message.clone());
            }
            Message::System(_) => {
                if pending_tool_calls.is_empty() {
                    result.push(message.clone());
                } else {
                    held_system_messages.push(message.clone());
                }
            }
            Message::User(_) => {
                close_pending_tool_calls(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                    &mut held_system_messages,
                );
                result.push(message.clone());
            }
        }
    }
    close_pending_tool_calls(
        &mut result,
        &mut pending_tool_calls,
        &mut existing_tool_result_ids,
        &mut held_system_messages,
    );
    result
}

fn close_pending_tool_calls(
    result: &mut Vec<Message>,
    pending_tool_calls: &mut Vec<crate::types::ToolCall>,
    existing_tool_result_ids: &mut std::collections::HashSet<String>,
    held_system_messages: &mut Vec<Message>,
) {
    if !pending_tool_calls.is_empty() {
        for tool_call in pending_tool_calls.iter() {
            if !existing_tool_result_ids.contains(&tool_call.id) {
                result.push(Message::ToolResult(ToolResultMessage {
                    role: ToolResultRole::ToolResult,
                    tool_call_id: tool_call.id.clone(),
                    tool_name: tool_call.name.clone(),
                    content: vec![ToolResultContentBlock::Text(TextContent::new("No result provided"))],
                    details: None,
                    usage: None,
                    nested_calls: None,
                    is_error: true,
                    timestamp: now_ms(),
                    duration_ms: None,
                }));
            }
        }
        pending_tool_calls.clear();
        existing_tool_result_ids.clear();
    }
    result.append(held_system_messages);
}

/// 供上层复用的类型别名（上游没有；只为让调用处读得懂）。
pub type NormalizeToolCallId<'a> =
    dyn Fn(&str, &Model, &crate::types::AssistantMessage) -> String + 'a;
