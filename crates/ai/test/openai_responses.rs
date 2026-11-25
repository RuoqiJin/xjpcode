//! 对应上游 `packages/ai/test/openai-responses-*.test.ts` 里走到的分支：
//! 别家工具调用 id 的改写、空工具结果、消息 id、命名空间、终止事件与参数收尾。
//!
//! 上游把事件写成 async generator；这里塞进一条通道，`process_responses_stream` 逐条读。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use xjpcode_ai::api::openai_responses_shared::{
    convert_responses_messages, create_output, process_responses_stream, short_hash,
    ConvertResponsesMessagesOptions, OpenAIResponsesStreamOptions,
};
use xjpcode_ai::providers::model_schema::{ModelCost, ModelThinkingLevel};
use xjpcode_ai::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream,
    AssistantRole, Message, Model, StopReason, TextContent, ToolCall, ToolResultContentBlock,
    ToolResultMessage, ToolResultRole, TranscriptContext, Usage, UsageCost, UserContent, UserMessage,
    UserRole,
};

fn model(provider: &str, id: &str, api: &str) -> Model {
    Model {
        id: id.to_string(),
        name: id.to_string(),
        api: api.to_string(),
        provider: provider.to_string(),
        base_url: "https://example.test".to_string(),
        input: vec![xjpcode_ai::providers::model_schema::ModelInputModality::Text],
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        r#type: None,
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 400_000,
        max_tokens: 128_000,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

fn codex_model() -> Model {
    model("openai-codex", "gpt-6.1-sol", "openai-codex-responses")
}

fn user(text: &str, timestamp: u64) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp,
    })
}

fn assistant(content: Vec<AssistantContentBlock>, provider: &str, api: &str, model_id: &str) -> Message {
    Message::Assistant(AssistantMessage {
        role: AssistantRole::Assistant,
        content,
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

fn tool_result(tool_call_id: &str, text: &str) -> Message {
    Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: tool_call_id.to_string(),
        tool_name: "bash".to_string(),
        content: vec![ToolResultContentBlock::Text(TextContent::new(text))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 2,
        duration_ms: None,
    })
}

fn allowed() -> BTreeSet<String> {
    ["openai", "openai-codex", "opencode"]
        .iter()
        .map(|value| value.to_string())
        .collect()
}

fn convert(model: &Model, messages: Vec<Message>) -> Vec<Value> {
    convert_responses_messages(
        model,
        &TranscriptContext { messages },
        &allowed(),
        Some(&ConvertResponsesMessagesOptions {
            supports_mid_convo_system_messages: false,
            supports_additional_tools: false,
            supports_tool_search: false,
            ..Default::default()
        }),
    )
}

fn processed(events: Vec<Value>) -> (AssistantMessage, Vec<AssistantMessageEvent>, Result<(), xjpcode_ai::auth::ModelsError>) {
    let model = model("openai", "gpt-5-mini", "openai-responses");
    let mut output = create_output(&model);
    let stream = AssistantMessageEventStream::new();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    for event in events {
        tx.send(Ok(event)).unwrap();
    }
    drop(tx);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut consumer = stream.consumer();
    let collector = runtime.spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = consumer.next().await {
            events.push(event);
        }
        events
    });
    let result = runtime.block_on(process_responses_stream(
        rx,
        &mut output,
        &stream,
        &model,
        &OpenAIResponsesStreamOptions::default(),
    ));
    stream.end(None);
    let events = runtime.block_on(collector).unwrap();
    (output, events, result)
}

#[test]
fn hashes_foreign_tool_item_ids_into_a_bounded_codex_safe_shape() {
    let raw = "call_4VnzVawQXPB9MgYib7CiQFEY|I9b95oN1wD/cHXKTw3PpRkL6KkCtzTJhUxMouMWYwHeTo2j3htzfSk7YPx2vifiIM4g3A8XXyOj8q4Bt6SLUG7gqY1E3ELkrkVQNHglRfUmWj84lqxJY+Puieb3VKyX0FB+83TUzn91cDMF/4gzt990IzqVrc+nIb9RRscRD070Du16q1glydVjWR0SBJsE6TbY/esOjFpqplogQqrajm1eI++f3eLi73R6q7hVusY0QbeFySVxABCjhN0lXB04caBe1rzHjYzul6MAXj7uq+0r17VLq+yrtyYhN12wkmFqHeqTyEei6EFPbMy24Nc+IbJlkP0OCg02W+gOnyBFcbi2ctvJFSOhSjt1CqBdqCnnhwUqXjbWiT0wh3DmLScRgTHmGkaI+oAcQQjfic65nxj+TnEkReA==";
    let messages = vec![
        user("Use the tool.", 0),
        assistant(
            vec![AssistantContentBlock::ToolCall(ToolCall::new(
                raw,
                "edit",
                serde_json::json!({"path":"src/styles/app.css"}).as_object().unwrap().clone(),
            ))],
            "github-copilot",
            "openai-responses",
            "gpt-5.5",
        ),
        tool_result(raw, "ok"),
    ];
    let input = convert(&codex_model(), messages);
    let function_call = input
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .expect("function_call item");
    let expected = format!("fc_{}", short_hash(raw.split('|').nth(1).unwrap()));
    assert_eq!(
        function_call.get("id").and_then(Value::as_str),
        Some(expected.as_str())
    );
    assert!(expected.len() <= 64);
    assert!(expected.chars().skip(3).all(|ch| ch.is_ascii_alphanumeric()));
    let call_id = function_call.get("call_id").and_then(Value::as_str).unwrap();
    assert!(call_id.starts_with("call_4VnzVawQXPB9MgYib7CiQFEY"));
    assert!(!call_id.contains('/'));
}

