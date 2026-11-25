//! `crates/ai/src/api/openai_completions.rs` 的测试。
//!
//! 上游出处（逐个用例在注释里注明）：`openai-completions-tool-choice.test.ts`、
//! `openai-completions-raw-stop-reason.test.ts`、`openai-completions-response-model.test.ts`、
//! `openai-completions-empty-tools.test.ts`、`openai-completions-non-null-assistant-content.test.ts`、
//! `openai-completions-tool-result-images.test.ts`、`provider-error-body-regression.test.ts`。
//!
//! 上游用 `vi.mock("openai")` 换掉 SDK，这里换成两条：直接喂 chunk 给组装器（大部分用例），
//! 或者起一台只应答一次的本地 HTTP 服务（传输、错误、取消、UTF-8 跨块）。没有搬的用例见
//! `PORTING.md` 差异表 #110。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use xjpcode_ai::api::openai_completions::{
    build_params, convert_messages, stream_simple, ChatCompletionsStreamState, NormalizedEnd,
};
use xjpcode_ai::providers::compat_schema::OpenAICompletionsCompat;
use xjpcode_ai::providers::deepseek;
use xjpcode_ai::providers::model_schema::{
    ModelCost, ModelInputModality, ModelThinkingLevel,
};
use xjpcode_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, JsonObject,
    Message, Model, SimpleStreamOptions, StopReason, TextContent, ThinkingContent, Tool, ToolCall,
    ToolResultContentBlock, ToolResultMessage, ToolResultRole, TranscriptContext, UserContent,
    UserContentBlock, UserMessage, UserRole,
};
use xjpcode_ai::utils::abort::AbortController;

fn model(
    id: &str,
    provider: &str,
    base_url: &str,
    reasoning: bool,
    input: Vec<ModelInputModality>,
    compat: Option<OpenAICompletionsCompat>,
) -> Model {
    Model {
        id: id.to_string(),
        name: id.to_string(),
        api: "openai-completions".to_string(),
        provider: provider.to_string(),
        base_url: base_url.to_string(),
        input,
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        r#type: None,
        reasoning,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 128_000,
        max_tokens: 8_192,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat,
    }
}

fn local_model() -> Model {
    model(
        "test-model",
        "local-vllm",
        "http://localhost:8000/v1",
        true,
        vec![ModelInputModality::Text],
        None,
    )
}

fn openai_model() -> Model {
    model(
        "gpt-4o-mini",
        "openai",
        "https://api.openai.com/v1",
        false,
        vec![ModelInputModality::Text],
        None,
    )
}

fn user_message(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp: 0,
    })
}

fn context(messages: Vec<Message>, tools: Option<Vec<Tool>>) -> TranscriptContext {
    let _ = tools;
    TranscriptContext { messages }
}

fn text_of(state: &ChatCompletionsStreamState) -> String {
    state
        .message()
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect()
}

fn thinking_of(state: &ChatCompletionsStreamState) -> String {
    state
        .message()
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Thinking(thinking) => Some(thinking.thinking.clone()),
            _ => None,
        })
        .collect()
}

fn tool_calls_of(state: &ChatCompletionsStreamState) -> Vec<ToolCall> {
    state
        .message()
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::ToolCall(tool_call) => Some(tool_call.clone()),
            _ => None,
        })
        .collect()
}

fn run_chunks(model: Model, chunks: &[Value]) -> (ChatCompletionsStreamState, Vec<AssistantMessageEvent>) {
    let mut state = ChatCompletionsStreamState::new(model);
    let mut events = Vec::new();
    for chunk in chunks {
        events.extend(state.apply_chunk(chunk));
    }
    events.extend(state.finish_blocks());
    (state, events)
}

// ---------------------------------------------------------------------------
// 流式组装（直接喂 chunk）
// ---------------------------------------------------------------------------

