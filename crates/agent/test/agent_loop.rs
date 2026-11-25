//! 移植自上游 `packages/agent/test/agent-loop.test.ts`。
//!
//! 上游的 `obj({...})`/`str()` 是 pi-schema 的 builder；这里直接写它给出的那份 JSON。
//! `queueMicrotask(() => stream.push(...))` 在 Rust 侧不需要：`EventStream` 在没有消费者时先排队
//! （差异 #30、#31），同步 `push` 与上游同一个可见顺序。

#[path = "utils/tools.rs"]
mod tools;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use xjpcode_agent::{
    agent_loop, agent_loop_continue, empty_assistant_message, empty_usage, run_agent_loop,
    run_tool_call, set_default_stream_fn, AgentContext, AgentEvent, AgentLoopConfig, AgentMessage,
    ConvertToLlm, RunToolCallOptions, StreamFn, ToolError, ToolExecutionMode,
};
use xjpcode_ai::providers::model_schema::{ModelCost, ModelInputModality};
use xjpcode_ai::{
    create_assistant_message_event_stream, AbortController, AssistantContentBlock,
    AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, EventStream, JsonObject,
    Message, Model, StopReason, TextContent, ToolCall, ToolResultContentBlock, ToolResultMessage,
    UserContent, UserMessage, UserRole,
};

use tools::{
    after_tool_call as after_hook, before_tool_call as before_hook, boxed,
    finish_turn as finish_hook, prepare_next_turn as prepare_next_turn_hook,
    prepare_request as prepare_request_hook, text_block, transform_context as transform_hook,
    value_of, value_schema, TestTool,
};

fn model() -> Model {
    Model {
        id: "mock".to_string(),
        name: "mock".to_string(),
        api: "openai-responses".to_string(),
        provider: "openai".to_string(),
        base_url: "https://example.invalid".to_string(),
        input: vec![ModelInputModality::Text],
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        r#type: None,
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 8192,
        max_tokens: 2048,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

fn text_block_content(text: &str) -> AssistantContentBlock {
    AssistantContentBlock::Text(TextContent::new(text))
}

fn tool_call_content(id: &str, name: &str, arguments: Value) -> AssistantContentBlock {
    AssistantContentBlock::ToolCall(ToolCall::new(
        id,
        name,
        arguments.as_object().expect("an object").clone(),
    ))
}

fn assistant_message(
    content: Vec<AssistantContentBlock>,
    stop_reason: StopReason,
) -> AssistantMessage {
    let mut message = empty_assistant_message("openai-responses", "openai", "mock");
    message.content = content;
    message.stop_reason = stop_reason;
    message.usage = empty_usage();
    message
}

fn text_message(text: &str) -> AssistantMessage {
    assistant_message(vec![text_block_content(text)], StopReason::Stop)
}

fn user_message(text: &str) -> AgentMessage {
    AgentMessage::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp: 0,
    })
}

/// `Message::User` 的正文（上游 `message.content` 是 string 或块数组）。
fn user_message_text(message: &Message) -> Option<String> {
    match message {
        Message::User(message) => Some(match &message.content {
            UserContent::Text(text) => text.clone(),
            UserContent::Blocks(blocks) => xjpcode_ai::content_text(blocks, ""),
        }),
        _ => None,
    }
}

fn identity_converter() -> ConvertToLlm {
    Arc::new(|messages: &[AgentMessage]| {
        let messages: Vec<Message> = messages
            .iter()
            .filter_map(|message| match message {
                AgentMessage::System(message) => Some(Message::System(message.clone())),
                AgentMessage::User(message) => Some(Message::User(message.clone())),
                AgentMessage::Assistant(message) => Some(Message::Assistant(message.clone())),
                AgentMessage::ToolResult(message) => Some(Message::ToolResult(message.clone())),
                AgentMessage::Custom(_) => None,
            })
            .collect();
        boxed(async move { messages })
    })
}

/// 测试的起点：与上游 `{model: createModel(), convertToLlm: identityConverter, ...config}` 同义。
fn config() -> AgentLoopConfig {
    AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        ..Default::default()
    }
}

fn empty_context() -> AgentContext {
    AgentContext {
        messages: Vec::new(),
        tools: None,
    }
}

fn done_stream(message: AssistantMessage, reason: StopReason) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    stream.push(AssistantMessageEvent::Done { reason, message });
    stream
}

fn error_stream(message: AssistantMessage, reason: StopReason) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    stream.push(AssistantMessageEvent::Error {
        reason,
        error: message,
    });
    stream
}

async fn collect(stream: &EventStream<AgentEvent, Vec<AgentMessage>>) -> Vec<AgentEvent> {
    let mut consumer = stream.consumer();
    let mut events = Vec::new();
    while let Some(event) = consumer.next().await {
        events.push(event);
    }
    events
}

fn event_types(events: &[AgentEvent]) -> Vec<&'static str> {
    events.iter().map(AgentEvent::r#type).collect()
}

fn tool_result_texts(messages: &[AgentMessage]) -> Vec<(String, String)> {
    messages
        .iter()
        .filter_map(|message| match message {
            AgentMessage::ToolResult(message) => Some((
                message.tool_call_id.clone(),
                xjpcode_ai::content_text(&message.content, ""),
            )),
            _ => None,
        })
        .collect()
}

fn tool_event_ids(events: &[AgentEvent], event_type: &str) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match (event_type, event) {
            ("tool_execution_start", AgentEvent::ToolExecutionStart { tool_call_id, .. })
            | ("tool_execution_end", AgentEvent::ToolExecutionEnd { tool_call_id, .. }) => {
                Some(tool_call_id.clone())
            }
            _ => None,
        })
        .collect()
}

fn message_end_tool_ids(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::MessageEnd {
                message: AgentMessage::ToolResult(message),
            } => Some(message.tool_call_id.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn uses_the_configured_default_when_a_legacy_caller_omits_stream_fn() {
    let calls = Arc::new(AtomicUsize::new(0));
    let default_calls = Arc::clone(&calls);
    set_default_stream_fn(Some(Arc::new(move |_model, _context, _options| {
        default_calls.fetch_add(1, Ordering::SeqCst);
        done_stream(text_message("fallback"), StopReason::Stop)
    })));

    let stream = agent_loop(
        vec![user_message("Hello")],
        empty_context(),
        config(),
        None,
        None,
    );
    let _ = stream.result().await;
    set_default_stream_fn(None);

    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn emits_events_with_agent_message_types() {
    let stream_fn: StreamFn = Arc::new(|_model, _context, _options| {
        done_stream(text_message("Hi there!"), StopReason::Stop)
    });
    let stream = agent_loop(
        vec![user_message("Hello")],
        empty_context(),
        config(),
        None,
        Some(stream_fn),
    );
    let events = collect(&stream).await;
    let messages = stream.result().await;

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role(), "user");
    assert_eq!(messages[1].role(), "assistant");

    let types = event_types(&events);
    for expected in [
        "agent_start",
        "turn_start",
        "message_start",
        "message_end",
        "turn_end",
        "agent_end",
    ] {
        assert!(types.contains(&expected), "missing {expected} in {types:?}");
    }
}

#[tokio::test]
async fn builds_provider_context_exclusively_from_transcript_messages() {
    let initial_system = AgentMessage::System(xjpcode_ai::SystemMessage {
        role: xjpcode_ai::SystemRole::System,
        content: xjpcode_ai::SystemContent::Text("Transcript prompt".to_string()),
        sections: None,
        tools_added: Some(Vec::new()),
        tools_removed: None,
        timestamp: 1,
    });
    let first_message_is_system = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&first_message_is_system);
    let stream_fn: StreamFn = Arc::new(move |_model, provider_context, _options| {
        observed.store(
            provider_context.messages.first().map(Message::role) == Some("system"),
            Ordering::SeqCst,
        );
        done_stream(text_message("done"), StopReason::Stop)
    });
    let stream = agent_loop(
        vec![initial_system.clone(), user_message("Hello")],
        empty_context(),
        config(),
        None,
        Some(stream_fn),
    );
    let _ = stream.result().await;
    // provider 收到的只有一份记录：没有顶层 prompt 或工具字段（`TranscriptContext` 只有一个字段）。
    assert!(first_message_is_system.load(Ordering::SeqCst));
}

#[tokio::test]
async fn handles_custom_message_types_via_convert_to_llm() {
    let notification = AgentMessage::Custom(xjpcode_agent::CustomAgentMessage {
        role: "notification".to_string(),
        payload: json!({ "text": "This is a notification" })
            .as_object()
            .expect("object")
            .clone(),
    });
    let converted = Arc::new(Mutex::new(Vec::<Message>::new()));
    let converted_in_hook = Arc::clone(&converted);
    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: Arc::new(move |messages: &[AgentMessage]| {
            let messages: Vec<Message> = messages
                .iter()
                .filter(|message| message.role() != "notification")
                .filter_map(|message| match message {
                    AgentMessage::User(message) => Some(Message::User(message.clone())),
                    AgentMessage::Assistant(message) => Some(Message::Assistant(message.clone())),
                    AgentMessage::ToolResult(message) => Some(Message::ToolResult(message.clone())),
                    _ => None,
                })
                .collect();
            *converted_in_hook.lock().expect("converted") = messages.clone();
            boxed(async move { messages })
        }),
        ..Default::default()
    };
    let context = AgentContext {
        messages: vec![notification],
        tools: None,
    };
    let stream_fn: StreamFn = Arc::new(|_model, _context, _options| {
        done_stream(text_message("Response"), StopReason::Stop)
    });
    let stream = agent_loop(
        vec![user_message("Hello")],
        context,
        config,
        None,
        Some(stream_fn),
    );
    let _ = collect(&stream).await;

    let converted = converted.lock().expect("converted").clone();
    assert_eq!(converted.len(), 1);
    assert_eq!(converted[0].role(), "user");
}