#[test]
fn uses_a_no_tool_output_placeholder_for_blank_results() {
    for text in ["", "\r\n", " \n\t"] {
        let messages = vec![
            user("Run the command", 0),
            assistant(
                vec![AssistantContentBlock::ToolCall(ToolCall::new(
                    "tool-1",
                    "bash",
                    Default::default(),
                ))],
                "openai",
                "openai-responses",
                "gpt-4o-mini",
            ),
            tool_result("tool-1", text),
        ];
        let input = convert(&model("openai", "gpt-4o-mini", "openai-responses"), messages);
        let output = input
            .iter()
            .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call_output"))
            .and_then(|item| item.get("output"))
            .and_then(Value::as_str);
        assert_eq!(output, Some("(no tool output)"), "text={text:?}");
    }
}

#[test]
fn generates_unique_fallback_message_ids_for_several_text_blocks() {
    let messages = vec![
        user("hello", 0),
        assistant(
            vec![
                AssistantContentBlock::Text(TextContent::new("first answer")),
                AssistantContentBlock::Text(TextContent::new("visible answer")),
            ],
            "anthropic",
            "anthropic-messages",
            "claude-opus-4-8",
        ),
    ];
    let input = convert(&codex_model(), messages);
    let ids: Vec<&str> = input
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .collect();
    assert_eq!(ids, vec!["msg_pi_1", "msg_pi_1_1"]);
}

#[test]
fn rejects_a_stream_that_ends_before_a_terminal_event() {
    let (_, _, result) = processed(vec![
        json!({"type": "response.created", "response": {"id": "resp_early"}}),
        json!({"type": "response.output_item.added", "output_index": 0,
               "item": {"type": "reasoning", "id": "rs_1", "summary": []}}),
        json!({"type": "response.reasoning_text.delta", "output_index": 0,
               "delta": "partial reasoning"}),
    ]);
    assert_eq!(
        result.unwrap_err().message,
        "OpenAI Responses stream ended before a terminal response event"
    );
}