/// 上游：`accumulates mixed content, reasoning, and parallel tool call deltas independently`。
#[test]
fn accumulates_mixed_content_reasoning_and_parallel_tool_call_deltas_independently() {
    let chunks = vec![
        json!({
            "id": "chatcmpl-mixed-deltas",
            "choices": [{
                "delta": {
                    "content": "answer 1",
                    "reasoning_content": "think 1",
                    "tool_calls": [
                        {"index": 0, "id": "tc_read_initial", "type": "function",
                         "function": {"name": "read", "arguments": "{\"path\":\"README"}},
                        {"index": 1, "id": "tc_grep_initial", "type": "function",
                         "function": {"name": "grep", "arguments": "{\"pattern\":\"TODO"}},
                        {"id": "tc_list_no_index", "type": "function",
                         "function": {"name": "list", "arguments": "{\"path\":\"packages"}},
                        {"id": "tc_write_no_index", "type": "function",
                         "function": {"name": "write", "arguments": "{\"path\":\"out"}}
                    ]
                },
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-mixed-deltas",
            "choices": [{
                "delta": {
                    "content": " answer 2",
                    "tool_calls": [
                        {"index": 1, "id": "tc_grep_changed", "type": "function",
                         "function": {"arguments": "\",\"path\":\"src"}},
                        {"id": "tc_write_no_index", "type": "function",
                         "function": {"arguments": ".txt\",\"content\":\"ok\"}"}},
                        {"id": "tc_list_no_index", "type": "function",
                         "function": {"arguments": "/ai\"}"}}
                    ]
                },
                "finish_reason": null
            }]
        }),
        json!({
            "id": "chatcmpl-mixed-deltas",
            "choices": [{
                "delta": {
                    "content": "\n",
                    "reasoning_content": " think 2",
                    "tool_calls": [
                        {"index": 0, "id": "tc_read_changed", "type": "function",
                         "function": {"arguments": ".md\"}"}},
                        {"index": 1, "type": "function", "function": {"arguments": "\"}"}}
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 8,
                "prompt_tokens_details": {"cached_tokens": 0},
                "completion_tokens_details": {"reasoning_tokens": 2}
            }
        }),
    ];
    let (state, events) = run_chunks(local_model(), &chunks);

    let count = |name: &str| {
        events
            .iter()
            .filter(|event| {
                matches!(
                    (name, event),
                    ("text_start", AssistantMessageEvent::TextStart { .. })
                        | ("text_delta", AssistantMessageEvent::TextDelta { .. })
                        | ("text_end", AssistantMessageEvent::TextEnd { .. })
                        | ("thinking_start", AssistantMessageEvent::ThinkingStart { .. })
                        | ("thinking_delta", AssistantMessageEvent::ThinkingDelta { .. })
                        | ("thinking_end", AssistantMessageEvent::ThinkingEnd { .. })
                        | ("toolcall_start", AssistantMessageEvent::ToolcallStart { .. })
                        | ("toolcall_delta", AssistantMessageEvent::ToolcallDelta { .. })
                        | ("toolcall_end", AssistantMessageEvent::ToolcallEnd { .. })
                )
            })
            .count()
    };
    assert_eq!(count("text_start"), 1);
    assert_eq!(count("text_delta"), 3);
    assert_eq!(count("text_end"), 1);
    assert_eq!(count("thinking_start"), 1);
    assert_eq!(count("thinking_delta"), 2);
    assert_eq!(count("thinking_end"), 1);
    assert_eq!(count("toolcall_start"), 4);
    assert_eq!(count("toolcall_delta"), 9);
    assert_eq!(count("toolcall_end"), 4);

    // 工具事件按内容块分组，顺序是 start → deltas → end。
    let mut per_index: std::collections::BTreeMap<usize, Vec<&'static str>> = Default::default();
    for event in &events {
        match event {
            AssistantMessageEvent::ToolcallStart { content_index, .. } => {
                per_index.entry(*content_index).or_default().push("toolcall_start")
            }
            AssistantMessageEvent::ToolcallDelta { content_index, .. } => {
                per_index.entry(*content_index).or_default().push("toolcall_delta")
            }
            AssistantMessageEvent::ToolcallEnd { content_index, .. } => {
                per_index.entry(*content_index).or_default().push("toolcall_end")
            }
            _ => {}
        }
    }
    assert_eq!(
        per_index.get(&2).cloned().unwrap_or_default(),
        vec!["toolcall_start", "toolcall_delta", "toolcall_delta", "toolcall_end"]
    );
    assert_eq!(
        per_index.get(&3).cloned().unwrap_or_default(),
        vec![
            "toolcall_start",
            "toolcall_delta",
            "toolcall_delta",
            "toolcall_delta",
            "toolcall_end"
        ]
    );
    assert_eq!(
        per_index.get(&4).cloned().unwrap_or_default(),
        vec!["toolcall_start", "toolcall_delta", "toolcall_delta", "toolcall_end"]
    );
    assert_eq!(
        per_index.get(&5).cloned().unwrap_or_default(),
        vec!["toolcall_start", "toolcall_delta", "toolcall_delta", "toolcall_end"]
    );

    assert_eq!(state.message().content.len(), 6);
    assert_eq!(
        state.message().content[1],
        AssistantContentBlock::Text(TextContent::new("answer 1 answer 2\n"))
    );
    let tool_calls = tool_calls_of(&state);
    assert_eq!(tool_calls[0].id, "tc_read_initial");
    assert_eq!(tool_calls[0].name, "read");
    assert_eq!(
        Value::Object(tool_calls[0].arguments.clone()),
        json!({"path": "README.md"})
    );
    assert_eq!(tool_calls[1].id, "tc_grep_initial");
    assert_eq!(
        Value::Object(tool_calls[1].arguments.clone()),
        json!({"pattern": "TODO", "path": "src"})
    );
    assert_eq!(tool_calls[2].id, "tc_list_no_index");
    assert_eq!(
        Value::Object(tool_calls[2].arguments.clone()),
        json!({"path": "packages/ai"})
    );
    assert_eq!(tool_calls[3].id, "tc_write_no_index");
    assert_eq!(
        Value::Object(tool_calls[3].arguments.clone()),
        json!({"path": "out.txt", "content": "ok"})
    );
    assert_eq!(state.message().stop_reason, StopReason::ToolUse);
    assert_eq!(state.message().usage.input, 10);
    assert_eq!(state.message().usage.output, 8);
    assert_eq!(state.message().usage.reasoning, Some(2));
}

#[test]
fn preserves_the_thinking_signature_field_name() {
    let (state, _) = run_chunks(
        local_model(),
        &[json!({"choices": [{"delta": {"reasoning_content": "想一想"}, "finish_reason": "stop"}]})],
    );
    assert_eq!(thinking_of(&state), "想一想");
    match &state.message().content[0] {
        AssistantContentBlock::Thinking(thinking) => {
            assert_eq!(thinking.thinking_signature.as_deref(), Some("reasoning_content"))
        }
        other => panic!("expected a thinking block, got {other:?}"),
    }
}

/// 上游：`maps non-standard provider finish_reason values to stopReason error`。
#[test]
fn maps_non_standard_provider_finish_reason_values_to_stop_reason_error() {
    let mut state = ChatCompletionsStreamState::new(local_model());
    state.apply_chunk(&json!({"choices": [{"delta": {"content": "partial"}, "finish_reason": null}]}));
    state.apply_chunk(&json!({"choices": [{"delta": {}, "finish_reason": "network_error"}]}));
    state.finish_blocks();
    let error = state.finalize(None).expect_err("network_error is an error");
    assert_eq!(
        error,
        NormalizedEnd::Error("Provider finish_reason: network_error".to_string())
    );
    assert_eq!(state.message().stop_reason, StopReason::Error);
    assert_eq!(
        state.message().error_message.as_deref(),
        Some("Provider finish_reason: network_error")
    );
}

/// 上游：`ignores null stream chunks from openai-compatible providers`。
#[test]
fn ignores_null_stream_chunks() {
    let (state, _) = run_chunks(
        openai_model(),
        &[
            Value::Null,
            json!({"id": "chatcmpl-test", "choices": [{"delta": {"content": "OK"}, "finish_reason": null}]}),
            json!({
                "id": "chatcmpl-test",
                "choices": [{"delta": {}, "finish_reason": "stop"}],
                "usage": {
                    "prompt_tokens": 3,
                    "completion_tokens": 1,
                    "prompt_tokens_details": {"cached_tokens": 0},
                    "completion_tokens_details": {"reasoning_tokens": 0}
                }
            }),
        ],
    );
    assert_eq!(state.message().stop_reason, StopReason::Stop);
    assert_eq!(state.message().response_id.as_deref(), Some("chatcmpl-test"));
    assert_eq!(state.message().usage.total_tokens, 4);
    assert_eq!(
        state.message().content,
        vec![AssistantContentBlock::Text(TextContent::new("OK"))]
    );
}

/// 上游：`errors when a stream ends after only null finish_reason chunks`。
#[test]
fn errors_when_a_stream_ends_after_only_null_finish_reason_chunks() {
    let mut state = ChatCompletionsStreamState::new(openai_model());
    state.apply_chunk(&json!({
        "id": "chatcmpl-truncated",
        "choices": [{"delta": {"content": "partial answer"}, "finish_reason": null}]
    }));
    state.finish_blocks();
    assert_eq!(
        state.finalize(None),
        Err(NormalizedEnd::Error(
            "Stream ended without finish_reason".to_string()
        ))
    );
}

/// 上游：`accepts streams without finish_reason when compat disables it`。
#[test]
fn accepts_streams_without_finish_reason_when_compat_disables_it() {
    let mut compat = OpenAICompletionsCompat::default();
    compat.supports_finish_reason = Some(false);
    let model = model(
        "test-model",
        "local-vllm",
        "http://localhost:8000/v1",
        true,
        vec![ModelInputModality::Text],
        Some(compat),
    );
    let (mut state, _) = run_chunks(
        model,
        &[json!({
            "id": "chatcmpl-no-finish-reason",
            "choices": [{"delta": {"content": "complete answer"}, "finish_reason": null}]
        })],
    );
    assert_eq!(state.finalize(None), Ok(()));
    assert_eq!(state.message().stop_reason, StopReason::Stop);
    assert_eq!(text_of(&state), "complete answer");
}

/// 上游：`does not double-count reasoning tokens in completion usage`。
#[test]
fn does_not_double_count_reasoning_tokens_in_completion_usage() {
    let (state, _) = run_chunks(
        openai_model(),
        &[json!({
            "id": "chatcmpl-reasoning-usage",
            "choices": [{"delta": {}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 33,
                "prompt_tokens_details": {"cached_tokens": 0},
                "completion_tokens_details": {"reasoning_tokens": 21}
            }
        })],
    );
    assert_eq!(state.message().usage.input, 10);
    assert_eq!(state.message().usage.output, 33);
    assert_eq!(state.message().usage.total_tokens, 43);
}

/// 上游：`preserves prompt_tokens_details cache read/write fields from chunk usage`。
#[test]
fn preserves_prompt_tokens_details_cache_read_write_fields() {
    let (state, _) = run_chunks(
        openai_model(),
        &[json!({
            "id": "chatcmpl-cache-write",
            "choices": [{"delta": {"content": "OK"}, "finish_reason": null}]
        }),
        json!({
            "id": "chatcmpl-cache-write",
            "choices": [{"delta": {}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 5,
                "prompt_tokens_details": {"cached_tokens": 50, "cache_write_tokens": 30},
                "completion_tokens_details": {"reasoning_tokens": 0}
            }
        })],
    );
    assert_eq!(state.message().usage.input, 20);
    assert_eq!(state.message().usage.cache_read, 50);
    assert_eq!(state.message().usage.cache_write, 30);
    assert_eq!(state.message().usage.total_tokens, 105);
}

/// 上游：`preserves prompt_tokens_details cache read/write fields from choice usage fallback`。
#[test]
fn preserves_cache_fields_from_choice_usage_fallback() {
    let (state, _) = run_chunks(
        openai_model(),
        &[json!({
            "id": "chatcmpl-cache-write-choice",
            "choices": [{
                "delta": {"content": "OK"},
                "finish_reason": null,
                "usage": {
                    "prompt_tokens": 100,
                    "completion_tokens": 5,
                    "prompt_tokens_details": {"cached_tokens": 50, "cache_write_tokens": 30},
                    "completion_tokens_details": {"reasoning_tokens": 0}
                }
            }]
        })],
    );
    assert_eq!(state.message().usage.input, 20);
    assert_eq!(state.message().usage.cache_read, 50);
    assert_eq!(state.message().usage.cache_write, 30);
    assert_eq!(state.message().usage.total_tokens, 105);
}

/// 上游：`preserves raw finish reasons for successful stops` / `for provider error stops`。
#[test]
fn preserves_raw_finish_reasons() {
    let (state, _) = run_chunks(
        openai_model(),
        &[json!({"choices": [{"delta": {"content": "OK"}, "finish_reason": "stop"}]})],
    );
    assert_eq!(state.message().raw_stop_reason.as_deref(), Some("stop"));
    assert_eq!(state.message().stop_reason, StopReason::Stop);

    let (mut state, _) = run_chunks(
        openai_model(),
        &[json!({"choices": [{"delta": {"content": "blocked"}, "finish_reason": "content_filter"}]})],
    );
    assert_eq!(
        state.message().raw_stop_reason.as_deref(),
        Some("content_filter")
    );
    assert_eq!(
        state.message().error_message.as_deref(),
        Some("Provider finish_reason: content_filter")
    );
    assert!(matches!(
        state.finalize(None),
        Err(NormalizedEnd::Error(message)) if message == "Provider finish_reason: content_filter"
    ));
}

/// 上游：`surfaces routed chunk.model on responseModel without changing model` 等三条。
#[test]
fn surfaces_a_routed_response_model_only_when_it_differs() {
    let (state, _) = run_chunks(
        openai_model(),
        &[json!({
            "id": "chatcmpl-routed",
            "model": "gpt-4o-mini-2024-07-18",
            "choices": [{"delta": {"content": "OK"}, "finish_reason": "stop"}]
        })],
    );
    assert_eq!(state.message().model, "gpt-4o-mini");
    assert_eq!(
        state.message().response_model.as_deref(),
        Some("gpt-4o-mini-2024-07-18")
    );

    let (state, _) = run_chunks(
        openai_model(),
        &[json!({
            "id": "chatcmpl-same",
            "model": "gpt-4o-mini",
            "choices": [{"delta": {"content": "OK"}, "finish_reason": "stop"}]
        })],
    );
    assert_eq!(state.message().response_model, None);

    let (state, _) = run_chunks(
        openai_model(),
        &[json!({
            "id": "chatcmpl-missing",
            "model": "",
            "choices": [{"delta": {"content": "OK"}, "finish_reason": "stop"}]
        })],
    );
    assert_eq!(state.message().response_model, None);
}

/// 上游 `streaming-tool-arguments.test.ts` 的 OpenAI 一路：部分参数只增不减，最后整份精确。
#[test]
fn partial_tool_arguments_only_grow_and_finish_exactly() {
    let mut state = ChatCompletionsStreamState::new(local_model());
    let mut partials: Vec<JsonObject> = Vec::new();
    for fragment in ["{\"pa", "th\":", "\"READ", "ME.md\"", "}"] {
        for event in state.apply_chunk(&json!({
            "choices": [{
                "delta": {"tool_calls": [{"index": 0, "id": "t1", "function": {"name": "read", "arguments": fragment}}]},
                "finish_reason": null
            }]
        })) {
            if let AssistantMessageEvent::ToolcallDelta { partial, .. } = event {
                if let Some(AssistantContentBlock::ToolCall(tool_call)) = partial.content.last() {
                    partials.push(tool_call.arguments.clone());
                }
            }
        }
    }
    // 每一份都是前一份的前缀（同一个键的值只会变长）。
    for pair in partials.windows(2) {
        for (key, value) in &pair[0] {
            let later = pair[1]
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default();
            let earlier = value.as_str().unwrap_or_default();
            assert!(later.starts_with(earlier), "{later:?} is not prefixed by {earlier:?}");
        }
    }
    state.apply_chunk(&json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}));
    state.finish_blocks();
    let tool_calls = tool_calls_of(&state);
    assert_eq!(
        Value::Object(tool_calls[0].arguments.clone()),
        json!({"path": "README.md"})
    );
}

#[test]
fn broken_tool_arguments_are_reported_on_the_block() {
    let (state, _) = run_chunks(
        local_model(),
        &[
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "t1",
                "function": {"name": "read", "arguments": "{\"path\":"}}]}, "finish_reason": null}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
        ],
    );
    let tool_calls = tool_calls_of(&state);
    assert!(tool_calls[0].arguments.is_empty());
    assert!(tool_calls[0].arguments_error.is_some());
}

// ---------------------------------------------------------------------------
// 请求构造
// ---------------------------------------------------------------------------

/// 上游：`omits tools field when context.tools is an empty array / undefined`。
#[test]
fn omits_the_tools_field_when_there_are_no_request_tools() {
    let model = openai_model();
    let compat = xjpcode_ai::api::openai_completions::get_compat(&model);
    let params = build_params(
        &model,
        &context(vec![user_message("hi")], None),
        None,
        &compat,
    )
    .expect("params");
    assert!(!params.contains_key("tools"));
}

/// 上游：`sends default maxTokens` / `sends explicit maxTokens`（DeepSeek 的字段名）。
#[test]
fn sends_max_tokens_for_deepseek() {
    let model = deepseek::model("deepseek-v4-pro").expect("deepseek model");
    let compat = xjpcode_ai::api::openai_completions::get_compat(&model);
    let options = xjpcode_ai::api::openai_completions::OpenAICompletionsOptions {
        api_key: Some("test".to_string()),
        max_tokens: Some(1234),
        ..Default::default()
    };
    let params = build_params(&model, &context(vec![user_message("hi")], None), Some(&options), &compat)
        .expect("params");
    assert_eq!(params.get("max_tokens"), Some(&json!(1234)));
    assert_eq!(params.get("max_completion_tokens"), None);
    // DeepSeek 不发 store，也不发 developer 角色。
    assert_eq!(params.get("store"), None);
    assert_eq!(
        params.get("stream_options"),
        Some(&json!({"include_usage": true}))
    );
}

/// 上游：`sends max_tokens for built-in and custom DeepSeek API models`。
#[test]
fn sends_thinking_disabled_and_mapped_reasoning_effort_for_deepseek() {
    let model = deepseek::model("deepseek-v4-pro").expect("deepseek model");
    let compat = xjpcode_ai::api::openai_completions::get_compat(&model);
    let mut options = xjpcode_ai::api::openai_completions::OpenAICompletionsOptions {
        api_key: Some("test".to_string()),
        ..Default::default()
    };
    let params = build_params(&model, &context(vec![user_message("hi")], None), Some(&options), &compat)
        .expect("params");
    assert_eq!(params.get("thinking"), Some(&json!({"type": "disabled"})));

    options.reasoning_effort = Some(xjpcode_ai::providers::model_schema::ThinkingLevel::Max);
    let params = build_params(&model, &context(vec![user_message("hi")], None), Some(&options), &compat)
        .expect("params");
    assert_eq!(params.get("thinking"), Some(&json!({"type": "enabled"})));
    assert_eq!(params.get("reasoning_effort"), Some(&json!("max")));
}

/// 上游：`sends null content for tool-call-only assistant messages by default`。
#[test]
fn sends_null_content_for_tool_call_only_assistant_messages() {
    let model = openai_model();
    let compat = xjpcode_ai::api::openai_completions::get_compat(&model);
    let assistant = AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
            "t1",
            "read",
            JsonObject::from_iter([("path".to_string(), json!("README.md"))]),
        ))],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Default::default(),
        stop_reason: StopReason::ToolUse,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
        duration_ms: None,
    };
    let messages = convert_messages(
        &model,
        &context(vec![user_message("hi"), Message::Assistant(assistant)], None),
        &compat,
    );
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"], Value::Null);
    assert_eq!(messages[1]["tool_calls"][0]["id"], "t1");
    assert_eq!(
        messages[1]["tool_calls"][0]["function"]["arguments"],
        "{\"path\":\"README.md\"}"
    );
}

