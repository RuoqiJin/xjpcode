//! `crates/ai/src/utils/estimate.rs` 的测试。
//!
//! 上游出处：`test/context-estimate.test.ts` 三条全部。

use xjpcode_ai::api::simple_options::build_base_options;
use xjpcode_ai::providers::model_schema::ModelCost;
use xjpcode_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, Model, StopReason,
    TextContent, TranscriptContext, Usage, UsageCost, UserContent, UserMessage, UserRole,
};
use xjpcode_ai::types::Context;
use xjpcode_ai::utils::estimate::estimate_context_tokens;
use xjpcode_ai::utils::transcript::normalize_context;

fn create_usage(total_tokens: u64) -> Usage {
    Usage {
        input: total_tokens,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens,
        cost: UsageCost::default(),
    }
}

fn create_assistant(timestamp: u64, total_tokens: u64) -> Message {
    Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(TextContent::new("kept"))],
        api: "openai-responses".to_string(),
        provider: "openai".to_string(),
        model: "test-model".to_string(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: create_usage(total_tokens),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp,
        duration_ms: None,
    })
}

fn user(content: &str, timestamp: u64) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(content.to_string()),
        timestamp,
    })
}

fn model() -> Model {
    Model {
        id: "test-model".to_string(),
        name: "Test Model".to_string(),
        api: "openai-responses".to_string(),
        provider: "openai".to_string(),
        base_url: "https://api.openai.com/v1".to_string(),
        input: vec![xjpcode_ai::providers::model_schema::ModelInputModality::Text],
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        r#type: None,
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 10_000,
        max_tokens: 8_000,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

/// 上游：`reserves 3.5 characters per token for new text when limiting output`。
#[test]
fn reserves_3_5_characters_per_token_for_new_text() {
    let context = TranscriptContext {
        messages: vec![create_assistant(100, 2_000), user(&"x".repeat(3_500), 200)],
    };
    let estimate = estimate_context_tokens(&context);
    assert_eq!(estimate.tokens, 3_000);
    assert_eq!(estimate.usage_tokens, 2_000);
    assert_eq!(estimate.trailing_tokens, 1_000);
    assert_eq!(estimate.last_usage_index, Some(0));
    assert_eq!(
        build_base_options(&model(), &context, None, None).max_tokens,
        Some(2_904)
    );
}

/// 上游：`ignores stale assistant usage after a newer message is inserted before it`。
#[test]
fn ignores_stale_assistant_usage_after_a_newer_message_is_inserted_before_it() {
    // 上游这一条带 `systemPrompt: "system"`，开头的系统消息也要算进去。
    let context = normalize_context(Context {
        system_prompt: Some("system".to_string()),
        messages: vec![
            user("summary", 200),
            create_assistant(100, 9_500),
            user(&"x".repeat(4_000), 300),
        ],
        tools: None,
    });
    let estimate = estimate_context_tokens(&context);
    assert_eq!(estimate.tokens, 1_149);
    assert_eq!(estimate.usage_tokens, 0);
    assert_eq!(estimate.trailing_tokens, 1_149);
    assert_eq!(estimate.last_usage_index, None);
    assert_eq!(
        build_base_options(&model(), &context, None, None).max_tokens,
        Some(4_755)
    );
}

/// 上游：`uses assistant usage again after a response to the inserted context`。
#[test]
fn uses_assistant_usage_again_after_a_response_to_the_inserted_context() {
    let context = TranscriptContext {
        messages: vec![
            user("summary", 200),
            create_assistant(100, 9_500),
            user("new prompt", 300),
            create_assistant(400, 2_000),
            user("tail", 500),
        ],
    };
    let estimate = estimate_context_tokens(&context);
    assert_eq!(estimate.tokens, 2_002);
    assert_eq!(estimate.usage_tokens, 2_000);
    assert_eq!(estimate.trailing_tokens, 2);
    assert_eq!(estimate.last_usage_index, Some(3));
}
