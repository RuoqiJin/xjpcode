//! 对应上游 `packages/ai/src/api/openai-completions.ts` 的 `normalizeToolCallId`（1193–1219 行）：
//! 别家（Responses 一族）留下的 `call_id|item_id`，发给 DeepSeek 之前要收成 Chat Completions 认的 id。
//!
//! 这条路径原先没搬（里程碑六记的「kit-session 里请求还差一处」就是它）；现在有测试守着。

use serde_json::Value;
use xjpcode_ai::api::openai_completions::{convert_messages, get_compat};
use xjpcode_ai::providers::deepseek;
use xjpcode_ai::providers::model_schema::{ModelCost, ModelInputModality};
use xjpcode_ai::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, Model, StopReason, TextContent,
    ToolCall, ToolResultContentBlock, ToolResultMessage, ToolResultRole, TranscriptContext, Usage,
    UsageCost, UserContent, UserMessage, UserRole,
};

fn model(provider: &str, api: &str, id: &str) -> Model {
    Model {
        id: id.to_string(),
        name: id.to_string(),
        api: api.to_string(),
        provider: provider.to_string(),
        base_url: "https://example.test".to_string(),
        input: vec![ModelInputModality::Text],
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        r#type: None,
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 100_000,
        max_tokens: 8_000,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

fn assistant(provider: &str, api: &str, model_id: &str, tool_call_id: &str) -> Message {
    Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
            tool_call_id,
            "bash",
            serde_json::json!({"command": "echo hi"}).as_object().unwrap().clone(),
        ))],
        api: api.to_string(),
        provider: provider.to_string(),
        model: model_id.to_string(),
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
        stop_reason: StopReason::ToolUse,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 1,
        duration_ms: None,
    })
}

fn context(tool_call_id: &str) -> TranscriptContext {
    TranscriptContext {
        messages: vec![
            Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("run it".to_string()),
                timestamp: 0,
            }),
            assistant("openai-codex", "openai-codex-responses", "gpt-6.2-astra", tool_call_id),
            Message::ToolResult(ToolResultMessage {
                role: ToolResultRole::ToolResult,
                tool_call_id: tool_call_id.to_string(),
                tool_name: "bash".to_string(),
                content: vec![ToolResultContentBlock::Text(TextContent::new("hi"))],
                details: None,
                usage: None,
                nested_calls: None,
                is_error: false,
                timestamp: 2,
                duration_ms: None,
            }),
        ],
    }
}

fn ids(input: &[Value]) -> (String, String) {
    let mut call = String::new();
    let mut result = String::new();
    for item in input {
        if let Some(tool_calls) = item.get("tool_calls").and_then(Value::as_array) {
            call = tool_calls[0]
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
        }
        if item.get("role").and_then(Value::as_str) == Some("tool") {
            result = item
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
        }
    }
    (call, result)
}

#[test]
fn short_call_and_item_ids_are_joined_and_kept_unique() {
    let deepseek = deepseek::model("deepseek-flash").unwrap();
    let input = convert_messages(&deepseek, &context("call_a|fc_b"), &get_compat(&deepseek));
    let (call, result) = ids(&input);
    assert_eq!(call, "call_a_fc_b");
    assert_eq!(result, call);
}

#[test]
fn long_responses_ids_are_shortened_to_forty_chars_with_the_same_hash() {
    let raw = "call_4VnzVawQXPB9MgYib7CiQFEY|I9b95oN1wD/cHXKTw3PpRkL6KkCtzTJhUxMouMWYwHeTo2j3htzfSk7YPx2vifiIM4g3A8XXyOj8q4Bt6SLUG7gqY1E3ELkrkVQNHglRfUmWj84lqxJY+Puieb3VKyX0FB+83TUzn91cDMF/4gzt990IzqVrc+nIb9RRscRD070Du16q1glydVjWR0SBJsE6TbY/esOjFpqplogQqrajm1eI++f3eLi73R6q7hVusY0QbeFySVxABCjhN0lXB04caBe1rzHjYzul6MAXj7uq+0r17VLq+yrtyYhN12wkmFqHeqTyEei6EFPbMy24Nc+IbJlkP0OCg02W+gOnyBFcbi2ctvJFSOhSjt1CqBdqCnnhwUqXjbWiT0wh3DmLScRgTHmGkaI+oAcQQjfic65nxj+TnEkReA==";
    let deepseek = deepseek::model("deepseek-flash").unwrap();
    let input = convert_messages(&deepseek, &context(raw), &get_compat(&deepseek));
    let (call, result) = ids(&input);
    assert_eq!(result, call);
    assert!(call.chars().count() <= 40);
    let hash: String = xjpcode_ai::utils::hash::short_hash(raw).chars().take(8).collect();
    assert!(call.ends_with(&format!("_{hash}")));
    assert!(call.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'));
    assert!(call.starts_with("call_4VnzVawQXPB9MgYib7CiQFEY_"));
}

#[test]
fn ids_without_a_pipe_stay_as_they_are_for_deepseek() {
    let long = "x".repeat(60);
    let deepseek = deepseek::model("deepseek-flash").unwrap();
    let input = convert_messages(&deepseek, &context(&long), &get_compat(&deepseek));
    let (call, result) = ids(&input);
    assert_eq!(call, long);
    assert_eq!(result, long);
}

#[test]
fn openai_truncates_long_plain_ids_to_forty_chars() {
    let openai = model("openai", "openai-completions", "gpt-4o-mini");
    let long = "y".repeat(60);
    let input = convert_messages(&openai, &context(&long), &get_compat(&openai));
    let (call, _) = ids(&input);
    assert_eq!(call, "y".repeat(40));
}