/// 上游：`sends empty string content without inserting an assistant message after tool results`。
#[test]
fn sends_empty_string_content_when_compat_requires_non_null_assistant_content() {
    let mut model = openai_model();
    model.compat = Some(OpenAICompletionsCompat {
        requires_non_null_assistant_content: Some(true),
        ..Default::default()
    });
    let compat = xjpcode_ai::api::openai_completions::get_compat(&model);
    let assistant = AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
            "t1",
            "read",
            JsonObject::new(),
        ))],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Default::default(),
        stop_reason: StopReason::ToolUse,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
        duration_ms: None,
    };
    let messages = convert_messages(
        &model,
        &context(vec![user_message("hi"), Message::Assistant(assistant)], None),
        &compat,
    );
    assert_eq!(messages[1]["content"], json!(""));
}

#[test]
fn inserts_the_synthetic_assistant_message_after_tool_results_when_required() {
    let mut model = openai_model();
    model.compat = Some(OpenAICompletionsCompat {
        requires_assistant_after_tool_result: Some(true),
        ..Default::default()
    });
    let compat = xjpcode_ai::api::openai_completions::get_compat(&model);
    let tool_result = Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "t1".to_string(),
        tool_name: "read".to_string(),
        content: vec![ToolResultContentBlock::Text(TextContent::new("file text"))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 0,
        duration_ms: None,
    });
    let messages = convert_messages(
        &model,
        &context(vec![tool_result, user_message("and now?")], None),
        &compat,
    );
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"], "I have processed the tool results.");
    assert_eq!(messages[2]["role"], "user");
}

