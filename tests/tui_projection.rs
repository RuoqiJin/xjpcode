//! 全屏界面的投影矩阵：`AgentSessionEvent` → 卡片，接着聊时从消息重画。

use xjpcode::ui::hydrate::hydrate;
use xjpcode::ui::projection::apply;
use xjpcode::ui::{App, BlockKind};
use xjpcode_agent::{AgentMessage, AgentToolResult};
use xjpcode_ai::{
    AssistantContentBlock, StopReason, TextContent, ToolCall, ToolResultContentBlock,
    ToolResultMessage, ToolResultRole, Usage, UserContent, UserMessage, UserRole,
};
use xjpcode_coding_agent::core::agent_session::{AgentSessionEvent, CompactionReason};
use xjpcode_coding_agent::core::compaction::CompactionResult;

fn app() -> App {
    App::new()
}

fn assistant(content: Vec<AssistantContentBlock>, stop_reason: StopReason) -> AgentMessage {
    let mut message =
        xjpcode_agent::empty_assistant_message("openai-completions", "deepseek", "deepseek-flash");
    message.content = content;
    message.stop_reason = stop_reason;
    AgentMessage::Assistant(message)
}

fn text(value: &str) -> AssistantContentBlock {
    AssistantContentBlock::Text(TextContent::new(value))
}

fn tool_result(text: &str) -> AgentToolResult {
    AgentToolResult {
        content: vec![ToolResultContentBlock::Text(TextContent::new(text))],
        details: None,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

fn user_message(text: &str) -> AgentMessage {
    AgentMessage::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp: 0,
    })
}

fn tool_call_message(id: &str, name: &str, path: &str) -> AgentMessage {
    let mut arguments = xjpcode_ai::JsonObject::new();
    arguments.insert("path".to_string(), serde_json::json!(path));
    assistant(
        vec![AssistantContentBlock::ToolCall(ToolCall::new(
            id, name, arguments,
        ))],
        StopReason::ToolUse,
    )
}

fn notice_texts(app: &App) -> Vec<String> {
    app.blocks
        .iter()
        .filter(|block| block.kind == BlockKind::SystemNotice)
        .map(|block| block.content.clone())
        .collect()
}

#[test]
fn text_and_thinking_deltas_share_one_assistant_card() {
    let mut app = app();
    apply(
        &AgentSessionEvent::MessageUpdate {
            message: assistant(Vec::new(), StopReason::Pending),
            assistant_message_event: xjpcode_ai::AssistantMessageEvent::ThinkingDelta {
                content_index: 0,
                delta: "想一下".to_string(),
                partial: xjpcode_agent::empty_assistant_message(
                    "openai-completions",
                    "deepseek",
                    "deepseek-flash",
                ),
            },
        },
        &mut app,
    );
    for delta in ["hel", "lo"] {
        apply(
            &AgentSessionEvent::MessageUpdate {
                message: assistant(vec![text("hello")], StopReason::Pending),
                assistant_message_event: xjpcode_ai::AssistantMessageEvent::TextDelta {
                    content_index: 1,
                    delta: delta.to_string(),
                    partial: xjpcode_agent::empty_assistant_message(
                        "openai-completions",
                        "deepseek",
                        "deepseek-flash",
                    ),
                },
            },
            &mut app,
        );
    }
    assert_eq!(app.blocks.len(), 1);
    assert_eq!(app.blocks[0].content, "hello");
    assert_eq!(app.blocks[0].reasoning.as_deref(), Some("想一下"));

    apply(
        &AgentSessionEvent::MessageEnd {
            message: assistant(vec![text("hello")], StopReason::Stop),
        },
        &mut app,
    );
    assert_eq!(app.blocks.len(), 1);
    assert_eq!(app.blocks[0].content, "hello");
}

#[test]
fn a_provider_without_deltas_still_commits_its_body() {
    let mut app = app();
    apply(
        &AgentSessionEvent::MessageEnd {
            message: assistant(vec![text("only in the final message")], StopReason::Stop),
        },
        &mut app,
    );
    assert_eq!(
        app.blocks
            .iter()
            .map(|block| block.content.as_str())
            .collect::<Vec<_>>(),
        ["only in the final message"]
    );
}

#[test]
fn tool_calls_keep_their_arguments_and_receive_results_by_id() {
    let mut app = app();
    let mut args = xjpcode_ai::JsonObject::new();
    args.insert("path".to_string(), serde_json::json!("src/lib.rs"));
    apply(
        &AgentSessionEvent::ToolExecutionStart {
            tool_call_id: "call-1".to_string(),
            tool_name: "read".to_string(),
            args,
        },
        &mut app,
    );
    apply(
        &AgentSessionEvent::ToolExecutionEnd {
            tool_call_id: "call-1".to_string(),
            tool_name: "read".to_string(),
            result: tool_result("file body"),
            is_error: false,
            duration_ms: Some(3),
        },
        &mut app,
    );

    let calls: Vec<&xjpcode::ui::ContentBlock> = app
        .blocks
        .iter()
        .filter(|block| block.kind == BlockKind::ToolCall)
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].tool_call_id.as_deref(), Some("call-1"));
    assert!(calls[0].content.contains("src/lib.rs"));
    assert!(calls[0].content.contains("file body"));
    assert!(!calls[0].is_error);
}

