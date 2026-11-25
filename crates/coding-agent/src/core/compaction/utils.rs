//! 对应上游 `src/core/compaction/utils.ts`：文件操作统计、对话序列化、摘要提示词。

use std::collections::HashSet;

use xjpcode_agent::AgentMessage;
use xjpcode_ai::{content_text, AssistantContentBlock, Message, UserContent};

use crate::core::tools::truncate::slice_head;

/// pi: `FileOperations`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileOperations {
    pub read: HashSet<String>,
    pub written: HashSet<String>,
    pub edited: HashSet<String>,
}

/// pi: `createFileOps`
pub fn create_file_ops() -> FileOperations {
    FileOperations::default()
}

fn add_file_op(tool_name: &str, arguments: Option<&serde_json::Map<String, serde_json::Value>>, ops: &mut FileOperations) {
    let Some(path) = arguments
        .and_then(|arguments| arguments.get("path"))
        .and_then(serde_json::Value::as_str)
    else {
        return;
    };
    match tool_name {
        "read" => {
            ops.read.insert(path.to_string());
        }
        "write" => {
            ops.written.insert(path.to_string());
        }
        "edit" => {
            ops.edited.insert(path.to_string());
        }
        _ => {}
    }
}

/// pi: `extractFileOpsFromMessage`——assistant 的工具调用，或工具结果上记着的嵌套调用。
pub fn extract_file_ops_from_message(message: &AgentMessage, ops: &mut FileOperations) {
    match message {
        AgentMessage::ToolResult(result) => {
            if let Some(nested) = &result.nested_calls {
                for call in &nested.calls {
                    add_file_op(&call.name, call.arguments.as_ref(), ops);
                }
            }
        }
        AgentMessage::Assistant(assistant) => {
            for block in &assistant.content {
                if let AssistantContentBlock::ToolCall(call) = block {
                    add_file_op(&call.name, Some(&call.arguments), ops);
                }
            }
        }
        _ => {}
    }
}

/// pi: `computeFileLists`
pub fn compute_file_lists(ops: &FileOperations) -> (Vec<String>, Vec<String>) {
    let mut modified: Vec<String> = ops.edited.iter().chain(ops.written.iter()).cloned().collect();
    modified.sort();
    let modified_set: HashSet<&String> = modified.iter().collect();
    let mut read_files: Vec<String> = ops
        .read
        .iter()
        .filter(|path| !modified_set.contains(path))
        .cloned()
        .collect();
    read_files.sort();
    (read_files, modified)
}

/// pi: `formatFileOperations`
pub fn format_file_operations(read_files: &[String], modified_files: &[String]) -> String {
    let mut sections = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!(
            "<read-files>\n{}\n</read-files>",
            read_files.join("\n")
        ));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files.join("\n")
        ));
    }
    if sections.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", sections.join("\n\n"))
    }
}

/// 工具结果在摘要里最多留多少个字符（JS 的 `string.length`）。
const TOOL_RESULT_MAX_CHARS: usize = 2000;

fn js_length(text: &str) -> usize {
    text.encode_utf16().count()
}

fn user_content_text(content: &UserContent) -> String {
    match content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => content_text(blocks.as_slice(), ""),
    }
}

fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    let length = js_length(text);
    if length <= max_chars {
        return text.to_string();
    }
    let truncated = length - max_chars;
    format!(
        "{}\n\n[... {truncated} more characters truncated]",
        slice_head(text, max_chars)
    )
}

/// pi: `serializeConversation`——工具结果截短，思考不写进去。
pub fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for message in messages {
        match message {
            Message::User(user) => {
                let content = user_content_text(&user.content);
                if !content.is_empty() {
                    parts.push(format!("[User]: {content}"));
                }
            }
            Message::Assistant(assistant) => {
                let mut tool_calls: Vec<String> = Vec::new();
                for block in &assistant.content {
                    if let AssistantContentBlock::ToolCall(call) = block {
                        let arguments = call
                            .arguments
                            .iter()
                            .map(|(key, value)| {
                                format!("{key}={}", serde_json::to_string(value).unwrap_or_default())
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        tool_calls.push(format!("{}({arguments})", call.name));
                    }
                }
                if assistant
                    .content
                    .iter()
                    .any(|block| matches!(block, AssistantContentBlock::Text(_)))
                {
                    parts.push(format!("[Assistant]: {}", content_text(&assistant.content, "")));
                }
                if !tool_calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", tool_calls.join("; ")));
                }
            }
            Message::ToolResult(result) => {
                let content = content_text(result.content.as_slice(), "");
                if !content.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_for_summary(&content, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
            Message::System(_) => {}
        }
    }
    parts.join("\n\n")
}

/// pi: `SUMMARIZATION_SYSTEM_PROMPT`
pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.\n\nDo NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";