#[tokio::test]
async fn applies_transform_context_before_convert_to_llm() {
    let context = AgentContext {
        messages: vec![
            user_message("old message 1"),
            AgentMessage::Assistant(text_message("old response 1")),
            user_message("old message 2"),
            AgentMessage::Assistant(text_message("old response 2")),
        ],
        tools: None,
    };
    let transformed_len = Arc::new(Mutex::new(0_usize));
    let converted_len = Arc::new(Mutex::new(0_usize));
    let transformed_in_hook = Arc::clone(&transformed_len);
    let converted_in_hook = Arc::clone(&converted_len);
    let config = AgentLoopConfig {
        model: model(),
        transform_context: Some(transform_hook(move |messages, _signal| {
            let kept: Vec<AgentMessage> = messages[messages.len().saturating_sub(2)..].to_vec();
            *transformed_in_hook.lock().expect("transformed") = kept.len();
            boxed(async move { kept })
        })),
        convert_to_llm: Arc::new(move |messages: &[AgentMessage]| {
            let messages: Vec<Message> = messages
                .iter()
                .filter_map(|message| match message {
                    AgentMessage::User(message) => Some(Message::User(message.clone())),
                    AgentMessage::Assistant(message) => Some(Message::Assistant(message.clone())),
                    AgentMessage::ToolResult(message) => Some(Message::ToolResult(message.clone())),
                    _ => None,
                })
                .collect();
            *converted_in_hook.lock().expect("converted") = messages.len();
            boxed(async move { messages })
        }),
        ..Default::default()
    };
    let stream_fn: StreamFn = Arc::new(|_model, _context, _options| {
        done_stream(text_message("Response"), StopReason::Stop)
    });
    let stream = agent_loop(
        vec![user_message("new message")],
        context,
        config,
        None,
        Some(stream_fn),
    );
    let _ = collect(&stream).await;

    assert_eq!(*transformed_len.lock().expect("transformed"), 2);
    assert_eq!(*converted_len.lock().expect("converted"), 2);
}

#[tokio::test]
async fn handles_tool_calls_and_results() {
    let executed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_in_tool = Arc::clone(&executed);
    let tool_usage = xjpcode_ai::Usage {
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 4,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 10,
        cost: xjpcode_ai::UsageCost {
            input: 0.1,
            output: 0.2,
            cache_read: 0.3,
            cache_write: 0.4,
            total: 1.0,
            total_from_provider: None,
        },
    };
    let patched_usage = xjpcode_ai::Usage {
        input: 5,
        output: 6,
        cache_read: 7,
        cache_write: 8,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 26,
        cost: xjpcode_ai::UsageCost {
            input: 0.5,
            output: 0.6,
            cache_read: 0.7,
            cache_write: 0.8,
            total: 2.6,
            total_from_provider: None,
        },
    };
    let tool_usage_in_tool = tool_usage.clone();
    let tool = TestTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        Arc::new(move |_id, params, _signal, _update| {
            let value = value_of(params);
            let executed = Arc::clone(&executed_in_tool);
            let usage = tool_usage_in_tool.clone();
            boxed(async move {
                executed.lock().expect("executed").push(value.clone());
                Ok(xjpcode_agent::AgentToolResult {
                    content: vec![ToolResultContentBlock::Text(TextContent::new(format!(
                        "echoed: {value}"
                    )))],
                    details: Some(json!({ "value": value })),
                    structured_content: None,
                    usage: Some(usage),
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );
    let observed_usage: Arc<Mutex<Option<xjpcode_ai::Usage>>> = Arc::new(Mutex::new(None));
    let observed_in_hook = Arc::clone(&observed_usage);
    let patched_for_hook = patched_usage.clone();
    let after_tool_call = after_hook(move |context, _signal| {
        *observed_in_hook.lock().expect("observed") = context.result.usage.clone();
        let usage = patched_for_hook.clone();
        boxed(async move {
            Some(xjpcode_agent::AfterToolCallResult {
                usage: Some(usage),
                ..Default::default()
            })
        })
    });
    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        after_tool_call: Some(after_tool_call),
        ..Default::default()
    };
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(vec![Arc::new(tool)]),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&calls);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        let call = calls_in_stream.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            done_stream(
                assistant_message(
                    vec![tool_call_content(
                        "tool-1",
                        "echo",
                        json!({ "value": "hello" }),
                    )],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            )
        } else {
            done_stream(text_message("done"), StopReason::Stop)
        }
    });
    let stream = agent_loop(
        vec![user_message("echo something")],
        context,
        config,
        None,
        Some(stream_fn),
    );
    let events = collect(&stream).await;

    assert_eq!(
        *executed.lock().expect("executed"),
        vec!["hello".to_string()]
    );
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolExecutionStart { .. })));
    let tool_end = events
        .iter()
        .find(|event| matches!(event, AgentEvent::ToolExecutionEnd { .. }))
        .expect("tool_execution_end");
    match tool_end {
        AgentEvent::ToolExecutionEnd { is_error, .. } => assert!(!is_error),
        _ => unreachable!(),
    }
    assert_eq!(
        *observed_usage.lock().expect("observed"),
        Some(tool_usage.clone())
    );
    let messages = stream.result().await;
    let tool_result = messages
        .iter()
        .find_map(|message| match message {
            AgentMessage::ToolResult(message) => Some(message),
            _ => None,
        })
        .expect("tool result message");
    assert_eq!(tool_result.usage, Some(patched_usage));
}

#[tokio::test]
async fn records_how_long_execute_took_excluding_hooks() {
    let tool = TestTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        Arc::new(|_id, params, _signal, _update| {
            let value = value_of(params);
            boxed(async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                Ok(xjpcode_agent::AgentToolResult {
                    content: vec![ToolResultContentBlock::Text(TextContent::new(value))],
                    details: None,
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );
    let before_tool_call = before_hook(|context, _signal| {
        let blocked = context.tool_call.id == "blocked";
        boxed(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if blocked {
                Some(xjpcode_agent::BeforeToolCallResult {
                    block: Some(true),
                    reason: Some("no".to_string()),
                    terminate: None,
                })
            } else {
                None
            }
        })
    });
    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        before_tool_call: Some(before_tool_call),
        ..Default::default()
    };
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(vec![Arc::new(tool)]),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&calls);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        let call = calls_in_stream.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            done_stream(
                assistant_message(
                    vec![
                        tool_call_content("ran", "echo", json!({ "value": "a" })),
                        tool_call_content("blocked", "echo", json!({ "value": "b" })),
                    ],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            )
        } else {
            done_stream(text_message("done"), StopReason::Stop)
        }
    });
    let stream = agent_loop(
        vec![user_message("go")],
        context,
        config,
        None,
        Some(stream_fn),
    );
    let _ = collect(&stream).await;
    let results: Vec<ToolResultMessage> = stream
        .result()
        .await
        .into_iter()
        .filter_map(|message| match message {
            AgentMessage::ToolResult(message) => Some(message),
            _ => None,
        })
        .collect();

    let ran = results.first().expect("ran");
    let blocked = results.get(1).expect("blocked");
    let duration = ran.duration_ms.expect("duration");
    assert!(duration >= 25, "duration {duration}");
    assert!(duration < 100, "duration {duration}");
    assert!(blocked.is_error);
    assert_eq!(blocked.duration_ms, None);
}

#[tokio::test]
async fn does_not_execute_tool_calls_from_a_length_truncated_assistant_message() {
    let executed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_in_tool = Arc::clone(&executed);
    let tool = TestTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        Arc::new(move |_id, params, _signal, _update| {
            let value = value_of(params);
            let executed = Arc::clone(&executed_in_tool);
            boxed(async move {
                executed.lock().expect("executed").push(value);
                Ok(xjpcode_agent::AgentToolResult::text("echoed"))
            })
        }),
    );
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(vec![Arc::new(tool)]),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&calls);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        let call = calls_in_stream.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            done_stream(
                assistant_message(
                    vec![tool_call_content(
                        "tool-1",
                        "echo",
                        json!({ "value": "hel" }),
                    )],
                    StopReason::Length,
                ),
                StopReason::Length,
            )
        } else {
            done_stream(text_message("done"), StopReason::Stop)
        }
    });
    let stream = agent_loop(
        vec![user_message("echo something")],
        context,
        config(),
        None,
        Some(stream_fn),
    );
    let events = collect(&stream).await;

    assert!(executed.lock().expect("executed").is_empty());
    let tool_end = events
        .iter()
        .find(|event| matches!(event, AgentEvent::ToolExecutionEnd { .. }))
        .expect("tool_execution_end");
    match tool_end {
        AgentEvent::ToolExecutionEnd {
            is_error, result, ..
        } => {
            assert!(is_error);
            assert!(result.text_content().contains("output token limit"));
        }
        _ => unreachable!(),
    }
    // 循环接着跑，好让模型重新发一次。
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let messages = stream.result().await;
    assert_eq!(messages.last().map(AgentMessage::role), Some("assistant"));
}

#[tokio::test]
async fn executes_mutated_before_tool_call_args_without_revalidation() {
    let executed: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_in_tool = Arc::clone(&executed);
    let tool = TestTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        Arc::new(move |_id, params, _signal, _update| {
            let value = params.get("value").cloned().unwrap_or(Value::Null);
            let executed = Arc::clone(&executed_in_tool);
            boxed(async move {
                executed.lock().expect("executed").push(value.clone());
                Ok(xjpcode_agent::AgentToolResult::text(format!(
                    "echoed: {}",
                    tools::js_text(&value)
                )))
            })
        }),
    );
    let before_tool_call = before_hook(|context, _signal| {
        if let Value::Object(args) = context.args {
            args.insert("value".to_string(), json!(123));
        }
        boxed(async { None })
    });
    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        before_tool_call: Some(before_tool_call),
        ..Default::default()
    };
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(vec![Arc::new(tool)]),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&calls);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        let call = calls_in_stream.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            done_stream(
                assistant_message(
                    vec![tool_call_content(
                        "tool-1",
                        "echo",
                        json!({ "value": "hello" }),
                    )],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            )
        } else {
            done_stream(text_message("done"), StopReason::Stop)
        }
    });
    let stream = agent_loop(
        vec![user_message("echo something")],
        context,
        config,
        None,
        Some(stream_fn),
    );
    let _ = collect(&stream).await;

    assert_eq!(*executed.lock().expect("executed"), vec![json!(123)]);
}

