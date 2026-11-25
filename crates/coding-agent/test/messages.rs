//! `core/messages.rs` 的测试：压缩摘要要能变成模型看得懂的 user 消息。
//!
//! 上游没有直接测 `convertToLlm` 的文件；这些用例是对着 `src/core/messages.ts` 的分支
//! （`custom`/`branchSummary`/`compactionSummary`/`bashExecution`）写的。

use serde_json::json;
use xjpcode_agent::{empty_assistant_message, AgentMessage};
use xjpcode_ai::{
    content_text, AssistantContentBlock, Message, StopReason, SystemContent, SystemMessage,
    SystemRole, TextContent, ToolResultContentBlock, ToolResultMessage, ToolResultRole, UserContent,
    UserMessage, UserRole,
};
use xjpcode_coding_agent::core::messages::{
    convert_to_llm, create_compaction_summary_message, create_custom_message,
    COMPACTION_SUMMARY_PREFIX, COMPACTION_SUMMARY_SUFFIX,
};

fn user_text(content: &UserContent) -> String {
    match content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => content_text(blocks.as_slice(), ""),
    }
}

#[test]
fn turns_a_compaction_summary_into_a_user_message() {
    let messages = vec![create_compaction_summary_message("the summary", 123, 1_700_000_000_000)];
    let converted = convert_to_llm(&messages);
    assert_eq!(converted.len(), 1);
    let Message::User(user) = &converted[0] else {
        panic!("expected a user message");
    };
    assert_eq!(
        user_text(&user.content),
        format!("{COMPACTION_SUMMARY_PREFIX}the summary{COMPACTION_SUMMARY_SUFFIX}")
    );
    assert_eq!(user.timestamp, 1_700_000_000_000);
}

#[test]
fn turns_a_custom_message_into_a_user_message() {
    let messages = vec![create_custom_message(
        "note",
        json!("remember this"),
        true,
        None,
        7,
    )];
    let converted = convert_to_llm(&messages);
    let Message::User(user) = &converted[0] else {
        panic!("expected a user message");
    };
    assert_eq!(user_text(&user.content), "remember this");
}

#[test]
fn keeps_the_four_llm_message_types_as_they_are() {
    let system = AgentMessage::System(SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text("prompt".to_string()),
        sections: None,
        tools_added: None,
        tools_removed: None,
        timestamp: 1,
    });
    let user = AgentMessage::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text("hello".to_string()),
        timestamp: 2,
    });
    let mut assistant = empty_assistant_message("openai-completions", "deepseek", "deepseek-flash");
    assistant.content = vec![AssistantContentBlock::Text(TextContent::new("hi"))];
    assistant.stop_reason = StopReason::Stop;
    let assistant = AgentMessage::Assistant(assistant);
    let tool_result = AgentMessage::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "call".to_string(),
        tool_name: "read".to_string(),
        content: vec![ToolResultContentBlock::Text(TextContent::new("file"))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 3,
        duration_ms: None,
    });
    let converted = convert_to_llm(&[system, user, assistant, tool_result]);
    assert_eq!(converted.len(), 4);
    assert!(matches!(converted[0], Message::System(_)));
    assert!(matches!(converted[1], Message::User(_)));
    assert!(matches!(converted[2], Message::Assistant(_)));
    assert!(matches!(converted[3], Message::ToolResult(_)));
}

#[test]
fn turns_a_bash_execution_into_a_user_message_and_skips_excluded_ones() {
    let payload = |command: &str, output: &str, exclude: bool| {
        let mut payload = serde_json::Map::new();
        payload.insert("role".to_string(), json!("bashExecution"));
        payload.insert("command".to_string(), json!(command));
        payload.insert("output".to_string(), json!(output));
        payload.insert("exitCode".to_string(), json!(0));
        payload.insert("cancelled".to_string(), json!(false));
        payload.insert("truncated".to_string(), json!(false));
        if exclude {
            payload.insert("excludeFromContext".to_string(), json!(true));
        }
        payload.insert("timestamp".to_string(), json!(5));
        AgentMessage::Custom(xjpcode_agent::CustomAgentMessage {
            role: "bashExecution".to_string(),
            payload,
        })
    };
    let converted = convert_to_llm(&[
        payload("ls", "a b", false),
        payload("pwd", "/work", true),
    ]);
    assert_eq!(converted.len(), 1);
    let Message::User(user) = &converted[0] else {
        panic!("expected a user message");
    };
    let text = user_text(&user.content);
    assert!(text.contains("Ran `ls`"));
    assert!(text.contains("a b"));
    assert!(!text.contains("pwd"));
}
