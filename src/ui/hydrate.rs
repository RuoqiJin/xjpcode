//! 从会话消息重画界面卡片（接着聊时用）。
//!
//! 工具调用在 assistant 的 content 里，工具结果另是一条消息；按 tool_call_id 对上。

use std::collections::HashMap;

use xjpcode_agent::AgentMessage;
use xjpcode_ai::{AssistantContentBlock, ToolResultContentBlock};

use super::state::App;

fn pretty_arguments(arguments: &xjpcode_ai::JsonObject) -> String {
    serde_json::to_string_pretty(&serde_json::Value::Object(arguments.clone()))
        .unwrap_or_else(|_| "{}".to_string())
}

fn tool_result_text(result: &xjpcode_ai::ToolResultMessage) -> String {
    result
        .content
        .iter()
        .map(|block| match block {
            ToolResultContentBlock::Text(text) => text.text.clone(),
            ToolResultContentBlock::Image(_) => "[image]".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn hydrate(app: &mut App, messages: &[AgentMessage]) {
    app.blocks.clear();
    app.next_block_id = 1;
    app.active_assistant_block_id = None;
    app.active_tool_start = None;

    let mut tool_blocks: HashMap<String, usize> = HashMap::new();
    for message in messages {
        match message {
            AgentMessage::System(_) => {}
            AgentMessage::User(user) => {
                let text = match &user.content {
                    xjpcode_ai::UserContent::Text(text) => text.clone(),
                    xjpcode_ai::UserContent::Blocks(blocks) => blocks
                        .iter()
                        .filter_map(|block| match block {
                            xjpcode_ai::UserContentBlock::Text(text) => Some(text.text.clone()),
                            xjpcode_ai::UserContentBlock::Image(_) => Some("[image]".to_string()),
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                if !text.trim().is_empty() {
                    app.add_user_block(text);
                }
            }
            AgentMessage::Assistant(assistant) => {
                let text = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let reasoning = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::Thinking(thinking)
                            if !thinking.thinking.trim().is_empty() =>
                        {
                            Some(thinking.thinking.clone())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if !text.trim().is_empty() || !reasoning.trim().is_empty() {
                    let block_id = app.add_block(
                        super::block::BlockKind::AssistantAnswer,
                        String::new(),
                        text,
                        true,
                    );
                    if let Some(block) = app.blocks.iter_mut().find(|block| block.id == block_id) {
                        if !reasoning.is_empty() {
                            block.reasoning = Some(reasoning);
                        }
                    }
                }
                for call in assistant.content.iter().filter_map(|block| match block {
                    AssistantContentBlock::ToolCall(call) => Some(call),
                    _ => None,
                }) {
                    let block_id = app.add_tool_call_block(
                        &call.name,
                        &call.id,
                        &pretty_arguments(&call.arguments),
                    );
                    tool_blocks.insert(call.id.clone(), block_id);
                }
            }
            AgentMessage::ToolResult(result) => {
                let text = tool_result_text(result);
                if !app.merge_tool_result(
                    &result.tool_call_id,
                    &result.tool_name,
                    &text,
                    None,
                    result.is_error,
                ) {
                    let block_id =
                        app.add_tool_call_block(&result.tool_name, &result.tool_call_id, "");
                    app.merge_tool_result(
                        &result.tool_call_id,
                        &result.tool_name,
                        &text,
                        None,
                        result.is_error,
                    );
                    tool_blocks.insert(result.tool_call_id.clone(), block_id);
                }
            }
            AgentMessage::Custom(custom) => {
                if custom.role == "compactionSummary" {
                    let tokens = custom
                        .payload
                        .get("tokensBefore")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    let summary = custom
                        .payload
                        .get("summary")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    app.add_system_notice(
                        "上下文已压缩",
                        format!(
                            "之前压缩过一次（压缩前 {} tokens）：{}",
                            tokens,
                            summary.lines().next().unwrap_or("")
                        ),
                    );
                }
            }
        }
    }
}