#[tokio::test]
async fn prepares_tool_arguments_for_validation() {
    let executed: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_in_tool = Arc::clone(&executed);
    let mut tool = TestTool::new(
        "edit",
        "Edit",
        "Edit tool",
        json!({
            "type": "object",
            "properties": {
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": { "type": "string" },
                            "newText": { "type": "string" }
                        },
                        "required": ["oldText", "newText"]
                    }
                }
            },
            "required": ["edits"]
        }),
        Arc::new(move |_id, params, _signal, _update| {
            let edits = params.get("edits").cloned().unwrap_or(Value::Null);
            let executed = Arc::clone(&executed_in_tool);
            boxed(async move {
                executed.lock().expect("executed").push(edits);
                Ok(xjpcode_agent::AgentToolResult::text("edited"))
            })
        }),
    );
    tool.prepare = Some(Arc::new(|args: &JsonObject| {
        let (Some(old_text), Some(new_text)) = (
            args.get("oldText").and_then(Value::as_str),
            args.get("newText").and_then(Value::as_str),
        ) else {
            return Ok(args.clone());
        };
        let mut edits: Vec<Value> = args
            .get("edits")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        edits.push(json!({ "oldText": old_text, "newText": new_text }));
        let mut next = args.clone();
        next.insert("edits".to_string(), Value::Array(edits));
        Ok(next)
    }));
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(vec![Arc::new(tool)]),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&calls);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        let call = calls_in_stream.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            done_stream(
                assistant_message(
                    vec![tool_call_content(
                        "tool-1",
                        "edit",
                        json!({ "oldText": "before", "newText": "after" }),
                    )],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            )
        } else {
            done_stream(text_message("done"), StopReason::Stop)
        }
    });
    let stream = agent_loop(
        vec![user_message("edit something")],
        context,
        config(),
        None,
        Some(stream_fn),
    );
    let _ = collect(&stream).await;

    assert_eq!(
        *executed.lock().expect("executed"),
        vec![json!([{ "oldText": "before", "newText": "after" }])]
    );
}

#[tokio::test]
async fn emits_tool_execution_end_in_completion_order_but_persists_results_in_source_order() {
    let release_first = Arc::new(tokio::sync::Notify::new());
    let first_resolved = Arc::new(AtomicBool::new(false));
    let parallel_observed = Arc::new(AtomicBool::new(false));
    let release_for_tool = Arc::clone(&release_first);
    let first_resolved_in_tool = Arc::clone(&first_resolved);
    let parallel_in_tool = Arc::clone(&parallel_observed);
    let tool = TestTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        Arc::new(move |_id, params, _signal, _update| {
            let value = value_of(params);
            let release = Arc::clone(&release_for_tool);
            let first_resolved = Arc::clone(&first_resolved_in_tool);
            let parallel_observed = Arc::clone(&parallel_in_tool);
            boxed(async move {
                if value == "first" {
                    release.notified().await;
                    first_resolved.store(true, Ordering::SeqCst);
                }
                if value == "second" && !first_resolved.load(Ordering::SeqCst) {
                    parallel_observed.store(true, Ordering::SeqCst);
                }
                Ok(xjpcode_agent::AgentToolResult::text(format!(
                    "echoed: {value}"
                )))
            })
        }),
    );
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(vec![Arc::new(tool)]),
    };
    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        tool_execution: ToolExecutionMode::Parallel,
        ..Default::default()
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&calls);
    let release_in_stream = Arc::clone(&release_first);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        let call = calls_in_stream.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            let stream = done_stream(
                assistant_message(
                    vec![
                        tool_call_content("tool-1", "echo", json!({ "value": "first" })),
                        tool_call_content("tool-2", "echo", json!({ "value": "second" })),
                    ],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            );
            let release = Arc::clone(&release_in_stream);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                release.notify_waiters();
            });
            stream
        } else {
            done_stream(text_message("done"), StopReason::Stop)
        }
    });
    let stream = agent_loop(
        vec![user_message("echo both")],
        context,
        config,
        None,
        Some(stream_fn),
    );
    let events = collect(&stream).await;

    assert!(parallel_observed.load(Ordering::SeqCst));
    assert_eq!(
        tool_event_ids(&events, "tool_execution_end"),
        vec!["tool-2".to_string(), "tool-1".to_string()]
    );
    assert_eq!(
        message_end_tool_ids(&events),
        vec!["tool-1".to_string(), "tool-2".to_string()]
    );
    let turn_tool_result_ids: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::TurnEnd { tool_results, .. } => Some(
                tool_results
                    .iter()
                    .map(|result| result.tool_call_id.clone())
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        turn_tool_result_ids,
        vec!["tool-1".to_string(), "tool-2".to_string()]
    );
}

#[tokio::test]
async fn injects_queued_messages_after_all_tool_calls_complete() {
    let executed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_in_tool = Arc::clone(&executed);
    let executed_for_poll = Arc::clone(&executed);
    let tool = TestTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        Arc::new(move |_id, params, _signal, _update| {
            let value = value_of(params);
            let executed = Arc::clone(&executed_in_tool);
            boxed(async move {
                executed.lock().expect("executed").push(value.clone());
                Ok(xjpcode_agent::AgentToolResult::text(format!("ok:{value}")))
            })
        }),
    );
    let queued_delivered = Arc::new(AtomicBool::new(false));
    let queued_for_poll = Arc::clone(&queued_delivered);
    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        tool_execution: ToolExecutionMode::Sequential,
        get_steering_messages: Some(Arc::new(move || {
            let executed = Arc::clone(&executed_for_poll);
            let queued_delivered = Arc::clone(&queued_for_poll);
            boxed(async move {
                if !executed.lock().expect("executed").is_empty()
                    && !queued_delivered.swap(true, Ordering::SeqCst)
                {
                    return vec![user_message("interrupt")];
                }
                Vec::new()
            })
        })),
        ..Default::default()
    };
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(vec![Arc::new(tool)]),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&calls);
    let saw_interrupt = Arc::new(AtomicBool::new(false));
    let saw_interrupt_in_stream = Arc::clone(&saw_interrupt);
    let stream_fn: StreamFn = Arc::new(move |_model, provider_context, _options| {
        let call = calls_in_stream.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            done_stream(
                assistant_message(
                    vec![
                        tool_call_content("tool-1", "echo", json!({ "value": "first" })),
                        tool_call_content("tool-2", "echo", json!({ "value": "second" })),
                    ],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            )
        } else {
            if call == 1 {
                saw_interrupt_in_stream.store(
                    provider_context
                        .messages
                        .iter()
                        .any(|message| user_message_text(message).as_deref() == Some("interrupt")),
                    Ordering::SeqCst,
                );
            }
            done_stream(text_message("done"), StopReason::Stop)
        }
    });
    let stream = agent_loop(
        vec![user_message("start")],
        context,
        config,
        None,
        Some(stream_fn),
    );
    let events = collect(&stream).await;

    assert_eq!(
        *executed.lock().expect("executed"),
        vec!["first".to_string(), "second".to_string()]
    );
    let tool_ends = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ToolExecutionEnd { .. }))
        .count();
    assert_eq!(tool_ends, 2);

    let event_sequence: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::MessageStart { message } => match message {
                AgentMessage::ToolResult(message) => Some(format!("tool:{}", message.tool_call_id)),
                AgentMessage::User(message) => match &message.content {
                    UserContent::Text(text) => Some(text.clone()),
                    UserContent::Blocks(blocks) => Some(xjpcode_ai::content_text(blocks, "")),
                },
                _ => None,
            },
            _ => None,
        })
        .collect();
    let interrupt_index = event_sequence
        .iter()
        .position(|entry| entry == "interrupt")
        .expect("interrupt message");
    assert!(
        event_sequence
            .iter()
            .position(|entry| entry == "tool:tool-1")
            .expect("tool-1")
            < interrupt_index
    );
    assert!(
        event_sequence
            .iter()
            .position(|entry| entry == "tool:tool-2")
            .expect("tool-2")
            < interrupt_index
    );
    assert!(saw_interrupt.load(Ordering::SeqCst));
}

#[tokio::test]
async fn force_sequential_when_a_tool_has_execution_mode_sequential() {
    let release_first = Arc::new(tokio::sync::Notify::new());
    let first_resolved = Arc::new(AtomicBool::new(false));
    let parallel_observed = Arc::new(AtomicBool::new(false));
    let release_for_tool = Arc::clone(&release_first);
    let first_resolved_in_tool = Arc::clone(&first_resolved);
    let parallel_in_tool = Arc::clone(&parallel_observed);
    let mut tool = TestTool::new(
        "slow",
        "Slow",
        "Slow tool",
        value_schema(),
        Arc::new(move |_id, params, _signal, _update| {
            let value = value_of(params);
            let release = Arc::clone(&release_for_tool);
            let first_resolved = Arc::clone(&first_resolved_in_tool);
            let parallel_observed = Arc::clone(&parallel_in_tool);
            boxed(async move {
                if value == "first" {
                    release.notified().await;
                    first_resolved.store(true, Ordering::SeqCst);
                }
                if value == "second" && !first_resolved.load(Ordering::SeqCst) {
                    parallel_observed.store(true, Ordering::SeqCst);
                }
                Ok(xjpcode_agent::AgentToolResult::text(format!(
                    "slow: {value}"
                )))
            })
        }),
    );
    tool.execution_mode = Some(ToolExecutionMode::Sequential);
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(vec![Arc::new(tool)]),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&calls);
    let release_in_stream = Arc::clone(&release_first);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        let call = calls_in_stream.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            let stream = done_stream(
                assistant_message(
                    vec![
                        tool_call_content("tool-1", "slow", json!({ "value": "first" })),
                        tool_call_content("tool-2", "slow", json!({ "value": "second" })),
                    ],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            );
            let release = Arc::clone(&release_in_stream);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                release.notify_waiters();
            });
            stream
        } else {
            done_stream(text_message("done"), StopReason::Stop)
        }
    });
    let stream = agent_loop(
        vec![user_message("run both")],
        context,
        config(),
        None,
        Some(stream_fn),
    );
    let events = collect(&stream).await;

    assert!(!parallel_observed.load(Ordering::SeqCst));
    assert_eq!(
        message_end_tool_ids(&events),
        vec!["tool-1".to_string(), "tool-2".to_string()]
    );
}

#[tokio::test]
async fn answers_every_tool_call_of_a_sequential_batch_aborted_mid_batch() {
    let controller = AbortController::new();
    let executed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_in_tool = Arc::clone(&executed);
    let controller_in_tool = controller.clone();
    let mut tool = TestTool::new(
        "slow",
        "Slow",
        "Slow tool",
        value_schema(),
        Arc::new(move |_id, params, _signal, _update| {
            let value = value_of(params);
            let executed = Arc::clone(&executed_in_tool);
            let controller = controller_in_tool.clone();
            boxed(async move {
                executed.lock().expect("executed").push(value);
                controller.abort();
                Err(ToolError::from("interrupted"))
            })
        }),
    );
    tool.execution_mode = Some(ToolExecutionMode::Sequential);
    let (events, messages) =
        run_three_call_batch(Arc::new(tool), config(), controller.signal()).await;

    assert_eq!(
        *executed.lock().expect("executed"),
        vec!["first".to_string()]
    );
    assert_eq!(
        tool_result_texts(&messages),
        vec![
            ("tool-1".to_string(), "interrupted".to_string()),
            ("tool-2".to_string(), "Operation aborted".to_string()),
            ("tool-3".to_string(), "Operation aborted".to_string()),
        ]
    );
    assert_eq!(
        tool_event_ids(&events, "tool_execution_start"),
        vec![
            "tool-1".to_string(),
            "tool-2".to_string(),
            "tool-3".to_string()
        ]
    );
    assert_eq!(
        tool_event_ids(&events, "tool_execution_end"),
        vec![
            "tool-1".to_string(),
            "tool-2".to_string(),
            "tool-3".to_string()
        ]
    );
}