/// 上游：`batches tool-result images after consecutive tool results`。
#[test]
fn batches_tool_result_images_after_consecutive_tool_results() {
    let model = model(
        "vision-model",
        "openai",
        "https://api.openai.com/v1",
        false,
        vec![ModelInputModality::Text, ModelInputModality::Image],
        None,
    );
    let compat = xjpcode_ai::api::openai_completions::get_compat(&model);
    let tool_result = |id: &str| {
        Message::ToolResult(ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: id.to_string(),
            tool_name: "read".to_string(),
            content: vec![
                ToolResultContentBlock::Text(TextContent::new("see image")),
                ToolResultContentBlock::Image(xjpcode_ai::ImageContent::new("AAAA", "image/png")),
            ],
            details: None,
            usage: None,
            nested_calls: None,
            is_error: false,
            timestamp: 0,
            duration_ms: None,
        })
    };
    let messages = convert_messages(
        &model,
        &context(vec![tool_result("t1"), tool_result("t2")], None),
        &compat,
    );
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["role"], "tool");
    assert_eq!(messages[1]["role"], "tool");
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(
        messages[2]["content"][0]["text"],
        "Attached image(s) from tool result:"
    );
    assert_eq!(
        messages[2]["content"][1]["image_url"]["url"],
        "data:image/png;base64,AAAA"
    );
    assert_eq!(
        messages[2]["content"][2]["image_url"]["url"],
        "data:image/png;base64,AAAA"
    );
}

