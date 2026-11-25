//! `crates/ai/src/api/simple_options.rs` 的测试。
//!
//! 上游出处：`test/clamp-max-tokens.test.ts` 全部 7 条。

use xjpcode_ai::api::simple_options::clamp_max_tokens_to_context;
use xjpcode_ai::providers::model_schema::ModelCost;
use xjpcode_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantRole, Message, Model, StopReason,
    TextContent, TranscriptContext, Usage, UsageCost, UserMessage, UserRole,
};

const WINDOW: u64 = 1_048_576;

fn model_with(max_tokens: u64, context_window: u64) -> Model {
    Model {
        id: "gpt-4o-mini".to_string(),
        name: "GPT-4o mini".to_string(),
        api: "openai-completions".to_string(),
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
        context_window,
        max_tokens,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

/// 最后一条 assistant 的用量是估算的锚点，所以估出来的输入正好是 `input_tokens`。
fn context_with_input(input_tokens: u64) -> TranscriptContext {
    let assistant = AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::Text(TextContent::new("ok"))],
        api: "openai-completions".to_string(),
        provider: "openai".to_string(),
        model: "gpt-4o-mini".to_string(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            input: input_tokens,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: input_tokens,
            cost: UsageCost::default(),
        },
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 2,
        duration_ms: None,
    };
    TranscriptContext {
        messages: vec![
            Message::User(UserMessage {
                role: UserRole::User,
                content: xjpcode_ai::UserContent::Text("hi".to_string()),
                timestamp: 1,
            }),
            Message::Assistant(assistant),
        ],
    }
}

/// 上游：`leaves 15% of the input unused so an OpenRouter overcount still fits (#10497)`。
#[test]
fn leaves_fifteen_percent_of_the_input_unused() {
    let max_tokens = clamp_max_tokens_to_context(
        &model_with(943_718, WINDOW),
        &context_with_input(220_815),
        943_718,
    );
    assert_eq!(max_tokens, WINDOW - 220_815 - 33_123);
    assert!(229_747 + max_tokens <= WINDOW);
}

/// 上游：`leaves at least 4096 tokens unused for small inputs`。
#[test]
fn leaves_at_least_4096_tokens_unused_for_small_inputs() {
    assert_eq!(
        clamp_max_tokens_to_context(&model_with(WINDOW, WINDOW), &context_with_input(10_000), WINDOW),
        WINDOW - 10_000 - 4_096
    );
}

/// 上游：`keeps the model output limit when it already fits`。
#[test]
fn keeps_the_model_output_limit_when_it_already_fits() {
    assert_eq!(
        clamp_max_tokens_to_context(&model_with(64_000, WINDOW), &context_with_input(100_000), 64_000),
        64_000
    );
}

/// 上游：`sends up to 32768 tokens when the proportional margin leaves no room`。
#[test]
fn sends_up_to_32768_tokens_when_the_proportional_margin_leaves_no_room() {
    assert_eq!(
        clamp_max_tokens_to_context(
            &model_with(943_718, WINDOW),
            &context_with_input(912_000),
            943_718
        ),
        32_768
    );
}

/// 上游：`uses the real room minus 4096 when less than 32768 tokens fit`。
#[test]
fn uses_the_real_room_minus_4096_when_less_than_32768_tokens_fit() {
    assert_eq!(
        clamp_max_tokens_to_context(
            &model_with(943_718, WINDOW),
            &context_with_input(1_030_000),
            943_718
        ),
        WINDOW - 1_030_000 - 4_096
    );
}

/// 上游：`keeps only the fixed 4096 margin for small windows`。
#[test]
fn keeps_only_the_fixed_4096_margin_for_small_windows() {
    assert_eq!(
        clamp_max_tokens_to_context(&model_with(32_768, 32_768), &context_with_input(20_000), 32_768),
        32_768 - 20_000 - 4_096
    );
}

/// 上游：`sends 1 token when the input already exceeds the window`。
#[test]
fn sends_one_token_when_the_input_already_exceeds_the_window() {
    assert_eq!(
        clamp_max_tokens_to_context(
            &model_with(943_718, WINDOW),
            &context_with_input(1_050_000),
            943_718
        ),
        1
    );
}