#[tokio::test]
async fn answers_every_tool_call_of_a_parallel_batch_aborted_while_calls_are_prepared() {
    let controller = AbortController::new();
    let executed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_in_tool = Arc::clone(&executed);
    let tool = TestTool::new(
        "slow",
        "Slow",
        "Slow tool",
        value_schema(),
        Arc::new(move |_id, params, _signal, _update| {
            let value = value_of(params);
            let executed = Arc::clone(&executed_in_tool);
            boxed(async move {
                executed.lock().expect("executed").push(value.clone());
                Ok(xjpcode_agent::AgentToolResult::text(value))
            })
        }),
    );
    let hook_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let hook_calls_in_hook = Arc::clone(&hook_calls);
    let controller_in_hook = controller.clone();
    let before_tool_call = before_hook(move |context, _signal| {
        let id = context.tool_call.id.clone();
        let hook_calls = Arc::clone(&hook_calls_in_hook);
        let controller = controller_in_hook.clone();
        boxed(async move {
            hook_calls.lock().expect("hook calls").push(id.clone());
            if id == "tool-2" {
                controller.abort();
            }
            None
        })
    });
    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        before_tool_call: Some(before_tool_call),
        ..Default::default()
    };
    let (events, messages) =
        run_three_call_batch(Arc::new(tool), config, controller.signal()).await;

    assert!(executed.lock().expect("executed").is_empty());
    assert_eq!(
        *hook_calls.lock().expect("hook calls"),
        vec!["tool-1".to_string(), "tool-2".to_string()]
    );
    assert_eq!(
        tool_result_texts(&messages),
        vec![
            ("tool-1".to_string(), "Operation aborted".to_string()),
            ("tool-2".to_string(), "Operation aborted".to_string()),
            ("tool-3".to_string(), "Operation aborted".to_string()),
        ]
    );
    assert_eq!(
        tool_event_ids(&events, "tool_execution_start"),
        vec![
            "tool-1".to_string(),
            "tool-2".to_string(),
            "tool-3".to_string()
        ]
    );
    let mut end_ids = tool_event_ids(&events, "tool_execution_end");
    end_ids.sort();
    assert_eq!(
        end_ids,
        vec![
            "tool-1".to_string(),
            "tool-2".to_string(),
            "tool-3".to_string()
        ]
    );
}

/// 一次调三个工具，第二轮直接 aborted；上游 `runToolBatch` 的 Rust 版。
async fn run_three_call_batch(
    tool: Arc<TestTool>,
    config: AgentLoopConfig,
    signal: xjpcode_ai::AbortSignal,
) -> (Vec<AgentEvent>, Vec<AgentMessage>) {
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(vec![tool.clone()]),
    };
    let tool_name = tool.name.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&calls);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        let call = calls_in_stream.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            done_stream(
                assistant_message(
                    vec![
                        tool_call_content("tool-1", &tool_name, json!({ "value": "first" })),
                        tool_call_content("tool-2", &tool_name, json!({ "value": "second" })),
                        tool_call_content("tool-3", &tool_name, json!({ "value": "third" })),
                    ],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            )
        } else {
            let mut message = assistant_message(Vec::new(), StopReason::Aborted);
            message.error_message = Some("aborted".to_string());
            error_stream(message, StopReason::Aborted)
        }
    });
    let stream = agent_loop(
        vec![user_message("run three")],
        context,
        config,
        Some(signal),
        Some(stream_fn),
    );
    let events = collect(&stream).await;
    let messages = stream.result().await;
    (events, messages)
}

#[tokio::test]
async fn force_sequential_when_one_of_multiple_tools_is_sequential() {
    let execution_order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let release_slow = Arc::new(tokio::sync::Notify::new());

    let order_for_slow = Arc::clone(&execution_order);
    let release_for_slow = Arc::clone(&release_slow);
    let mut slow_tool = TestTool::new(
        "slow",
        "Slow",
        "Slow tool",
        value_schema(),
        Arc::new(move |_id, params, _signal, _update| {
            let value = value_of(params);
            let execution_order = Arc::clone(&order_for_slow);
            let release = Arc::clone(&release_for_slow);
            boxed(async move {
                execution_order
                    .lock()
                    .expect("execution order")
                    .push(format!("slow:{value}"));
                if value == "a" {
                    release.notified().await;
                }
                Ok(xjpcode_agent::AgentToolResult::text(format!(
                    "slow: {value}"
                )))
            })
        }),
    );
    slow_tool.execution_mode = Some(ToolExecutionMode::Sequential);

    let order_for_fast = Arc::clone(&execution_order);
    let fast_tool = TestTool::new(
        "fast",
        "Fast",
        "Fast tool",
        value_schema(),
        Arc::new(move |_id, params, _signal, _update| {
            let value = value_of(params);
            let execution_order = Arc::clone(&order_for_fast);
            boxed(async move {
                execution_order
                    .lock()
                    .expect("execution order")
                    .push(format!("fast:{value}"));
                Ok(xjpcode_agent::AgentToolResult::text(format!(
                    "fast: {value}"
                )))
            })
        }),
    );

    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(vec![Arc::new(slow_tool), Arc::new(fast_tool)]),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&calls);
    let release_in_stream = Arc::clone(&release_slow);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        let call = calls_in_stream.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            let stream = done_stream(
                assistant_message(
                    vec![
                        tool_call_content("tool-1", "slow", json!({ "value": "a" })),
                        tool_call_content("tool-2", "fast", json!({ "value": "b" })),
                    ],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            );
            let release = Arc::clone(&release_in_stream);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                release.notify_waiters();
            });
            stream
        } else {
            done_stream(text_message("done"), StopReason::Stop)
        }
    });
    let stream = agent_loop(
        vec![user_message("run both")],
        context,
        config(),
        None,
        Some(stream_fn),
    );
    let _ = collect(&stream).await;

    let order = execution_order.lock().expect("execution order").clone();
    assert_eq!(order.first().map(String::as_str), Some("slow:a"));
    assert!(order.contains(&"fast:b".to_string()));
}

#[tokio::test]
async fn allows_parallel_execution_when_all_tools_are_parallel() {
    let release_first = Arc::new(tokio::sync::Notify::new());
    let first_resolved = Arc::new(AtomicBool::new(false));
    let parallel_observed = Arc::new(AtomicBool::new(false));
    let release_for_tool = Arc::clone(&release_first);
    let first_resolved_in_tool = Arc::clone(&first_resolved);
    let parallel_in_tool = Arc::clone(&parallel_observed);
    let mut tool = TestTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        Arc::new(move |_id, params, _signal, _update| {
            let value = value_of(params);
            let release = Arc::clone(&release_for_tool);
            let first_resolved = Arc::clone(&first_resolved_in_tool);
            let parallel_observed = Arc::clone(&parallel_in_tool);
            boxed(async move {
                if value == "first" {
                    release.notified().await;
                    first_resolved.store(true, Ordering::SeqCst);
                }
                if value == "second" && !first_resolved.load(Ordering::SeqCst) {
                    parallel_observed.store(true, Ordering::SeqCst);
                }
                Ok(xjpcode_agent::AgentToolResult::text(format!(
                    "echoed: {value}"
                )))
            })
        }),
    );
    tool.execution_mode = Some(ToolExecutionMode::Parallel);
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(vec![Arc::new(tool)]),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&calls);
    let release_in_stream = Arc::clone(&release_first);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        let call = calls_in_stream.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            let stream = done_stream(
                assistant_message(
                    vec![
                        tool_call_content("tool-1", "echo", json!({ "value": "first" })),
                        tool_call_content("tool-2", "echo", json!({ "value": "second" })),
                    ],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            );
            let release = Arc::clone(&release_in_stream);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                release.notify_waiters();
            });
            stream
        } else {
            done_stream(text_message("done"), StopReason::Stop)
        }
    });
    let stream = agent_loop(
        vec![user_message("echo both")],
        context,
        config(),
        None,
        Some(stream_fn),
    );
    let _ = collect(&stream).await;

    assert!(parallel_observed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn runs_finish_turn_after_tool_result_messages_and_before_turn_end() {
    let tool = TestTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        Arc::new(|_id, params, _signal, _update| {
            let value = value_of(params);
            boxed(async move {
                let mut result = xjpcode_agent::AgentToolResult::text(value);
                result.terminate = Some(true);
                Ok(result)
            })
        }),
    );
    let ordering: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let ordering_in_finish = Arc::clone(&ordering);
    let finish_turn = finish_hook(move |context, _signal| {
        let mut ordering = ordering_in_finish.lock().expect("ordering");
        ordering.push("finishTurn".to_string());
        assert_eq!(context.tool_results.len(), 1);
        assert_eq!(
            context.context.messages.last().map(AgentMessage::role),
            Some("toolResult")
        );
        drop(ordering);
        boxed(async { None })
    });
    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        finish_turn: Some(finish_turn),
        ..Default::default()
    };
    let ordering_for_emit = Arc::clone(&ordering);
    let emit = Arc::new(move |event: AgentEvent| {
        if let AgentEvent::MessageEnd { message } = &event {
            ordering_for_emit
                .lock()
                .expect("ordering")
                .push(format!("message_end:{}", message.role()));
        }
        if matches!(event, AgentEvent::TurnEnd { .. }) {
            ordering_for_emit
                .lock()
                .expect("ordering")
                .push("turn_end".to_string());
        }
        boxed(async {})
    });
    let stream_fn: StreamFn = Arc::new(|_model, _context, _options| {
        done_stream(
            assistant_message(
                vec![tool_call_content(
                    "tool-1",
                    "echo",
                    json!({ "value": "hello" }),
                )],
                StopReason::ToolUse,
            ),
            StopReason::ToolUse,
        )
    });
    run_agent_loop(
        vec![user_message("echo")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![Arc::new(tool)]),
        },
        config,
        emit,
        None,
        Some(stream_fn),
    )
    .await
    .expect("run");

    let ordering = ordering.lock().expect("ordering").clone();
    assert_eq!(
        ordering[ordering.len() - 3..].to_vec(),
        vec![
            "message_end:toolResult".to_string(),
            "finishTurn".to_string(),
            "turn_end".to_string()
        ]
    );
}

