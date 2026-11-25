//! `crates/ai/src/api/transform_messages.rs` 的测试。
//!
//! 上游出处：`test/transform-messages-cross-model-thinking.test.ts` 两条全部。

use xjpcode_ai::api::transform_messages::transform_messages;
use xjpcode_ai::providers::model_schema::{ModelCost, ModelInputModality};
use xjpcode_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, Model, StopReason,
    TextContent, ThinkingContent, ThinkingContentType, ToolCall, ToolResultContentBlock,
    ToolResultMessage, ToolResultRole, TranscriptContext, Usage, UsageCost, UserContent, UserMessage,
    UserRole,
};

fn kimi() -> Model {
    Model {
        id: "moonshotai/Kimi-K3".to_string(),
        name: "Kimi K3".to_string(),
        api: "openai-completions".to_string(),
        provider: "sference".to_string(),
        base_url: "https://api.sference.com/v1".to_string(),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        r#type: None,
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 1_048_576,
        max_tokens: 32_768,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

fn assistant(model: &str, content: Vec<AssistantContentBlock>, stop_reason: StopReason) -> Message {
    Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: "openai-completions".to_string(),
        provider: "sference".to_string(),
        model: model.to_string(),
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
        stop_reason,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
        duration_ms: None,
    })
}

fn thinking(text: &str) -> AssistantContentBlock {
    AssistantContentBlock::Thinking(ThinkingContent {
        r#type: ThinkingContentType::Thinking,
        thinking: text.to_string(),
        thinking_signature: Some("reasoning_content".to_string()),
        redacted: None,
    })
}

fn text(text: &str) -> AssistantContentBlock {
    AssistantContentBlock::Text(TextContent::new(text))
}

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp: 0,
    })
}

/// 上游：`does not send another model's readable thinking as text`。
#[test]
fn does_not_send_another_models_readable_thinking_as_text() {
    let messages = vec![
        user("Why does curl work but node fails?"),
        assistant(
            "zai-org/GLM-5.3",
            vec![
                thinking("The user asks about ECONNRESET. Candidates: proxy, IPv6, TLS."),
                text("Let me check the TLS versions."),
                AssistantContentBlock::ToolCall(ToolCall::new(
                    "call_1",
                    "bash",
                    serde_json::json!({"command": "curl -v"})
                        .as_object()
                        .cloned()
                        .expect("object"),
                )),
            ],
            StopReason::ToolUse,
        ),
        Message::ToolResult(ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: "call_1".to_string(),
            tool_name: "bash".to_string(),
            content: vec![ToolResultContentBlock::Text(TextContent::new("TLSv1.2"))],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 0,
            duration_ms: None,
        }),
    ];
    let transformed = transform_messages(&messages, &kimi(), None);
    let assistant_message = transformed
        .iter()
        .find_map(|message| match message {
            Message::Assistant(assistant) => Some(assistant),
            _ => None,
        })
        .expect("assistant message");
    assert_eq!(
        assistant_message.content,
        vec![
            text("Let me check the TLS versions."),
            AssistantContentBlock::ToolCall(ToolCall::new(
                "call_1",
                "bash",
                serde_json::json!({"command": "curl -v"})
                    .as_object()
                    .cloned()
                    .expect("object"),
            )),
        ]
    );
}

/// 上游：`keeps thinking only from the target model when models alternate`。
#[test]
fn keeps_thinking_only_from_the_target_model_when_models_alternate() {
    let messages = vec![
        user("1"),
        assistant(
            "moonshotai/Kimi-K3",
            vec![thinking("kimi 1"), text("a")],
            StopReason::Stop,
        ),
        user("2"),
        assistant(
            "zai-org/GLM-5.3",
            vec![thinking("glm 1"), text("b")],
            StopReason::Stop,
        ),
        user("3"),
        assistant(
            "moonshotai/Kimi-K3",
            vec![thinking("kimi 2"), text("c")],
            StopReason::Stop,
        ),
        user("4"),
        assistant(
            "zai-org/GLM-5.3",
            vec![thinking("glm 2"), text("d")],
            StopReason::Stop,
        ),
        user("hi"),
    ];
    let transformed = transform_messages(&messages, &kimi(), None);
    let contents: Vec<Vec<AssistantContentBlock>> = transformed
        .iter()
        .filter_map(|message| match message {
            Message::Assistant(assistant) => Some(assistant.content.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        contents,
        vec![
            vec![thinking("kimi 1"), text("a")],
            vec![text("b")],
            vec![thinking("kimi 2"), text("c")],
            vec![text("d")],
        ]
    );
}

/// `transcript.ts` 的 `resolve_transcript` 在 `convertMessages` 里也会跑；这里锁住
/// `TranscriptContext` 的形状没漂。
#[test]
fn keeps_the_transcript_shape_used_by_convert_messages() {
    let context = TranscriptContext {
        messages: vec![user("hi")],
    };
    assert_eq!(context.messages.len(), 1);
}