#[test]
fn rejects_a_completed_stream_with_an_unfinished_tool_call() {
    let (_, _, result) = processed(vec![
        json!({"type": "response.output_item.added", "output_index": 0,
               "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "bash", "arguments": ""}}),
        json!({"type": "response.completed", "response": {"status": "completed"}}),
    ]);
    assert_eq!(
        result.unwrap_err().message,
        "OpenAI Responses stream completed with an unfinished tool call: bash (call_1|fc_1)"
    );
}

#[test]
fn rejects_parallel_tool_calls_that_share_a_missing_output_index() {
    let (_, _, result) = processed(vec![
        json!({"type": "response.output_item.added",
               "item": {"type": "function_call", "id": "fc_a", "call_id": "call_a", "name": "bash", "arguments": ""}}),
        json!({"type": "response.output_item.added",
               "item": {"type": "function_call", "id": "fc_b", "call_id": "call_b", "name": "bash", "arguments": ""}}),
        json!({"type": "response.function_call_arguments.delta", "delta": "{\"command\":\"echo a\"}"}),
        json!({"type": "response.output_item.done",
               "item": {"type": "function_call", "id": "fc_a", "call_id": "call_a", "name": "bash", "arguments": "{\"command\":\"echo a\"}"}}),
        json!({"type": "response.output_item.done",
               "item": {"type": "function_call", "id": "fc_b", "call_id": "call_b", "name": "bash", "arguments": "{\"command\":\"echo a\"}"}}),
        json!({"type": "response.completed", "response": {"status": "completed"}}),
    ]);
    assert_eq!(
        result.unwrap_err().message,
        "OpenAI Responses stream completed with an unfinished tool call: bash (call_a|fc_a)"
    );
}

#[test]
fn round_trips_a_function_namespace_received_only_on_output_item_done() {
    let (output, events, result) = processed(vec![
        json!({"type": "response.output_item.added", "output_index": 0,
               "item": {"type": "function_call", "id": "fc_test", "call_id": "call_test", "name": "lookup", "arguments": ""}}),
        json!({"type": "response.output_item.done", "output_index": 0,
               "item": {"type": "function_call", "id": "fc_test", "call_id": "call_test", "name": "lookup",
                        "arguments": "{\"value\":\"hello\"}", "namespace": "dynamic_tools"}}),
        json!({"type": "response.completed", "response": {"status": "completed"}}),
    ]);
    result.unwrap();
    let tool_call = output
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::ToolCall(tool_call) => Some(tool_call.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(tool_call.namespace.as_deref(), Some("dynamic_tools"));
    assert_eq!(tool_call.arguments.get("value").and_then(Value::as_str), Some("hello"));
    assert!(events.iter().any(|event| matches!(event, AssistantMessageEvent::ToolcallEnd { .. })));

    // 同一个模型重放时带上命名空间；换成别家工具时丢掉。
    let messages = vec![
        user("hello", 0),
        Message::Assistant(output.clone()),
    ];
    let replayed = convert(
        &model("openai", "gpt-5-mini", "openai-responses"),
        messages.clone(),
    );
    let item = replayed
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .unwrap();
    assert_eq!(item.get("namespace").and_then(Value::as_str), Some("dynamic_tools"));

    let dropped = convert(
        &model("openai-codex", "gpt-6.1-sol", "openai-codex-responses"),
        messages,
    );
    let item = dropped
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .unwrap();
    assert_eq!(item.get("namespace"), None);
}

#[test]
fn cleans_partial_json_at_output_item_done() {
    let (output, _, result) = processed(vec![
        json!({"type": "response.output_item.added",
               "item": {"type": "function_call", "id": "fc_test", "call_id": "call_test", "name": "edit", "arguments": ""}}),
        json!({"type": "response.function_call_arguments.delta", "delta": "{\"path\":\"README.md\""}),
        json!({"type": "response.function_call_arguments.delta", "delta": ",\"content\":\"updated\"}"}),
        json!({"type": "response.function_call_arguments.done", "arguments": "{\"path\":\"README.md\",\"content\":\"updated\"}"}),
        json!({"type": "response.output_item.done",
               "item": {"type": "function_call", "id": "fc_test", "call_id": "call_test", "name": "edit",
                        "arguments": "{\"path\":\"README.md\",\"content\":\"updated\"}"}}),
        json!({"type": "response.completed", "response": {"id": "resp_test", "status": "completed"}}),
    ]);
    result.unwrap();
    let tool_call = output
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::ToolCall(tool_call) => Some(tool_call.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(tool_call.arguments.get("path").and_then(Value::as_str), Some("README.md"));
    assert_eq!(tool_call.arguments.get("content").and_then(Value::as_str), Some("updated"));
    assert!(tool_call.arguments_error.is_none());
    assert_eq!(output.stop_reason, StopReason::ToolUse);
}

#[test]
fn maps_incomplete_reasons_to_length_or_error() {
    let (output, _, result) = processed(vec![json!({
        "type": "response.incomplete",
        "response": {"status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"},
                     "usage": {"input_tokens": 5, "output_tokens": 3, "total_tokens": 8}}
    })]);
    result.unwrap();
    assert_eq!(output.stop_reason, StopReason::Length);
    assert_eq!(output.usage.total_tokens, 8);

    let (output, _, result) = processed(vec![json!({
        "type": "response.incomplete",
        "response": {"status": "incomplete", "incomplete_details": {"reason": "content_filter"}}
    })]);
    result.unwrap();
    assert_eq!(output.stop_reason, StopReason::Error);
    assert_eq!(
        output.error_message.as_deref(),
        Some("Response incomplete: content_filter")
    );
}

#[test]
fn thinking_level_map_drives_the_codex_effort() {
    let mut model = codex_model();
    model.thinking_level_map = Some(BTreeMap::from([
        (ModelThinkingLevel::Off, None),
        (ModelThinkingLevel::Minimal, Some("low".to_string())),
        (ModelThinkingLevel::Max, Some("max".to_string())),
    ]));
    assert_eq!(
        xjpcode_ai::api::openai_responses_shared::resolve_reasoning_effort(
            &model,
            ModelThinkingLevel::Off
        ),
        "none"
    );
    assert_eq!(
        xjpcode_ai::api::openai_responses_shared::resolve_reasoning_effort(
            &model,
            ModelThinkingLevel::Minimal
        ),
        "low"
    );
    assert_eq!(
        xjpcode_ai::api::openai_responses_shared::resolve_reasoning_effort(
            &model,
            ModelThinkingLevel::Medium
        ),
        "medium"
    );
}