#[tokio::test]
async fn runs_finish_turn_for_an_error_or_aborted_assistant_without_changing_the_hard_exit() {
    for reason in [StopReason::Error, StopReason::Aborted] {
        let ordering: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let steering_polls = Arc::new(AtomicUsize::new(0));
        let follow_up_polls = Arc::new(AtomicUsize::new(0));

        let ordering_in_finish = Arc::clone(&ordering);
        let finish_turn = finish_hook(move |context, _signal| {
            assert_eq!(context.message.stop_reason, reason);
            ordering_in_finish
                .lock()
                .expect("ordering")
                .push("finishTurn".to_string());
            boxed(async { Some(xjpcode_agent::AgentTurnDecision::Continue) })
        });
        let config = AgentLoopConfig {
            model: model(),
            convert_to_llm: identity_converter(),
            finish_turn: Some(finish_turn),
            get_steering_messages: {
                let steering_polls = Arc::clone(&steering_polls);
                Some(Arc::new(move || {
                    steering_polls.fetch_add(1, Ordering::SeqCst);
                    boxed(async { Vec::new() })
                }))
            },
            get_follow_up_messages: {
                let follow_up_polls = Arc::clone(&follow_up_polls);
                Some(Arc::new(move || {
                    follow_up_polls.fetch_add(1, Ordering::SeqCst);
                    boxed(async { vec![user_message("queued")] })
                }))
            },
            ..Default::default()
        };
        let ordering_for_emit = Arc::clone(&ordering);
        let emit = Arc::new(move |event: AgentEvent| {
            if matches!(event, AgentEvent::TurnEnd { .. }) {
                ordering_for_emit
                    .lock()
                    .expect("ordering")
                    .push("turn_end".to_string());
            }
            boxed(async {})
        });
        let provider_calls_in_stream = Arc::clone(&provider_calls);
        let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
            provider_calls_in_stream.fetch_add(1, Ordering::SeqCst);
            let mut message = assistant_message(Vec::new(), reason);
            message.error_message = Some(format!("{reason:?}").to_lowercase());
            error_stream(message, reason)
        });
        run_agent_loop(
            vec![user_message("run")],
            empty_context(),
            config,
            emit,
            None,
            Some(stream_fn),
        )
        .await
        .expect("run");

        assert_eq!(
            *ordering.lock().expect("ordering"),
            vec!["finishTurn".to_string(), "turn_end".to_string()]
        );
        assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
        assert_eq!(steering_polls.load(Ordering::SeqCst), 1);
        assert_eq!(follow_up_polls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn action_end_skips_queue_polling_and_next_turn_preparation() {
    let tool = TestTool::new(
        "noop",
        "Noop",
        "Noop tool",
        json!({ "type": "object", "properties": {} }),
        Arc::new(|_id, _params, _signal, _update| {
            boxed(async { Ok(xjpcode_agent::AgentToolResult::text("done")) })
        }),
    );
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let steering_polls = Arc::new(AtomicUsize::new(0));
    let follow_up_polls = Arc::new(AtomicUsize::new(0));
    let prepare_next_turn_calls = Arc::new(AtomicUsize::new(0));

    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        finish_turn: Some(finish_hook(|_context, _signal| {
            boxed(async { Some(xjpcode_agent::AgentTurnDecision::End) })
        })),
        prepare_next_turn: {
            let prepare_next_turn_calls = Arc::clone(&prepare_next_turn_calls);
            Some(prepare_next_turn_hook(move |_context| {
                prepare_next_turn_calls.fetch_add(1, Ordering::SeqCst);
                boxed(async { None })
            }))
        },
        get_steering_messages: {
            let steering_polls = Arc::clone(&steering_polls);
            Some(Arc::new(move || {
                steering_polls.fetch_add(1, Ordering::SeqCst);
                boxed(async { Vec::new() })
            }))
        },
        get_follow_up_messages: {
            let follow_up_polls = Arc::clone(&follow_up_polls);
            Some(Arc::new(move || {
                follow_up_polls.fetch_add(1, Ordering::SeqCst);
                boxed(async { vec![user_message("queued")] })
            }))
        },
        ..Default::default()
    };
    let calls_in_stream = Arc::clone(&provider_calls);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        calls_in_stream.fetch_add(1, Ordering::SeqCst);
        done_stream(
            assistant_message(
                vec![tool_call_content("tool-1", "noop", json!({}))],
                StopReason::ToolUse,
            ),
            StopReason::ToolUse,
        )
    });
    let stream = agent_loop(
        vec![user_message("run")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![Arc::new(tool)]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = stream.result().await;

    assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
    assert_eq!(steering_polls.load(Ordering::SeqCst), 1);
    assert_eq!(follow_up_polls.load(Ordering::SeqCst), 0);
    assert_eq!(prepare_next_turn_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn makes_exactly_one_context_only_request_when_no_natural_request_satisfies_continuation() {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let finish_calls = Arc::new(AtomicUsize::new(0));
    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        finish_turn: {
            let finish_calls = Arc::clone(&finish_calls);
            Some(finish_hook(move |_context, _signal| {
                let call = finish_calls.fetch_add(1, Ordering::SeqCst);
                boxed(async move {
                    if call == 0 {
                        Some(xjpcode_agent::AgentTurnDecision::Continue)
                    } else {
                        None
                    }
                })
            }))
        },
        ..Default::default()
    };
    let calls_in_stream = Arc::clone(&provider_calls);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        let call = calls_in_stream.fetch_add(1, Ordering::SeqCst) + 1;
        done_stream(text_message(&format!("response {call}")), StopReason::Stop)
    });
    let stream = agent_loop(
        vec![user_message("run")],
        empty_context(),
        config,
        None,
        Some(stream_fn),
    );
    let _ = stream.result().await;

    assert_eq!(provider_calls.load(Ordering::SeqCst), 2);
    assert_eq!(finish_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn lets_a_natural_tool_result_request_satisfy_continuation() {
    let tool = TestTool::new(
        "noop",
        "Noop",
        "Noop tool",
        json!({ "type": "object", "properties": {} }),
        Arc::new(|_id, _params, _signal, _update| {
            boxed(async { Ok(xjpcode_agent::AgentToolResult::text("done")) })
        }),
    );
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let finish_calls = Arc::new(AtomicUsize::new(0));
    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        finish_turn: {
            let finish_calls = Arc::clone(&finish_calls);
            Some(finish_hook(move |_context, _signal| {
                let call = finish_calls.fetch_add(1, Ordering::SeqCst);
                boxed(async move {
                    if call == 0 {
                        Some(xjpcode_agent::AgentTurnDecision::Continue)
                    } else {
                        None
                    }
                })
            }))
        },
        ..Default::default()
    };
    let calls_in_stream = Arc::clone(&provider_calls);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        let call = calls_in_stream.fetch_add(1, Ordering::SeqCst) + 1;
        if call == 1 {
            done_stream(
                assistant_message(
                    vec![tool_call_content("tool-1", "noop", json!({}))],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            )
        } else {
            done_stream(text_message("done"), StopReason::Stop)
        }
    });
    let stream = agent_loop(
        vec![user_message("run")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![Arc::new(tool)]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = stream.result().await;

    assert_eq!(provider_calls.load(Ordering::SeqCst), 2);
    assert_eq!(finish_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn lets_a_natural_steering_or_follow_up_request_satisfy_continuation() {
    for queue_kind in ["steering", "follow-up"] {
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let finish_calls = Arc::new(AtomicUsize::new(0));
        let steering_polls = Arc::new(AtomicUsize::new(0));
        let follow_up_delivered = Arc::new(AtomicBool::new(false));
        let second_request_users: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let config = AgentLoopConfig {
            model: model(),
            convert_to_llm: identity_converter(),
            finish_turn: {
                let finish_calls = Arc::clone(&finish_calls);
                Some(finish_hook(move |_context, _signal| {
                    let call = finish_calls.fetch_add(1, Ordering::SeqCst);
                    boxed(async move {
                        if call == 0 {
                            Some(xjpcode_agent::AgentTurnDecision::Continue)
                        } else {
                            None
                        }
                    })
                }))
            },
            get_steering_messages: {
                let steering_polls = Arc::clone(&steering_polls);
                Some(Arc::new(move || {
                    let polls = steering_polls.fetch_add(1, Ordering::SeqCst) + 1;
                    let deliver = queue_kind == "steering" && polls == 2;
                    boxed(async move {
                        if deliver {
                            vec![user_message(queue_kind)]
                        } else {
                            Vec::new()
                        }
                    })
                }))
            },
            get_follow_up_messages: {
                let follow_up_delivered = Arc::clone(&follow_up_delivered);
                Some(Arc::new(move || {
                    let deliver = queue_kind == "follow-up"
                        && !follow_up_delivered.swap(true, Ordering::SeqCst);
                    boxed(async move {
                        if deliver {
                            vec![user_message(queue_kind)]
                        } else {
                            Vec::new()
                        }
                    })
                }))
            },
            ..Default::default()
        };
        let calls_in_stream = Arc::clone(&provider_calls);
        let second_users_in_stream = Arc::clone(&second_request_users);
        let stream_fn: StreamFn =
            Arc::new(move |_model, provider_context, _options| {
                let call = calls_in_stream.fetch_add(1, Ordering::SeqCst) + 1;
                if call == 2 {
                    second_users_in_stream
                        .lock()
                        .expect("second request users")
                        .extend(provider_context.messages.iter().filter_map(
                            |message| match message {
                                Message::User(message) => match &message.content {
                                    UserContent::Text(text) => Some(text.clone()),
                                    UserContent::Blocks(blocks) => {
                                        Some(xjpcode_ai::content_text(blocks, ""))
                                    }
                                },
                                _ => None,
                            },
                        ));
                }
                done_stream(text_message("done"), StopReason::Stop)
            });
        let stream = agent_loop(
            vec![user_message("run")],
            empty_context(),
            config,
            None,
            Some(stream_fn),
        );
        let _ = stream.result().await;

        assert_eq!(provider_calls.load(Ordering::SeqCst), 2);
        assert_eq!(finish_calls.load(Ordering::SeqCst), 2);
        assert!(
            second_request_users
                .lock()
                .expect("second request users")
                .iter()
                .any(|text| text == queue_kind),
            "{queue_kind} missing from the second request"
        );
    }
}

#[tokio::test]
async fn prepares_the_initial_request_after_pending_messages_and_can_replace_request_state() {
    let replacement_model = Model {
        id: "replacement".to_string(),
        name: "replacement".to_string(),
        ..model()
    };
    let steering_message = user_message("steering");
    let completed_messages: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let steering_delivered = Arc::new(AtomicBool::new(false));
    let prepare_calls = Arc::new(AtomicUsize::new(0));
    let steering_for_hook = steering_message.clone();
    let completed_for_hook = Arc::clone(&completed_messages);
    let prepare_calls_in_hook = Arc::clone(&prepare_calls);
    let model_in_hook = replacement_model.clone();

    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        get_steering_messages: {
            let steering_delivered = Arc::clone(&steering_delivered);
            Some(Arc::new(move || {
                let deliver = !steering_delivered.swap(true, Ordering::SeqCst);
                let steering = steering_message.clone();
                boxed(async move {
                    if deliver {
                        vec![steering.clone()]
                    } else {
                        Vec::new()
                    }
                })
            }))
        },
        prepare_request: Some(prepare_request_hook(move |request, _signal| {
            prepare_calls_in_hook.fetch_add(1, Ordering::SeqCst);
            let completed = completed_for_hook.lock().expect("completed");
            assert_eq!(completed.last().map(String::as_str), Some("user"));
            drop(completed);
            assert!(request.context.messages.iter().any(|message| {
                matches!(message, AgentMessage::User(message) if message.content == UserContent::Text("steering".to_string()))
            }));
            let canonical = AgentMessage::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("canonical projection".to_string()),
                timestamp: 0,
            });
            let model = model_in_hook.clone();
            boxed(async move {
                Some(xjpcode_agent::AgentRequestUpdate {
                    context: Some(AgentContext {
                        messages: vec![canonical],
                        tools: None,
                    }),
                    model: Some(model),
                    thinking_level: Some(xjpcode_agent::ThinkingLevel::High),
                })
            })
        })),
        ..Default::default()
    };
    let model_seen: Arc<Mutex<Option<Model>>> = Arc::new(Mutex::new(None));
    let context_seen: Arc<Mutex<Vec<AgentMessage>>> = Arc::new(Mutex::new(Vec::new()));
    let reasoning_seen: Arc<Mutex<Option<xjpcode_agent::ReasoningLevel>>> =
        Arc::new(Mutex::new(None));
    let model_seen_in_stream = Arc::clone(&model_seen);
    let context_seen_in_stream = Arc::clone(&context_seen);
    let reasoning_seen_in_stream = Arc::clone(&reasoning_seen);
    let stream_fn: StreamFn = Arc::new(move |model, provider_context, options| {
        *model_seen_in_stream.lock().expect("model") = Some(model);
        let messages: Vec<AgentMessage> = provider_context
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::User(message) => Some(AgentMessage::User(message.clone())),
                _ => None,
            })
            .collect();
        *context_seen_in_stream.lock().expect("context") = messages;
        *reasoning_seen_in_stream.lock().expect("reasoning") = options.options.reasoning;
        done_stream(text_message("done"), StopReason::Stop)
    });
    let completed_for_emit = Arc::clone(&completed_messages);
    let emit = Arc::new(move |event: AgentEvent| {
        if let AgentEvent::MessageEnd { message } = &event {
            completed_for_emit
                .lock()
                .expect("completed")
                .push(message.role().to_string());
        }
        boxed(async {})
    });
    run_agent_loop(
        vec![user_message("prompt")],
        empty_context(),
        config,
        emit,
        None,
        Some(stream_fn),
    )
    .await
    .expect("run");

    assert_eq!(prepare_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        model_seen
            .lock()
            .expect("model")
            .as_ref()
            .map(|model| model.id.clone()),
        Some("replacement".to_string())
    );
    let context_seen = context_seen.lock().expect("context").clone();
    assert_eq!(context_seen.len(), 1);
    assert_eq!(context_seen[0].role(), "user");
    assert_eq!(
        *reasoning_seen.lock().expect("reasoning"),
        Some(xjpcode_agent::ReasoningLevel::High)
    );
    // 上游在这里断言 `context.messages` 与 canonical 逐项相等；上面已按内容比过。
    drop(steering_for_hook);
}

