//! 移植自上游 `packages/ai/test/overflow.test.ts`。

use xjpcode_ai::utils::overflow::{is_context_overflow, is_recoverable_length};
use xjpcode_ai::{
    AssistantMessage, AssistantRole, AssistantContentBlock, ProviderId, StopReason, Usage,
    UsageCost,
};

fn message(error_message: &str, provider: &str) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::<AssistantContentBlock>::new(),
        api: "openai-completions".to_string(),
        provider: ProviderId::from(provider),
        model: "qwen3.5:35b".to_string(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            cost: UsageCost::default(),
            ..Usage::default()
        },
        stop_reason: StopReason::Error,
        deferred: None,
        error_message: Some(error_message.to_string()),
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
        duration_ms: None,
    }
}

#[test]
fn detects_provider_independent_request_size_overflow() {
    for text in [
        "github-copilot API error (413): {\"message\":\"failed to parse request\",\"code\":\"\"}",
        "custom-provider API error (413): Payload Too Large",
        "413 {\"error\":{\"message\":\"failed to parse request\",\"code\":\"\"}}",
        "413",
    ] {
        assert!(is_context_overflow(&message(text, "ollama"), Some(1_000_000)), "{text}");
        assert!(is_context_overflow(&message(text, "ollama"), None), "{text}");
    }
}

#[test]
fn does_not_mistake_unrelated_errors_for_request_size_overflow() {
    for text in [
        "github-copilot API error (400): {\"message\":\"failed to parse request\",\"code\":\"\"}",
        "custom-provider API error (429): Too many requests",
        "github-copilot API error (400): {\"message\":\"failed to parse request: invalid value 413\"}",
        "custom-provider API error (400): upstream mentioned OpenAI API error (413): invalid input",
        "custom-provider API error (4130): invalid input",
        "4130 failed to parse request",
    ] {
        assert!(!is_context_overflow(&message(text, "ollama"), None), "{text}");
    }
}

#[test]
fn limits_http_status_matching_to_failed_responses() {
    let base = message(
        "OpenAI API error (413): {\"message\":\"failed to parse request\"}",
        "ollama",
    );
    let mut aborted = base.clone();
    aborted.stop_reason = StopReason::Aborted;
    assert!(!is_context_overflow(&aborted, None));
    let mut stopped = base;
    stopped.stop_reason = StopReason::Stop;
    assert!(!is_context_overflow(&stopped, None));
}

#[test]
fn detects_explicit_ollama_prompt_too_long_errors() {
    assert!(is_context_overflow(
        &message(
            "400 `prompt too long; exceeded max context length by 100918 tokens`",
            "ollama"
        ),
        Some(32768)
    ));
}

#[test]
fn detects_zai_prompt_too_long_errors() {
    assert!(is_context_overflow(
        &message("400 {\"code\":\"1261\",\"message\":\"Prompt too long\"}", "zai"),
        Some(1_048_576)
    ));
    assert!(is_context_overflow(
        &message(
            "400 {\"code\":\"1261\",\"message\":\"Prompt exceeds max length\"}",
            "zai"
        ),
        Some(1_048_576)
    ));
}

#[test]
fn detects_together_ai_context_length_errors() {
    assert!(is_context_overflow(
        &message(
            "400 The input (516368 tokens) is longer than the model's context length (262144 tokens).",
            "together"
        ),
        Some(262_144)
    ));
}

#[test]
fn detects_litellm_wrapped_openai_maximum_context_length_errors() {
    assert!(is_context_overflow(
        &message(
            "Error: 503 litellm.ServiceUnavailableError: litellm.MidStreamFallbackError: litellm.APIConnectionError: APIConnectionError: OpenAIException - Requested token count exceeds the model's maximum context length of 131072 tokens.",
            "litellm"
        ),
        Some(131_072)
    ));
    assert!(is_context_overflow(
        &message(
            "400: {\"message\":\"litellm.ContextWindowExceededError: litellm.BadRequestError: litellm._pre_call_checks: Context Window exceeded for given call.\"}",
            "litellm"
        ),
        Some(262_144)
    ));
}