#[test]
fn downgrades_images_for_text_only_models() {
    let model = openai_model();
    let compat = xjpcode_ai::api::openai_completions::get_compat(&model);
    let user = Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Blocks(vec![
            UserContentBlock::Text(TextContent::new("look")),
            UserContentBlock::Image(xjpcode_ai::ImageContent::new("AAAA", "image/png")),
        ]),
        timestamp: 0,
    });
    let messages = convert_messages(&model, &context(vec![user], None), &compat);
    assert_eq!(
        messages[0]["content"][1]["text"],
        "(image omitted: model does not support images)"
    );
}

#[test]
fn skips_errored_assistant_turns_and_closes_orphaned_tool_calls() {
    let model = openai_model();
    let compat = xjpcode_ai::api::openai_completions::get_compat(&model);
    let mut assistant = AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
            "t1",
            "read",
            JsonObject::new(),
        ))],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Default::default(),
        stop_reason: StopReason::ToolUse,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
        duration_ms: None,
    };
    let messages = convert_messages(
        &model,
        &context(vec![user_message("hi"), Message::Assistant(assistant.clone())], None),
        &compat,
    );
    assert_eq!(messages.last().unwrap()["role"], "tool");
    assert_eq!(messages.last().unwrap()["content"], "No result provided");

    assistant.stop_reason = StopReason::Error;
    let messages = convert_messages(
        &model,
        &context(vec![user_message("hi"), Message::Assistant(assistant)], None),
        &compat,
    );
    assert_eq!(messages.len(), 1);
}

#[test]
fn replayed_thinking_uses_the_signature_field_and_empty_reasoning_content_for_deepseek() {
    let model = deepseek::model("deepseek-v4-pro").expect("deepseek model");
    let compat = xjpcode_ai::api::openai_completions::get_compat(&model);
    let assistant = AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![
            AssistantContentBlock::Thinking(ThinkingContent {
                r#type: xjpcode_ai::ThinkingContentType::Thinking,
                thinking: "想一想".to_string(),
                thinking_signature: Some("reasoning_content".to_string()),
                redacted: None,
            }),
            AssistantContentBlock::Text(TextContent::new("答案")),
        ],
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Default::default(),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
        duration_ms: None,
    };
    let messages = convert_messages(
        &model,
        &context(vec![user_message("hi"), Message::Assistant(assistant)], None),
        &compat,
    );
    assert_eq!(messages[1]["reasoning_content"], "想一想");
    assert_eq!(messages[1]["content"], "答案");
}

// ---------------------------------------------------------------------------
// 传输：本地假服务
// ---------------------------------------------------------------------------

/// 起一台只应答一次的 HTTP 服务，把 `response` 原样写回去。
async fn serve_once(response: Vec<u8>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        loop {
            let read = socket.read(&mut buffer).await.unwrap_or(0);
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let _ = socket.write_all(&response).await;
        let _ = socket.flush().await;
        // 让客户端读完再关。
        tokio::time::sleep(Duration::from_millis(20)).await;
    });
    format!("http://{address}")
}

fn sse_response(events: &[&str]) -> Vec<u8> {
    let mut response = String::from(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
    );
    for event in events {
        response.push_str("data: ");
        response.push_str(event);
        response.push_str("\n\n");
    }
    response.push_str("data: [DONE]\n\n");
    response.into_bytes()
}