#[tokio::test]
async fn does_not_poll_steering_after_prepare_request() {
    let queued: Arc<Mutex<Vec<AgentMessage>>> = Arc::new(Mutex::new(Vec::new()));
    let request_included_steering: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
    let request_preparations = Arc::new(AtomicUsize::new(0));
    let steering_polls = Arc::new(AtomicUsize::new(0));

    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        get_steering_messages: {
            let queued = Arc::clone(&queued);
            let steering_polls = Arc::clone(&steering_polls);
            Some(Arc::new(move || {
                steering_polls.fetch_add(1, Ordering::SeqCst);
                let queued = Arc::clone(&queued);
                boxed(async move {
                    let mut queued = queued.lock().expect("queued");
                    std::mem::take(&mut *queued)
                })
            }))
        },
        prepare_request: {
            let queued = Arc::clone(&queued);
            let request_preparations = Arc::clone(&request_preparations);
            Some(Arc::new(move |_request, _signal| {
                let preparation = request_preparations.fetch_add(1, Ordering::SeqCst) + 1;
                if preparation == 1 {
                    queued
                        .lock()
                        .expect("queued")
                        .push(user_message("late steering"));
                }
                boxed(async { None })
            }))
        },
        ..Default::default()
    };
    let included_in_stream = Arc::clone(&request_included_steering);
    let stream_fn: StreamFn = Arc::new(move |_model, provider_context, _options| {
        included_in_stream.lock().expect("included").push(
            provider_context.messages.iter().any(|message| {
                matches!(message, Message::User(message) if message.content == UserContent::Text("late steering".to_string()))
            }),
        );
        done_stream(text_message("done"), StopReason::Stop)
    });
    let stream = agent_loop(
        vec![user_message("run")],
        empty_context(),
        config,
        None,
        Some(stream_fn),
    );
    let _ = stream.result().await;

    assert_eq!(
        *request_included_steering.lock().expect("included"),
        vec![false, true]
    );
    assert_eq!(request_preparations.load(Ordering::SeqCst), 2);
    // 开始前、这一轮之后、自然停下前的那一次。
    assert_eq!(steering_polls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn uses_prepare_next_turn_snapshot_before_continuing() {
    let tool = TestTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        Arc::new(|_id, params, _signal, _update| {
            let value = value_of(params);
            boxed(async move {
                Ok(xjpcode_agent::AgentToolResult::text(format!(
                    "echoed: {value}"
                )))
            })
        }),
    );
    let prepare_calls = Arc::new(AtomicUsize::new(0));
    let prepared = Arc::new(AtomicBool::new(false));
    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        prepare_next_turn: {
            let prepare_calls = Arc::clone(&prepare_calls);
            let prepared = Arc::clone(&prepared);
            Some(Arc::new(move |context| {
                prepare_calls.fetch_add(1, Ordering::SeqCst);
                let already_prepared = prepared.swap(true, Ordering::SeqCst);
                let messages = context.context.messages.clone();
                let tools = context.context.tools.clone();
                boxed(async move {
                    if already_prepared {
                        return None;
                    }
                    Some(xjpcode_agent::AgentLoopTurnUpdate {
                        context: Some(AgentContext { messages, tools }),
                        messages: Some(vec![AgentMessage::System(xjpcode_ai::SystemMessage {
                            role: xjpcode_ai::SystemRole::System,
                            content: xjpcode_ai::SystemContent::Text(
                                "updated guidance".to_string(),
                            ),
                            sections: None,
                            tools_added: None,
                            tools_removed: None,
                            timestamp: 1,
                        })]),
                        model: None,
                        thinking_level: None,
                    })
                })
            }))
        },
        ..Default::default()
    };
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let saw_update = Arc::new(AtomicBool::new(false));
    let llm_in_stream = Arc::clone(&llm_calls);
    let saw_update_in_stream = Arc::clone(&saw_update);
    let stream_fn: StreamFn = Arc::new(move |_model, provider_context, _options| {
        let call = llm_in_stream.fetch_add(1, Ordering::SeqCst) + 1;
        if call == 2 {
            saw_update_in_stream.store(
                provider_context.messages.iter().any(|message| {
                    matches!(message, Message::System(message) if message.content == xjpcode_ai::SystemContent::Text("updated guidance".to_string()))
                }),
                Ordering::SeqCst,
            );
        }
        if call == 1 {
            done_stream(
                assistant_message(
                    vec![tool_call_content(
                        "tool-1",
                        "echo",
                        json!({ "value": "hello" }),
                    )],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            )
        } else {
            done_stream(text_message("done"), StopReason::Stop)
        }
    });
    let stream = agent_loop(
        vec![user_message("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![Arc::new(tool)]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = collect(&stream).await;

    assert_eq!(llm_calls.load(Ordering::SeqCst), 2);
    assert_eq!(prepare_calls.load(Ordering::SeqCst), 1);
    assert!(saw_update.load(Ordering::SeqCst));
}

#[tokio::test]
async fn picks_up_steering_queued_during_prepare_next_turn_before_the_next_request() {
    let tool = TestTool::new(
        "noop",
        "Noop",
        "Noop tool",
        json!({ "type": "object", "properties": {} }),
        Arc::new(|_id, _params, _signal, _update| {
            boxed(async { Ok(xjpcode_agent::AgentToolResult::text("done")) })
        }),
    );
    let queued: Arc<Mutex<Vec<AgentMessage>>> = Arc::new(Mutex::new(Vec::new()));
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let second_request_included_steering = Arc::new(AtomicBool::new(false));
    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        prepare_next_turn: {
            let queued = Arc::clone(&queued);
            Some(prepare_next_turn_hook(move |_context| {
                queued
                    .lock()
                    .expect("queued")
                    .push(user_message("late steering"));
                boxed(async { None })
            }))
        },
        get_steering_messages: {
            let queued = Arc::clone(&queued);
            Some(Arc::new(move || {
                let queued = Arc::clone(&queued);
                boxed(async move {
                    let mut queued = queued.lock().expect("queued");
                    std::mem::take(&mut *queued)
                })
            }))
        },
        ..Default::default()
    };
    let calls_in_stream = Arc::clone(&provider_calls);
    let included_in_stream = Arc::clone(&second_request_included_steering);
    let stream_fn: StreamFn = Arc::new(move |_model, provider_context, _options| {
        let call = calls_in_stream.fetch_add(1, Ordering::SeqCst) + 1;
        if call == 2 {
            included_in_stream.store(
                provider_context.messages.iter().any(|message| {
                    matches!(message, Message::User(message) if message.content == UserContent::Text("late steering".to_string()))
                }),
                Ordering::SeqCst,
            );
        }
        if call == 1 {
            done_stream(
                assistant_message(
                    vec![tool_call_content("tool-1", "noop", json!({}))],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            )
        } else {
            done_stream(text_message("done"), StopReason::Stop)
        }
    });
    let stream = agent_loop(
        vec![user_message("run")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![Arc::new(tool)]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = stream.result().await;

    assert_eq!(provider_calls.load(Ordering::SeqCst), 2);
    assert!(second_request_included_steering.load(Ordering::SeqCst));
}

#[tokio::test]
async fn action_end_receives_finalized_turn_context_and_stops_before_queue_polling() {
    let executed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_in_tool = Arc::clone(&executed);
    let tool = TestTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        Arc::new(move |_id, params, _signal, _update| {
            let value = value_of(params);
            let executed = Arc::clone(&executed_in_tool);
            boxed(async move {
                executed.lock().expect("executed").push(value.clone());
                Ok(xjpcode_agent::AgentToolResult::text(format!(
                    "echoed: {value}"
                )))
            })
        }),
    );
    let steering_polls = Arc::new(AtomicUsize::new(0));
    let follow_up_polls = Arc::new(AtomicUsize::new(0));
    let callback_tool_result_ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let callback_context_roles: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let ids_in_finish = Arc::clone(&callback_tool_result_ids);
    let roles_in_finish = Arc::clone(&callback_context_roles);
    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        finish_turn: Some(finish_hook(move |context, _signal| {
            assert_eq!(context.message.role, xjpcode_ai::AssistantRole::Assistant);
            *ids_in_finish.lock().expect("ids") = context
                .tool_results
                .iter()
                .map(|result| result.tool_call_id.clone())
                .collect();
            *roles_in_finish.lock().expect("roles") = context
                .context
                .messages
                .iter()
                .map(|message| message.role().to_string())
                .collect();
            boxed(async { Some(xjpcode_agent::AgentTurnDecision::End) })
        })),
        get_steering_messages: {
            let steering_polls = Arc::clone(&steering_polls);
            Some(Arc::new(move || {
                steering_polls.fetch_add(1, Ordering::SeqCst);
                boxed(async { Vec::new() })
            }))
        },
        get_follow_up_messages: {
            let follow_up_polls = Arc::clone(&follow_up_polls);
            Some(Arc::new(move || {
                follow_up_polls.fetch_add(1, Ordering::SeqCst);
                boxed(async { vec![user_message("follow up should stay queued")] })
            }))
        },
        ..Default::default()
    };
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&llm_calls);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        let call = calls_in_stream.fetch_add(1, Ordering::SeqCst) + 1;
        if call == 1 {
            done_stream(
                assistant_message(
                    vec![tool_call_content(
                        "tool-1",
                        "echo",
                        json!({ "value": "hello" }),
                    )],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            )
        } else {
            done_stream(text_message("should not run"), StopReason::Stop)
        }
    });
    let stream = agent_loop(
        vec![user_message("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![Arc::new(tool)]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let events = collect(&stream).await;
    let messages = stream.result().await;

    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *executed.lock().expect("executed"),
        vec!["hello".to_string()]
    );
    assert_eq!(steering_polls.load(Ordering::SeqCst), 1);
    assert_eq!(follow_up_polls.load(Ordering::SeqCst), 0);
    assert_eq!(
        *callback_tool_result_ids.lock().expect("ids"),
        vec!["tool-1".to_string()]
    );
    assert_eq!(
        *callback_context_roles.lock().expect("roles"),
        vec![
            "system".to_string(),
            "user".to_string(),
            "assistant".to_string(),
            "toolResult".to_string()
        ]
    );
    let roles: Vec<&str> = messages.iter().map(AgentMessage::role).collect();
    assert_eq!(roles, vec!["system", "user", "assistant", "toolResult"]);
    assert_eq!(
        event_types(&events),
        vec![
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "tool_execution_start",
            "tool_execution_end",
            "message_start",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );
}

#[tokio::test]
async fn stops_after_a_tool_batch_when_every_tool_result_sets_terminate() {
    let tool = TestTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        Arc::new(|_id, params, _signal, _update| {
            let value = value_of(params);
            boxed(async move {
                let mut result = xjpcode_agent::AgentToolResult::text(format!("echoed: {value}"));
                result.terminate = Some(true);
                Ok(result)
            })
        }),
    );
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&llm_calls);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        calls_in_stream.fetch_add(1, Ordering::SeqCst);
        done_stream(
            assistant_message(
                vec![tool_call_content(
                    "tool-1",
                    "echo",
                    json!({ "value": "hello" }),
                )],
                StopReason::ToolUse,
            ),
            StopReason::ToolUse,
        )
    });
    let stream = agent_loop(
        vec![user_message("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![Arc::new(tool)]),
        },
        config(),
        None,
        Some(stream_fn),
    );
    let events = collect(&stream).await;
    let messages = stream.result().await;

    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
    let roles: Vec<&str> = messages.iter().map(AgentMessage::role).collect();
    assert_eq!(roles, vec!["system", "user", "assistant", "toolResult"]);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::TurnEnd { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn stops_after_a_blocked_tool_call_when_before_tool_call_sets_terminate() {
    let executed = Arc::new(AtomicBool::new(false));
    let executed_in_tool = Arc::clone(&executed);
    let tool = TestTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        Arc::new(move |_id, _params, _signal, _update| {
            let executed = Arc::clone(&executed_in_tool);
            boxed(async move {
                executed.store(true, Ordering::SeqCst);
                Ok(xjpcode_agent::AgentToolResult::text("should not execute"))
            })
        }),
    );
    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        before_tool_call: Some(before_hook(|_context, _signal| {
            boxed(async {
                Some(xjpcode_agent::BeforeToolCallResult {
                    block: Some(true),
                    reason: Some("Blocked by policy".to_string()),
                    terminate: Some(true),
                })
            })
        })),
        ..Default::default()
    };
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&llm_calls);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        let call = calls_in_stream.fetch_add(1, Ordering::SeqCst) + 1;
        if call == 1 {
            done_stream(
                assistant_message(
                    vec![tool_call_content(
                        "tool-1",
                        "echo",
                        json!({ "value": "hello" }),
                    )],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            )
        } else {
            done_stream(text_message("should not run"), StopReason::Stop)
        }
    });
    let stream = agent_loop(
        vec![user_message("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![Arc::new(tool)]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = collect(&stream).await;
    let messages = stream.result().await;

    let tool_result = messages
        .iter()
        .find_map(|message| match message {
            AgentMessage::ToolResult(message) => Some(message),
            _ => None,
        })
        .expect("tool result");
    assert!(!executed.load(Ordering::SeqCst));
    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
    assert!(tool_result.is_error);
    assert_eq!(tool_result.content, vec![text_block("Blocked by policy")]);
}

#[tokio::test]
async fn continues_after_a_mixed_batch_with_one_terminating_blocked_call() {
    let executed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let executed_in_tool = Arc::clone(&executed);
    let tool = TestTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        Arc::new(move |_id, params, _signal, _update| {
            let value = value_of(params);
            let executed = Arc::clone(&executed_in_tool);
            boxed(async move {
                executed.lock().expect("executed").push(value.clone());
                Ok(xjpcode_agent::AgentToolResult::text(format!(
                    "echoed: {value}"
                )))
            })
        }),
    );
    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        tool_execution: ToolExecutionMode::Parallel,
        before_tool_call: Some(before_hook(|context, _signal| {
            let first = context
                .args
                .get("value")
                .and_then(Value::as_str)
                .is_some_and(|value| value == "first");
            boxed(async move {
                if first {
                    Some(xjpcode_agent::BeforeToolCallResult {
                        block: Some(true),
                        reason: Some("Blocked first".to_string()),
                        terminate: Some(true),
                    })
                } else {
                    None
                }
            })
        })),
        ..Default::default()
    };
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&llm_calls);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        let call = calls_in_stream.fetch_add(1, Ordering::SeqCst) + 1;
        if call == 1 {
            done_stream(
                assistant_message(
                    vec![
                        tool_call_content("tool-1", "echo", json!({ "value": "first" })),
                        tool_call_content("tool-2", "echo", json!({ "value": "second" })),
                    ],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            )
        } else {
            done_stream(text_message("done"), StopReason::Stop)
        }
    });
    let stream = agent_loop(
        vec![user_message("echo both")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![Arc::new(tool)]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = collect(&stream).await;

    assert_eq!(
        *executed.lock().expect("executed"),
        vec!["second".to_string()]
    );
    assert_eq!(llm_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn continues_after_parallel_tool_calls_when_not_all_results_terminate() {
    let tool = TestTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        Arc::new(|_id, params, _signal, _update| {
            let value = value_of(params);
            boxed(async move {
                let mut result = xjpcode_agent::AgentToolResult::text(format!("echoed: {value}"));
                if value == "first" {
                    result.terminate = Some(true);
                }
                Ok(result)
            })
        }),
    );
    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        tool_execution: ToolExecutionMode::Parallel,
        ..Default::default()
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&calls);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        let call = calls_in_stream.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            done_stream(
                assistant_message(
                    vec![
                        tool_call_content("tool-1", "echo", json!({ "value": "first" })),
                        tool_call_content("tool-2", "echo", json!({ "value": "second" })),
                    ],
                    StopReason::ToolUse,
                ),
                StopReason::ToolUse,
            )
        } else {
            done_stream(text_message("done"), StopReason::Stop)
        }
    });
    let stream = agent_loop(
        vec![user_message("echo both")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![Arc::new(tool)]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = collect(&stream).await;
    let messages = stream.result().await;

    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let roles: Vec<&str> = messages.iter().map(AgentMessage::role).collect();
    assert_eq!(
        roles,
        vec![
            "system",
            "user",
            "assistant",
            "toolResult",
            "toolResult",
            "assistant"
        ]
    );
}

#[tokio::test]
async fn allows_after_tool_call_to_mark_a_tool_batch_as_terminating() {
    let tool = TestTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        Arc::new(|_id, params, _signal, _update| {
            let value = value_of(params);
            boxed(async move {
                Ok(xjpcode_agent::AgentToolResult::text(format!(
                    "echoed: {value}"
                )))
            })
        }),
    );
    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: identity_converter(),
        after_tool_call: Some(after_hook(|_context, _signal| {
            boxed(async {
                Some(xjpcode_agent::AfterToolCallResult {
                    terminate: Some(true),
                    ..Default::default()
                })
            })
        })),
        ..Default::default()
    };
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&llm_calls);
    let stream_fn: StreamFn = Arc::new(move |_model, _context, _options| {
        calls_in_stream.fetch_add(1, Ordering::SeqCst);
        done_stream(
            assistant_message(
                vec![tool_call_content(
                    "tool-1",
                    "echo",
                    json!({ "value": "hello" }),
                )],
                StopReason::ToolUse,
            ),
            StopReason::ToolUse,
        )
    });
    let stream = agent_loop(
        vec![user_message("echo something")],
        AgentContext {
            messages: Vec::new(),
            tools: Some(vec![Arc::new(tool)]),
        },
        config,
        None,
        Some(stream_fn),
    );
    let _ = collect(&stream).await;

    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn agent_loop_continue_rejects_an_empty_context() {
    let error = match agent_loop_continue(empty_context(), config(), None, None) {
        Ok(_) => panic!("empty context must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.message(), "Cannot continue: no messages in context");
}

#[tokio::test]
async fn agent_loop_continue_rejects_an_assistant_tail() {
    let context = AgentContext {
        messages: vec![AgentMessage::Assistant(text_message("done"))],
        tools: None,
    };
    let error = match agent_loop_continue(context, config(), None, None) {
        Ok(_) => panic!("assistant tail must be rejected"),
        Err(error) => error,
    };
    assert_eq!(
        error.message(),
        "Cannot continue from message role: assistant"
    );
}

#[tokio::test]
async fn agent_loop_continue_does_not_emit_user_message_events() {
    let context = AgentContext {
        messages: vec![user_message("Hello")],
        tools: None,
    };
    let stream_fn: StreamFn = Arc::new(|_model, _context, _options| {
        done_stream(text_message("Response"), StopReason::Stop)
    });
    let stream = agent_loop_continue(context, config(), None, Some(stream_fn)).expect("stream");
    let events = collect(&stream).await;
    let messages = stream.result().await;

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role(), "assistant");
    let message_end_roles: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::MessageEnd { message } => Some(message.role().to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(message_end_roles, vec!["assistant".to_string()]);
}

#[tokio::test]
async fn agent_loop_continue_allows_a_custom_message_as_the_last_message() {
    let custom = AgentMessage::Custom(xjpcode_agent::CustomAgentMessage {
        role: "custom".to_string(),
        payload: json!({ "text": "Hook content" })
            .as_object()
            .expect("object")
            .clone(),
    });
    let context = AgentContext {
        messages: vec![custom],
        tools: None,
    };
    let config = AgentLoopConfig {
        model: model(),
        convert_to_llm: Arc::new(|messages: &[AgentMessage]| {
            let messages: Vec<Message> = messages
                .iter()
                .filter_map(|message| match message {
                    AgentMessage::Custom(message) => Some(Message::User(UserMessage {
                        role: UserRole::User,
                        content: UserContent::Text(
                            message
                                .payload
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        ),
                        timestamp: 0,
                    })),
                    AgentMessage::User(message) => Some(Message::User(message.clone())),
                    AgentMessage::Assistant(message) => Some(Message::Assistant(message.clone())),
                    AgentMessage::ToolResult(message) => Some(Message::ToolResult(message.clone())),
                    AgentMessage::System(message) => Some(Message::System(message.clone())),
                })
                .collect();
            boxed(async move { messages })
        }),
        ..Default::default()
    };
    let stream_fn: StreamFn = Arc::new(|_model, _context, _options| {
        done_stream(text_message("Response to custom message"), StopReason::Stop)
    });
    let stream = agent_loop_continue(context, config, None, Some(stream_fn)).expect("stream");
    let events = collect(&stream).await;
    let messages = stream.result().await;

    assert!(!events.is_empty());
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role(), "assistant");
}

/// 上游 `runToolCall` 那一组用的 `echo` 工具。
fn run_tool_call_echo() -> TestTool {
    let mut tool = TestTool::new(
        "echo",
        "Echo",
        "Echo tool",
        value_schema(),
        Arc::new(|_id, params, signal, on_update| {
            let value = value_of(params);
            let update = on_update.cloned();
            boxed(async move {
                if let Some(update) = update {
                    update(xjpcode_agent::AgentToolResult {
                        content: vec![text_block("partial")],
                        details: Some(json!({})),
                        structured_content: None,
                        usage: None,
                        is_error: None,
                        terminate: None,
                    })
                    .await;
                }
                let _ = signal;
                Ok(xjpcode_agent::AgentToolResult {
                    content: vec![text_block(value.clone())],
                    details: Some(json!({})),
                    structured_content: Some(json!({ "value": value })),
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }),
    );
    tool.output_schema = Some(value_schema());
    tool
}

fn tool_call(id: &str, name: &str, arguments: Value) -> ToolCall {
    ToolCall::new(id, name, arguments.as_object().expect("object").clone())
}

fn run_options(tools: Vec<Arc<dyn xjpcode_agent::AgentTool>>) -> RunToolCallOptions {
    RunToolCallOptions {
        tools,
        assistant_message: assistant_message(Vec::new(), StopReason::Stop),
        context: empty_context(),
        signal: None,
        on_update: None,
        before_tool_call: None,
        after_tool_call: None,
    }
}

#[tokio::test]
async fn run_tool_call_validates_runs_the_hooks_and_reports_failures_as_error_outcomes() {
    let failing = TestTool::new(
        "failing",
        "Failing",
        "Returns an error result",
        json!({ "type": "object", "properties": {} }),
        Arc::new(|_id, _params, _signal, _update| {
            boxed(async {
                Ok(xjpcode_agent::AgentToolResult {
                    content: vec![text_block("bad")],
                    details: Some(json!({ "partial": true })),
                    structured_content: None,
                    usage: None,
                    is_error: Some(true),
                    terminate: None,
                })
            })
        }),
    );
    let hook_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let updates: Arc<Mutex<Vec<xjpcode_agent::AgentToolResult>>> = Arc::new(Mutex::new(Vec::new()));

    let hook_calls_in_before = Arc::clone(&hook_calls);
    let before_tool_call = before_hook(move |context, _signal| {
        let id = context.tool_call.id.clone();
        let blocked = context
            .args
            .get("value")
            .and_then(Value::as_str)
            .is_some_and(|value| value == "blocked");
        let hook_calls = Arc::clone(&hook_calls_in_before);
        boxed(async move {
            hook_calls
                .lock()
                .expect("hook calls")
                .push(format!("before {id}"));
            if blocked {
                Some(xjpcode_agent::BeforeToolCallResult {
                    block: Some(true),
                    reason: Some("nope".to_string()),
                    terminate: None,
                })
            } else {
                None
            }
        })
    });
    let hook_calls_in_after = Arc::clone(&hook_calls);
    let after_tool_call = after_hook(move |context, _signal| {
        let id = context.tool_call.id.clone();
        let hook_calls = Arc::clone(&hook_calls_in_after);
        boxed(async move {
            hook_calls
                .lock()
                .expect("hook calls")
                .push(format!("after {id}"));
            None
        })
    });
    let updates_in_hook = Arc::clone(&updates);
    let tools: Vec<Arc<dyn xjpcode_agent::AgentTool>> =
        vec![Arc::new(run_tool_call_echo()), Arc::new(failing)];

    // 上游五个调用共用同一个 hooks 对象；只有 a 带上 `onUpdate`（echo 只在那一次跑）。
    let make_options = || {
        let mut options = run_options(tools.clone());
        options.before_tool_call = Some(before_tool_call.clone());
        options.after_tool_call = Some(after_tool_call.clone());
        options
    };

    let mut options = make_options();
    options.on_update = Some(Arc::new(move |partial| {
        let updates = Arc::clone(&updates_in_hook);
        boxed(async move {
            updates.lock().expect("updates").push(partial);
        })
    }));
    let a = run_tool_call(&tool_call("a", "echo", json!({ "value": "a" })), options).await;
    assert_eq!(a.tool_call.id, "a");
    assert_eq!(a.result.structured_content, Some(json!({ "value": "a" })));
    assert!(!a.is_error);

    let b = run_tool_call(
        &tool_call("b", "echo", json!({ "value": { "nested": true } })),
        make_options(),
    )
    .await;
    assert!(b.is_error);

    let c = run_tool_call(
        &tool_call("c", "echo", json!({ "value": "blocked" })),
        make_options(),
    )
    .await;
    assert_eq!(c.result.content, vec![text_block("nope")]);
    assert!(c.is_error);

    let d = run_tool_call(&tool_call("d", "missing", json!({})), make_options()).await;
    assert_eq!(d.result.content, vec![text_block("Tool missing not found")]);
    assert!(d.is_error);

    let e = run_tool_call(&tool_call("e", "failing", json!({})), make_options()).await;
    assert_eq!(e.result.details, Some(json!({ "partial": true })));
    assert!(e.is_error);

    let updates = updates.lock().expect("updates").clone();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].content, vec![text_block("partial")]);
    assert_eq!(
        *hook_calls.lock().expect("hook calls"),
        vec![
            "before a".to_string(),
            "after a".to_string(),
            "before c".to_string(),
            "before e".to_string(),
            "after e".to_string()
        ]
    );
}

#[tokio::test]
async fn after_tool_call_replaces_structured_content_and_drops_it_when_only_content_is_replaced() {
    let redacted = vec![text_block("redacted")];
    let results = vec![
        xjpcode_agent::AfterToolCallResult {
            content: Some(redacted.clone()),
            ..Default::default()
        },
        xjpcode_agent::AfterToolCallResult {
            structured_content: Some(json!({ "value": "replaced" })),
            ..Default::default()
        },
        xjpcode_agent::AfterToolCallResult {
            content: Some(redacted.clone()),
            structured_content: Some(json!({ "value": "both" })),
            ..Default::default()
        },
        xjpcode_agent::AfterToolCallResult {
            details: Some(json!({ "note": "kept" })),
            ..Default::default()
        },
    ];
    let mut seen = Vec::new();
    for after_result in results {
        let seen_result = after_result.clone();
        let mut options = run_options(vec![Arc::new(run_tool_call_echo())]);
        options.after_tool_call = Some(after_hook(move |_context, _signal| {
            let after_result = seen_result.clone();
            boxed(async move { Some(after_result) })
        }));
        let outcome = run_tool_call(
            &tool_call("x", "echo", json!({ "value": "original" })),
            options,
        )
        .await;
        seen.push(outcome.result.structured_content);
    }

    assert_eq!(
        seen,
        vec![
            None,
            Some(json!({ "value": "replaced" })),
            Some(json!({ "value": "both" })),
            Some(json!({ "value": "original" }))
        ]
    );
}