#[test]
fn detects_openai_compatible_parenthesized_maximum_context_length_errors() {
    assert!(is_context_overflow(
        &message(
            "Error: 400 Input length (265330) exceeds model's maximum context length (262144).",
            "openai"
        ),
        Some(262_144)
    ));
}

#[test]
fn detects_openrouter_poolside_maximum_allowed_input_length_errors() {
    assert!(is_context_overflow(
        &message(
            "Provider returned error: Input length 131393 exceeds the maximum allowed input length of 131040 tokens.",
            "openrouter"
        ),
        Some(131_072)
    ));
}

#[test]
fn detects_ds4_configured_context_size_errors() {
    assert!(is_context_overflow(
        &message(
            "400 Prompt has 256468 tokens, but the configured context size is 256000 tokens",
            "ds4"
        ),
        Some(256_000)
    ));
    assert!(is_context_overflow(
        &message(
            "Prompt has 5,958,968 tokens, but the configured context size is 256,000 tokens",
            "ds4"
        ),
        Some(256_000)
    ));
}

#[test]
fn does_not_treat_generic_non_overflow_errors_as_overflow() {
    assert!(!is_context_overflow(
        &message("500 `model runner crashed unexpectedly`", "ollama"),
        Some(32768)
    ));
}

#[test]
fn only_treats_bodyless_400_errors_as_overflow_for_cerebras() {
    let text = "400 status code (no body)";
    assert!(is_context_overflow(&message(text, "cerebras"), Some(131_072)));
    assert!(!is_context_overflow(&message(text, "opencode-go"), Some(1_000_000)));
}

#[test]
fn does_not_treat_bedrock_throttling_too_many_tokens_as_overflow() {
    assert!(!is_context_overflow(
        &message(
            "Throttling error: Too many tokens, please wait before trying again.",
            "bedrock"
        ),
        Some(200_000)
    ));
}

#[test]
fn does_not_treat_bedrock_service_unavailable_as_overflow() {
    assert!(!is_context_overflow(
        &message(
            "Service unavailable: The service is temporarily unavailable.",
            "bedrock"
        ),
        Some(200_000)
    ));
}

#[test]
fn does_not_treat_generic_rate_limit_errors_as_overflow() {
    assert!(!is_context_overflow(
        &message(
            "Rate limit exceeded, please retry after 30 seconds.",
            "openai"
        ),
        Some(200_000)
    ));
    assert!(!is_context_overflow(
        &message("Too many requests. Please slow down.", "openai"),
        Some(200_000)
    ));
}

fn length_stop(input: u64, cache_read: u64, output: u64, cache_write: u64) -> AssistantMessage {
    let mut message = message("", "test-provider");
    message.stop_reason = StopReason::Length;
    message.error_message = None;
    message.usage = Usage {
        input,
        output,
        cache_read,
        cache_write,
        total_tokens: input + cache_read + cache_write + output,
        cost: UsageCost::default(),
        ..Usage::default()
    };
    message
}

#[test]
fn detects_xiaomi_style_overflow() {
    assert!(is_context_overflow(
        &length_stop(58, 1_048_512, 0, 0),
        Some(1_048_576)
    ));
}

#[test]
fn treats_a_length_stop_below_the_desired_output_limit_as_recoverable() {
    assert!(is_recoverable_length(&length_stop(3, 253_584, 16, 25_554), 128_000));
}

#[test]
fn does_not_recover_a_length_stop_that_reached_the_desired_output_limit() {
    assert!(!is_recoverable_length(&length_stop(4_062, 0, 1_024, 0), 1_024));
}

#[test]
fn treats_zero_output_length_stops_as_recoverable() {
    assert!(is_recoverable_length(&length_stop(100, 0, 0, 0), 128_000));
}

#[test]
fn does_not_treat_normal_length_stops_with_output_as_context_overflow() {
    assert!(!is_context_overflow(&length_stop(1_000, 0, 4_096, 0), Some(200_000)));
}

#[test]
fn does_not_treat_zero_output_length_stops_far_below_context_as_overflow() {
    assert!(!is_context_overflow(&length_stop(100, 0, 0, 0), Some(200_000)));
}