async fn collect(stream: xjpcode_ai::utils::event_stream::AssistantMessageEventStream) -> Vec<AssistantMessageEvent> {
    let mut consumer = stream.consumer();
    let mut events = Vec::new();
    while let Some(event) = consumer.next().await {
        events.push(event);
    }
    events
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_text_over_http() {
    let base_url = serve_once(sse_response(&[
        r#"{"id":"chatcmpl-http","choices":[{"delta":{"content":"你"},"finish_reason":null}]}"#,
        r#"{"id":"chatcmpl-http","choices":[{"delta":{"content":"好"},"finish_reason":null}]}"#,
        r#"{"id":"chatcmpl-http","choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":0},"completion_tokens_details":{"reasoning_tokens":0}}}"#,
    ]))
    .await;
    let model = model(
        "test-model",
        "local-vllm",
        &base_url,
        false,
        vec![ModelInputModality::Text],
        None,
    );
    let stream = stream_simple(
        model,
        context(vec![user_message("hi")], None),
        Some(SimpleStreamOptions {
            api_key: Some("test".to_string()),
            ..Default::default()
        }),
        None,
    );
    let events = collect(stream.clone()).await;
    let message = stream.result().await;
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(message.response_id.as_deref(), Some("chatcmpl-http"));
    assert_eq!(
        message.content,
        vec![AssistantContentBlock::Text(TextContent::new("你好"))]
    );
    assert!(events
        .iter()
        .any(|event| matches!(event, AssistantMessageEvent::Done { .. })));
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_multibyte_characters_split_across_tcp_writes() {
    let body = sse_response(&[
        r#"{"id":"chatcmpl-split","choices":[{"delta":{"content":"你好世界"},"finish_reason":"stop"}]}"#,
    ]);
    // 在「好」的 UTF-8 三字节中间劈开。
    let needle = "你好世界".as_bytes();
    let split_at = body
        .windows(needle.len())
        .position(|window| window == needle)
        .expect("text in body")
        + 4;
    let response = body[..split_at].to_vec();
    let rest = body[split_at..].to_vec();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        loop {
            let read = socket.read(&mut buffer).await.unwrap_or(0);
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let _ = socket.write_all(&response).await;
        let _ = socket.flush().await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        let _ = socket.write_all(&rest).await;
        let _ = socket.flush().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    });
    let base_url = format!("http://{address}");
    let model = model(
        "test-model",
        "local-vllm",
        &base_url,
        false,
        vec![ModelInputModality::Text],
        None,
    );
    let stream = stream_simple(
        model,
        context(vec![user_message("hi")], None),
        Some(SimpleStreamOptions {
            api_key: Some("test".to_string()),
            ..Default::default()
        }),
        None,
    );
    collect(stream.clone()).await;
    let message = stream.result().await;
    assert_eq!(
        message.content,
        vec![AssistantContentBlock::Text(TextContent::new("你好世界"))]
    );
}

/// 上游 `provider-error-body-regression.test.ts`：`openai-completions` 那条。
#[tokio::test(flavor = "multi_thread")]
async fn surfaces_status_and_body_on_http_errors() {
    let body = r#"{"error":{"message":"blocked by gateway WAF"}}"#;
    let response = format!(
        "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let base_url = serve_once(response.into_bytes()).await;
    let model = model(
        "test-model",
        "openrouter",
        &base_url,
        false,
        vec![ModelInputModality::Text],
        None,
    );
    let stream = stream_simple(
        model,
        context(vec![user_message("hi")], None),
        Some(SimpleStreamOptions {
            api_key: Some("test".to_string()),
            ..Default::default()
        }),
        None,
    );
    let events = collect(stream.clone()).await;
    assert!(events
        .iter()
        .all(|event| !matches!(event, AssistantMessageEvent::Start { .. })));
    let message = stream.result().await;
    assert_eq!(message.stop_reason, StopReason::Error);
    let error = message.error_message.expect("error message");
    assert!(error.contains("403"), "{error}");
    assert!(error.contains("blocked by gateway WAF"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_api_key_becomes_an_error_event() {
    let stream = stream_simple(
        openai_model(),
        context(vec![user_message("hi")], None),
        Some(SimpleStreamOptions::default()),
        None,
    );
    let events = collect(stream.clone()).await;
    assert!(matches!(
        events.last(),
        Some(AssistantMessageEvent::Error { .. })
    ));
    let message = stream.result().await;
    assert_eq!(
        message.error_message.as_deref(),
        Some("No API key for provider: openai")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn aborts_mid_stream_after_closing_started_blocks() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buffer = [0u8; 1024];
        let _ = socket.read(&mut buffer).await;
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n\
                    data: {\"id\":\"chatcmpl-slow\",\"choices\":[{\"delta\":{\"content\":\"先说一句\"},\"finish_reason\":null}]}\n\n";
        let _ = socket.write_all(head.as_bytes()).await;
        let _ = socket.flush().await;
        // 一直不结束：客户端必须靠取消收尾。
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let controller = AbortController::new();
    let model = model(
        "test-model",
        "local-vllm",
        &format!("http://{address}"),
        false,
        vec![ModelInputModality::Text],
        None,
    );
    let stream = stream_simple(
        model,
        context(vec![user_message("hi")], None),
        Some(SimpleStreamOptions {
            api_key: Some("test".to_string()),
            ..Default::default()
        }),
        Some(controller.signal()),
    );
    // 等第一段文字真的到了再打断：给固定 200 毫秒在机器忙的时候会赶在文字前面，
    // 测试就成了在比机器快慢，而不是看「已经开始的内容块有没有收尾」。
    let mut consumer = stream.consumer();
    let mut events: Vec<AssistantMessageEvent> = Vec::new();
    let mut aborted = false;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(10), consumer.next())
        .await
        .expect("an event within ten seconds")
    {
        if !aborted
            && matches!(
                event,
                AssistantMessageEvent::TextDelta { .. }
                    | AssistantMessageEvent::TextEnd { .. }
            )
        {
            aborted = true;
            controller.abort();
        }
        events.push(event);
    }
    let message = stream.result().await;
    assert_eq!(message.stop_reason, StopReason::Aborted);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Request was aborted")
    );
    assert_eq!(
        message.content,
        vec![AssistantContentBlock::Text(TextContent::new("先说一句"))]
    );
    assert!(events
        .iter()
        .any(|event| matches!(event, AssistantMessageEvent::Error { reason: StopReason::Aborted, .. })));
    assert!(events
        .iter()
        .any(|event| matches!(event, AssistantMessageEvent::TextEnd { .. })));
    assert!(!events
        .iter()
        .any(|event| matches!(event, AssistantMessageEvent::Done { .. })));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_malformed_stream_chunk_becomes_an_error_event() {
    let base_url = serve_once(sse_response(&["not json"])).await;
    let model = model(
        "test-model",
        "local-vllm",
        &base_url,
        false,
        vec![ModelInputModality::Text],
        None,
    );
    let stream = stream_simple(
        model,
        context(vec![user_message("hi")], None),
        Some(SimpleStreamOptions {
            api_key: Some("test".to_string()),
            ..Default::default()
        }),
        None,
    );
    collect(stream.clone()).await;
    let message = stream.result().await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert!(message
        .error_message
        .as_deref()
        .unwrap_or_default()
        .starts_with("Failed to parse stream chunk:"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_that_ends_without_finish_reason_is_an_error() {
    let base_url = serve_once(sse_response(&[
        r#"{"id":"chatcmpl-truncated","choices":[{"delta":{"content":"partial"},"finish_reason":null}]}"#,
    ]))
    .await;
    let model = model(
        "test-model",
        "local-vllm",
        &base_url,
        false,
        vec![ModelInputModality::Text],
        None,
    );
    let stream = stream_simple(
        model,
        context(vec![user_message("hi")], None),
        Some(SimpleStreamOptions {
            api_key: Some("test".to_string()),
            ..Default::default()
        }),
        None,
    );
    collect(stream.clone()).await;
    let message = stream.result().await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(
        message.error_message.as_deref(),
        Some("Stream ended without finish_reason")
    );
    assert_eq!(
        message.content,
        vec![AssistantContentBlock::Text(TextContent::new("partial"))]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_provider_chunks_to_on_provider_stream_event() {
    let base_url = serve_once(sse_response(&[
        r#"{"id":"chatcmpl-one","choices":[{"delta":{"content":"a"},"finish_reason":null}]}"#,
        r#"{"id":"chatcmpl-two","choices":[{"delta":{"content":"b"},"finish_reason":"stop"}]}"#,
    ]))
    .await;
    let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let callback: xjpcode_ai::Callback<xjpcode_ai::OnProviderStreamEventFn> = {
        let seen = seen.clone();
        xjpcode_ai::Callback::new(Arc::new(move |chunk: &Value, _model: &Model| {
            let id = chunk
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string);
            let seen = seen.clone();
            Box::pin(async move {
                seen.lock().expect("seen").push(id);
            }) as xjpcode_ai::BoxFuture<'_, ()>
        }))
    };
    let model = model(
        "test-model",
        "local-vllm",
        &base_url,
        false,
        vec![ModelInputModality::Text],
        None,
    );
    let stream = stream_simple(
        model,
        context(vec![user_message("hi")], None),
        Some(SimpleStreamOptions {
            api_key: Some("test".to_string()),
            on_provider_stream_event: callback,
            ..Default::default()
        }),
        None,
    );
    collect(stream).await;
    let seen = seen.lock().expect("seen").clone();
    assert_eq!(
        seen,
        vec![
            Some("chatcmpl-one".to_string()),
            Some("chatcmpl-two".to_string())
        ]
    );
}

/// `stream_simple` 的 `reasoning` 会被夹到模型支持的一档；DeepSeek 的映射表把
/// `medium` 标成不支持，于是落到 `high`。
#[test]
fn clamps_simple_reasoning_to_a_supported_level() {
    let model = deepseek::model("deepseek-flash").expect("deepseek model");
    let clamped = xjpcode_ai::models::clamp_thinking_level(&model, ModelThinkingLevel::Medium);
    assert_eq!(clamped, ModelThinkingLevel::High);
}

#[test]
fn parses_every_compat_key_of_the_provider_superset() {
    let compat: OpenAICompletionsCompat = serde_json::from_value(json!({
        "supportsStore": false,
        "supportsDeveloperRole": false,
        "supportsReasoningEffort": true,
        "supportsUsageInStreaming": true,
        "supportsFinishReason": false,
        "maxTokensField": "max_tokens",
        "requiresToolResultName": true,
        "requiresAssistantAfterToolResult": true,
        "requiresNonNullAssistantContent": true,
        "requiresToolsWithToolHistory": true,
        "requiresThinkingAsText": true,
        "requiresReasoningContentOnAssistantMessages": true,
        "thinkingFormat": "deepseek",
        "openRouterRouting": {"only": ["a"]},
        "vercelGatewayRouting": {"order": ["b"]},
        "chatTemplateKwargs": {"enable_thinking": {"$var": "thinking.enabled"}},
        "chatTemplateArgs": {"keep": true},
        "zaiToolStream": true,
        "supportsThinkingTokenBudget": true,
        "thinkingTokenBudgetField": "thinking_budget",
        "supportsStrictMode": true,
        "supportsOpenAIGrammarTools": true,
        "supportsMidConvoSystemMessages": true,
        "supportsMidConvoToolAdditions": true,
        "cacheControlFormat": "anthropic",
        "sendSessionAffinityHeaders": true,
        "sessionAffinityFormat": "openrouter",
        "supportsLongCacheRetention": false,
        "promptCacheKeyMode": "enabled",
        "vllmPriority": 3
    }))
    .expect("compat");
    assert_eq!(compat.supports_open_ai_grammar_tools, Some(true));
    assert_eq!(
        compat.max_tokens_field,
        Some(xjpcode_ai::providers::compat_schema::MaxTokensField::MaxTokens)
    );
    assert_eq!(
        compat.thinking_format,
        Some(xjpcode_ai::providers::compat_schema::ThinkingFormat::Deepseek)
    );
    assert_eq!(
        compat.thinking_token_budget_field,
        Some(xjpcode_ai::providers::compat_schema::ThinkingTokenBudgetField::ThinkingBudget)
    );
    assert_eq!(compat.vllm_priority, Some(3.0));
}

#[test]
fn detects_deepseek_compat_from_the_model_catalog() {
    let model = deepseek::model("deepseek-v4-pro").expect("deepseek model");
    let compat = xjpcode_ai::api::openai_completions::get_compat(&model);
    assert!(!compat.supports_store);
    assert!(!compat.supports_developer_role);
    assert!(compat.requires_reasoning_content_on_assistant_messages);
    assert_eq!(
        compat.max_tokens_field,
        xjpcode_ai::providers::compat_schema::MaxTokensField::MaxTokens
    );
    assert_eq!(
        compat.thinking_format,
        xjpcode_ai::providers::compat_schema::ThinkingFormat::Deepseek
    );
}

// ---------------------------------------------------------------------------
// 流的时限：只管等到响应开始（上游 OpenAI SDK 的 timeout 语义）
// ---------------------------------------------------------------------------

async fn accept_and_wait(listener: tokio::net::TcpListener, delay: Duration) -> tokio::net::TcpStream {
    let (mut socket, _) = listener.accept().await.expect("accept");
    let mut request = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        let read = socket.read(&mut buffer).await.unwrap_or(0);
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    tokio::time::sleep(delay).await;
    socket
}

#[tokio::test(flavor = "multi_thread")]
async fn times_out_while_waiting_for_the_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        let _socket = accept_and_wait(listener, Duration::from_millis(300)).await;
    });
    let model = model(
        "test-model",
        "local-vllm",
        &format!("http://{address}"),
        false,
        vec![ModelInputModality::Text],
        None,
    );
    let stream = stream_simple(
        model,
        context(vec![user_message("hi")], None),
        Some(SimpleStreamOptions {
            api_key: Some("test".to_string()),
            timeout_ms: Some(50),
            ..Default::default()
        }),
        None,
    );
    let message = stream.result().await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.error_message.as_deref(), Some("Request timed out"));
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_reading_the_body_after_the_timeout_window() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let body = sse_response(&[
        r#"{"id":"chatcmpl-slow","choices":[{"delta":{"content":"slow"},"finish_reason":null}]}"#,
        r#"{"id":"chatcmpl-slow","choices":[{"delta":{"content":" body"},"finish_reason":"stop"}]}"#,
    ]);
    tokio::spawn(async move {
        let mut socket = accept_and_wait(listener, Duration::ZERO).await;
        let split_at = body.len() / 2;
        let _ = socket.write_all(&body[..split_at]).await;
        let _ = socket.flush().await;
        // 正文流继续得比 timeout 久，仍然应当收完。
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = socket.write_all(&body[split_at..]).await;
        let _ = socket.flush().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    });
    let model = model(
        "test-model",
        "local-vllm",
        &format!("http://{address}"),
        false,
        vec![ModelInputModality::Text],
        None,
    );
    let stream = stream_simple(
        model,
        context(vec![user_message("hi")], None),
        Some(SimpleStreamOptions {
            api_key: Some("test".to_string()),
            timeout_ms: Some(50),
            ..Default::default()
        }),
        None,
    );
    let message = stream.result().await;
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(
        message.content,
        vec![AssistantContentBlock::Text(TextContent::new("slow body"))]
    );
}

// ---------------------------------------------------------------------------
// strict：用 pi 1.6.0 实际目录里 DeepSeek 的支持声明
// ---------------------------------------------------------------------------

fn strict_tool(parameters: Value) -> Tool {
    Tool {
        name: "sample_tool".to_string(),
        description: "Sample tool".to_string(),
        parameters,
        constrained_sampling: Some(xjpcode_ai::ConstrainedSampling::Config(
            xjpcode_ai::ConstrainedSamplingConfig::JsonSchema {
                strict: xjpcode_ai::types::ConstrainedSamplingStrict::Prefer,
            },
        )),
    }
}

#[test]
fn sends_strict_tool_schemas_for_the_deepseek_catalog() {
    let model = deepseek::model("deepseek-flash").expect("deepseek model");
    let compat = xjpcode_ai::api::openai_completions::get_compat(&model);
    assert!(compat.supports_strict_mode);
    let tools = vec![strict_tool(json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "offset": { "type": "number" }
        },
        "required": ["path"]
    }))];
    let converted = xjpcode_ai::api::openai_completions::convert_tools(&tools, &compat).unwrap();
    let function = &converted[0]["function"];
    assert_eq!(function["strict"], true);
    assert_eq!(function["parameters"]["required"], json!(["path", "offset"]));
    assert_eq!(function["parameters"]["additionalProperties"], false);
    assert_eq!(
        function["parameters"]["properties"]["offset"],
        json!({"anyOf": [{"type": "number"}, {"type": "null"}]})
    );
    assert_eq!(
        function["parameters"]["properties"]["path"],
        json!({"type": "string"})
    );
}

#[test]
fn falls_back_to_non_strict_for_schemas_strict_mode_cannot_take() {
    let model = deepseek::model("deepseek-flash").expect("deepseek model");
    let compat = xjpcode_ai::api::openai_completions::get_compat(&model);
    let parameters = json!({
        "type": "object",
        "properties": { "child": { "$ref": "https://example.com/child.json" } },
        "required": ["child"]
    });
    let converted =
        xjpcode_ai::api::openai_completions::convert_tools(&[strict_tool(parameters.clone())], &compat)
            .unwrap();
    assert_eq!(converted[0]["function"]["strict"], false);
    assert_eq!(converted[0]["function"]["parameters"], parameters);
}

#[test]
fn omits_strict_when_the_provider_does_not_support_it() {
    let model = model(
        "test-model",
        "local-vllm",
        "http://127.0.0.1:1",
        false,
        vec![ModelInputModality::Text],
        None,
    );
    let compat = xjpcode_ai::api::openai_completions::get_compat(&model);
    assert!(!compat.supports_strict_mode);
    let converted = xjpcode_ai::api::openai_completions::convert_tools(
        &[strict_tool(json!({"type": "object", "properties": {}}))],
        &compat,
    )
    .unwrap();
    assert!(converted[0]["function"].get("strict").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn reuses_the_same_http_connection_for_the_next_request() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    let accepted = Arc::new(AtomicUsize::new(0));
    let accepted_for_server = Arc::clone(&accepted);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            accepted_for_server.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let body = sse_response(&[
                    r#"{"id":"chatcmpl-reuse","choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
                ]);
                // 同一条连接上按顺序应答两轮；没复用时，第二轮会落到下一条连接。
                for _ in 0..2 {
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        let read = socket.read(&mut buffer).await.unwrap_or(0);
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..read]);
                    }
                    let content_length = String::from_utf8_lossy(&request)
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    let header_end = request
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .map(|index| index + 4)
                        .unwrap_or(request.len());
                    while request.len().saturating_sub(header_end) < content_length {
                        let read = socket.read(&mut buffer).await.unwrap_or(0);
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..read]);
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: keep-alive\r\n\r\n{}",
                        body.len(),
                        String::from_utf8_lossy(&body)
                    );
                    if socket.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                    let _ = socket.flush().await;
                }
            });
        }
    });

    let model = model(
        "test-model",
        "local-vllm",
        &format!("http://{address}"),
        false,
        vec![ModelInputModality::Text],
        None,
    );
    for _ in 0..2 {
        let stream = stream_simple(
            model.clone(),
            context(vec![user_message("hi")], None),
            Some(SimpleStreamOptions {
                api_key: Some("test".to_string()),
                ..Default::default()
            }),
            None,
        );
        let message = stream.result().await;
        assert_eq!(message.stop_reason, StopReason::Stop);
    }
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "two requests should share one pooled connection"
    );
}
