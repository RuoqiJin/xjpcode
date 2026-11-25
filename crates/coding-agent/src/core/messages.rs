//! 对应上游 `src/core/messages.ts` 中本里程碑用到的部分：三种自定义消息的构造与文本前缀，
//! 以及 `convertToLlm`（压缩摘要要能变成模型看得懂的 user 消息）。
//!
//! 上游靠 TypeScript 的声明合并扩展 `AgentMessage`；Rust 侧是 `AgentMessage::Custom`
//! （见 `docs/port/agent-contract.md`）。

use serde_json::Value;
use xjpcode_agent::{AgentMessage, CustomAgentMessage};
use xjpcode_ai::{
    JsonObject, Message, TextContent, UserContent, UserContentBlock, UserMessage, UserRole,
};

pub const COMPACTION_SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
pub const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";
pub const BRANCH_SUMMARY_PREFIX: &str =
    "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";
pub const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";

fn custom_message(payload: JsonObject) -> AgentMessage {
    let role = payload
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("custom")
        .to_string();
    AgentMessage::Custom(CustomAgentMessage { role, payload })
}

/// pi: `createCustomMessage`
pub fn create_custom_message(
    custom_type: &str,
    content: Value,
    display: bool,
    details: Option<Value>,
    timestamp: u64,
) -> AgentMessage {
    let mut payload = JsonObject::new();
    payload.insert("role".to_string(), Value::String("custom".to_string()));
    payload.insert(
        "customType".to_string(),
        Value::String(custom_type.to_string()),
    );
    payload.insert("content".to_string(), content);
    payload.insert("display".to_string(), Value::Bool(display));
    if let Some(details) = details {
        payload.insert("details".to_string(), details);
    }
    payload.insert("timestamp".to_string(), Value::from(timestamp));
    custom_message(payload)
}

/// pi: `createBranchSummaryMessage`
pub fn create_branch_summary_message(summary: &str, from_id: &str, timestamp: u64) -> AgentMessage {
    let mut payload = JsonObject::new();
    payload.insert(
        "role".to_string(),
        Value::String("branchSummary".to_string()),
    );
    payload.insert("summary".to_string(), Value::String(summary.to_string()));
    payload.insert("fromId".to_string(), Value::String(from_id.to_string()));
    payload.insert("timestamp".to_string(), Value::from(timestamp));
    custom_message(payload)
}

/// pi: `createCompactionSummaryMessage`
pub fn create_compaction_summary_message(
    summary: &str,
    tokens_before: u64,
    timestamp: u64,
) -> AgentMessage {
    let mut payload = JsonObject::new();
    payload.insert(
        "role".to_string(),
        Value::String("compactionSummary".to_string()),
    );
    payload.insert("summary".to_string(), Value::String(summary.to_string()));
    payload.insert("tokensBefore".to_string(), Value::from(tokens_before));
    payload.insert("timestamp".to_string(), Value::from(timestamp));
    custom_message(payload)
}

/// pi: `bashExecutionToText`
pub fn bash_execution_to_text(command: &str, output: &str) -> String {
    if output.is_empty() {
        format!("Ran `{command}`\n(no output)")
    } else {
        format!("Ran `{command}`\n```\n{output}\n```")
    }
}

fn payload_string(payload: &JsonObject, key: &str) -> Option<String> {
    payload.get(key).and_then(Value::as_str).map(ToString::to_string)
}

fn text_blocks(text: String) -> UserContent {
    UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(text))])
}

/// pi: `convertToLlm`——自定义消息变 user 消息；不参与上下文的自定义消息丢掉。
pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<Message> {
    let mut converted = Vec::with_capacity(messages.len());
    for message in messages {
        match message {
            AgentMessage::System(message) => converted.push(Message::System(message.clone())),
            AgentMessage::User(message) => converted.push(Message::User(message.clone())),
            AgentMessage::Assistant(message) => converted.push(Message::Assistant(message.clone())),
            AgentMessage::ToolResult(message) => {
                converted.push(Message::ToolResult(message.clone()))
            }
            AgentMessage::Custom(custom) => {
                let timestamp = custom
                    .payload
                    .get("timestamp")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let content = match custom.role.as_str() {
                    "bashExecution" => {
                        if custom
                            .payload
                            .get("excludeFromContext")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                        {
                            continue;
                        }
                        let command = payload_string(&custom.payload, "command").unwrap_or_default();
                        let output = payload_string(&custom.payload, "output").unwrap_or_default();
                        let mut text = bash_execution_to_text(&command, &output);
                        if custom
                            .payload
                            .get("cancelled")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                        {
                            text.push_str("\n\n(command cancelled)");
                        } else if let Some(exit_code) = custom.payload.get("exitCode").and_then(Value::as_i64)
                        {
                            if exit_code != 0 {
                                text.push_str(&format!("\n\nCommand exited with code {exit_code}"));
                            }
                        }
                        if custom
                            .payload
                            .get("truncated")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                        {
                            if let Some(path) = payload_string(&custom.payload, "fullOutputPath") {
                                text.push_str(&format!("\n\n[Output truncated. Full output: {path}]"));
                            }
                        }
                        text_blocks(text)
                    }
                    "custom" => match custom.payload.get("content") {
                        Some(Value::String(text)) => text_blocks(text.clone()),
                        Some(Value::Array(blocks)) => UserContent::Blocks(
                            blocks
                                .iter()
                                .filter_map(|block| serde_json::from_value(block.clone()).ok())
                                .collect(),
                        ),
                        _ => UserContent::Blocks(Vec::new()),
                    },
                    "branchSummary" => {
                        let summary = payload_string(&custom.payload, "summary").unwrap_or_default();
                        text_blocks(format!(
                            "{BRANCH_SUMMARY_PREFIX}{summary}{BRANCH_SUMMARY_SUFFIX}"
                        ))
                    }
                    "compactionSummary" => {
                        let summary = payload_string(&custom.payload, "summary").unwrap_or_default();
                        text_blocks(format!(
                            "{COMPACTION_SUMMARY_PREFIX}{summary}{COMPACTION_SUMMARY_SUFFIX}"
                        ))
                    }
                    _ => continue,
                };
                converted.push(Message::User(UserMessage {
                    role: UserRole::User,
                    content,
                    timestamp,
                }));
            }
        }
    }
    converted
}