#[test]
fn errors_retries_and_compaction_are_all_visible() {
    let mut app = app();
    let mut failed = assistant(Vec::new(), StopReason::Error);
    if let AgentMessage::Assistant(assistant) = &mut failed {
        assistant.error_message = Some("401 Unauthorized".to_string());
    }
    apply(&AgentSessionEvent::MessageEnd { message: failed }, &mut app);
    apply(
        &AgentSessionEvent::AutoRetryStart {
            attempt: 1,
            max_attempts: 3,
            delay_ms: 2000,
            error_message: "overloaded".to_string(),
        },
        &mut app,
    );
    apply(
        &AgentSessionEvent::AutoRetryEnd {
            success: false,
            attempt: 3,
            final_error: Some("still down".to_string()),
        },
        &mut app,
    );
    apply(
        &AgentSessionEvent::CompactionStart {
            reason: CompactionReason::Threshold,
        },
        &mut app,
    );
    apply(
        &AgentSessionEvent::CompactionEnd {
            reason: CompactionReason::Threshold,
            result: Some(CompactionResult {
                summary: "summary".to_string(),
                first_kept_entry_id: "e1".to_string(),
                tokens_before: 3219,
                estimated_tokens_after: Some(2304),
                usage: None,
                details: None,
            }),
            aborted: false,
            will_retry: false,
            error_message: None,
        },
        &mut app,
    );

    let notices = notice_texts(&app);
    assert!(notices.iter().any(|text| text.contains("401 Unauthorized")));
    assert!(notices.iter().any(|text| text.contains("still down")));
    assert!(notices.iter().any(|text| text.contains("第 1/3 次重试")));
    assert!(notices
        .iter()
        .any(|text| text.contains("3219") && text.contains("2304")));
}

#[test]
fn aborted_rounds_keep_partial_text_and_say_cancelled() {
    let mut app = app();
    apply(
        &AgentSessionEvent::MessageUpdate {
            message: assistant(vec![text("part")], StopReason::Pending),
            assistant_message_event: xjpcode_ai::AssistantMessageEvent::TextDelta {
                content_index: 0,
                delta: "part".to_string(),
                partial: xjpcode_agent::empty_assistant_message(
                    "openai-completions",
                    "deepseek",
                    "deepseek-flash",
                ),
            },
        },
        &mut app,
    );
    apply(
        &AgentSessionEvent::MessageEnd {
            message: assistant(vec![text("part")], StopReason::Aborted),
        },
        &mut app,
    );
    let assistant_texts: Vec<&str> = app
        .blocks
        .iter()
        .filter(|block| block.kind == BlockKind::AssistantAnswer)
        .map(|block| block.content.as_str())
        .collect();
    assert_eq!(assistant_texts, ["part"]);
    assert!(notice_texts(&app)
        .iter()
        .any(|text| text.contains("已取消")));
}

#[test]
fn queue_updates_show_pending_steering_and_follow_ups() {
    let mut app = app();
    apply(
        &AgentSessionEvent::QueueUpdate {
            steering: vec!["do this next".to_string()],
            follow_up: vec!["then that".to_string()],
        },
        &mut app,
    );
    assert_eq!(app.pending_steering, ["do this next"]);
    assert_eq!(app.pending_follow_up, ["then that"]);
    apply(
        &AgentSessionEvent::QueueUpdate {
            steering: Vec::new(),
            follow_up: Vec::new(),
        },
        &mut app,
    );
    assert!(app.pending_steering.is_empty());
    assert!(app.pending_follow_up.is_empty());
}

#[test]
fn hydration_rebuilds_users_assistants_tools_and_compaction_summaries() {
    let mut app = app();
    let mut messages = vec![
        user_message("first"),
        tool_call_message("call-1", "read", "README.md"),
        AgentMessage::ToolResult(ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: "call-1".to_string(),
            tool_name: "read".to_string(),
            content: vec![ToolResultContentBlock::Text(TextContent::new("read body"))],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 0,
            duration_ms: None,
        }),
        assistant(vec![text("answer")], StopReason::Stop),
    ];
    messages.insert(
        0,
        xjpcode_coding_agent::core::messages::create_compaction_summary_message(
            "old summary\nmore",
            1234,
            0,
        ),
    );
    hydrate(&mut app, &messages);

    assert!(matches!(app.blocks[0].kind, BlockKind::SystemNotice));
    assert!(app.blocks[0].content.contains("1234"));
    assert!(matches!(app.blocks[1].kind, BlockKind::UserPrompt));
    assert_eq!(app.blocks[1].content, "first");
    let tool = app
        .blocks
        .iter()
        .find(|block| block.kind == BlockKind::ToolCall)
        .expect("tool block");
    assert!(tool.content.contains("README.md"));
    assert!(tool.content.contains("read body"));
    assert!(app
        .blocks
        .iter()
        .any(|block| block.kind == BlockKind::AssistantAnswer && block.content == "answer"));
}

#[test]
fn hydration_tolerates_an_assistant_message_without_a_text_block() {
    let mut app = app();
    hydrate(&mut app, &[tool_call_message("call-1", "bash", "date")]);
    assert_eq!(app.blocks.len(), 1);
    assert_eq!(app.blocks[0].kind, BlockKind::ToolCall);
    assert!(app.blocks[0].content.contains("date"));
}

#[test]
fn turn_usage_reaches_the_spinner_line() {
    let mut app = app();
    let mut message = assistant(vec![text("hello")], StopReason::Stop);
    if let AgentMessage::Assistant(assistant) = &mut message {
        assistant.usage = Usage {
            input: 12,
            output: 3,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: 15,
            cost: Default::default(),
        };
    }
    apply(
        &AgentSessionEvent::TurnEnd {
            message,
            tool_results: Vec::new(),
        },
        &mut app,
    );
    assert!(app.spinner.format_tokens().contains("↑12 ↓3"));
}
