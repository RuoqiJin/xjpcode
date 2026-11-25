//! 上游没有测到的四条分支；这些用例是对着 `packages/agent/src/agent-loop.ts` 的行写的，每条开头注明
//! 对的是哪几行。
//!
//! 来历：这四处 Rust 侧曾经和上游不一样，而搬过来的 90 条测试都通过（2026-11-24 复核）。上游的测试
//! 文件里没有它们，所以单放一个文件，不混进搬来的那几份里。

#[path = "utils/tools.rs"]
mod tools;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use xjpcode_agent::{
    agent_loop, empty_assistant_message, run_agent_loop, run_agent_loop_continue,
    set_default_stream_fn, Agent, AgentContext, AgentEvent, AgentEventSink, AgentInitialState,
    AgentLoopConfig, AgentMessage, AgentOptions, AgentToolResult, BeforeToolCallResult,
    ConvertToLlm, StreamFn,
};
use xjpcode_ai::{
    create_assistant_message_event_stream, AssistantContentBlock, AssistantMessage,
    AssistantMessageEvent, AssistantMessageEventStream, EventStream, Message, StopReason,
    TextContent, ToolCall, UserContent, UserMessage, UserRole,
};

use tools::{before_tool_call as before_hook, boxed, TestTool};

const NO_DEFAULT_STREAM_FN: &str =
    "No default stream function configured. Pass streamFn explicitly or call setDefaultStreamFn().";

fn text_message(text: &str) -> AssistantMessage {
    let mut message = empty_assistant_message("openai-responses", "openai", "mock");
    message.content = vec![AssistantContentBlock::Text(TextContent::new(text))];
    message.stop_reason = StopReason::Stop;
    message
}

/// 一条只含一个工具调用的 assistant 响应，放在一个已经给出 `done` 的流里。
fn tool_use_stream(tool_name: &str) -> AssistantMessageEventStream {
    let mut message = empty_assistant_message("openai-responses", "openai", "mock");
    message.content = vec![AssistantContentBlock::ToolCall(ToolCall::new(
        "call-1",
        tool_name,
        json!({}).as_object().expect("an object").clone(),
    ))];
    message.stop_reason = StopReason::ToolUse;
    let stream = create_assistant_message_event_stream();
    stream.push(AssistantMessageEvent::Done {
        reason: StopReason::ToolUse,
        message,
    });
    stream
}

fn user_message(text: &str) -> AgentMessage {
    AgentMessage::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp: 0,
    })
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

fn config() -> AgentLoopConfig {
    AgentLoopConfig {
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

async fn collect(stream: &EventStream<AgentEvent, Vec<AgentMessage>>) -> Vec<AgentEvent> {
    let mut consumer = stream.consumer();
    let mut events = Vec::new();
    while let Some(event) = consumer.next().await {
        events.push(event);
    }
    events
}

/// 一个把事件类型记下来的出口，和记下来的那份清单。
fn recording_sink() -> (AgentEventSink, Arc<Mutex<Vec<&'static str>>>) {
    let seen: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_in_sink = Arc::clone(&seen);
    let emit: AgentEventSink = Arc::new(move |event| {
        seen_in_sink.lock().expect("seen").push(event.r#type());
        boxed(async {})
    });
    (emit, seen)
}

fn tool_result_texts(messages: &[AgentMessage]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            AgentMessage::ToolResult(message) => {
                Some(xjpcode_ai::content_text(&message.content, ""))
            }
            _ => None,
        })
        .collect()
}

/// 对 `agent-loop.ts:839-842`：`onUpdate` 一被调用，事件就开始发，不等 `execute()` 返回。
///
/// 工具报一次进度，然后等监听器说「看到了」再收工（最多等两秒）。事件要是攒到工具结束才发，
/// 工具就等不到。
#[tokio::test]
async fn delivers_a_tool_update_while_the_tool_is_still_running() {
    let seen = Arc::new(tokio::sync::Notify::new());
    let seen_in_tool = Arc::clone(&seen);
    let tool = TestTool::new(
        "slow_tool",
        "Slow Tool",
        "Reports progress, then waits until a listener has seen it",
        json!({ "type": "object", "properties": {} }),
        Arc::new(move |_id, _params, _signal, on_update| {
            let update = on_update.cloned();
            let seen = Arc::clone(&seen_in_tool);
            boxed(async move {
                if let Some(update) = update {
                    update(AgentToolResult::text("progress")).await;
                }
                let delivered = tokio::time::timeout(Duration::from_secs(2), seen.notified())
                    .await
                    .is_ok();
                Ok(AgentToolResult {
                    terminate: Some(true),
                    ..AgentToolResult::text(if delivered {
                        "seen while running"
                    } else {
                        "not seen while running"
                    })
                })
            })
        }),
    );
    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            tools: Some(vec![Arc::new(tool)]),
            ..Default::default()
        }),
        stream_fn: Some(Arc::new(|_model, _context, _options| {
            tool_use_stream("slow_tool")
        })),
        ..Default::default()
    })
    .expect("agent");
    let _subscription = agent.subscribe(move |event, _signal| {
        if matches!(event, AgentEvent::ToolExecutionUpdate { .. }) {
            seen.notify_one();
        }
        boxed(async {})
    });

    let _ = agent.prompt("run tool").await;

    assert_eq!(
        tool_result_texts(&agent.state().messages),
        vec!["seen while running".to_string()]
    );
}

/// 对 `agent-loop.ts:470-478`：流没有给任何事件、直接带着最终消息结束时，也先发 `message_start`。
#[tokio::test]
async fn emits_message_start_when_the_stream_ends_without_any_event() {
    let stream_fn: StreamFn = Arc::new(|_model, _context, _options| {
        let stream = create_assistant_message_event_stream();
        stream.end(Some(text_message("hello")));
        stream
    });

    let stream = agent_loop(
        vec![user_message("hi")],
        empty_context(),
        config(),
        None,
        Some(stream_fn),
    );
    let events = collect(&stream).await;

    let types: Vec<&'static str> = events.iter().map(AgentEvent::r#type).collect();
    assert_eq!(
        types,
        vec![
            "agent_start",
            "turn_start",
            "message_start",
            "message_end",
            "message_start",
            "message_end",
            "turn_end",
            "agent_end",
        ]
    );
}

/// 对 `agent-loop.ts:755`：`beforeResult.reason || "Tool execution was blocked"`，空字符串也用缺省那句。
#[tokio::test]
async fn uses_the_default_text_when_a_blocked_call_gives_an_empty_reason() {
    let tool = TestTool::new(
        "any_tool",
        "Any Tool",
        "Never runs",
        json!({ "type": "object", "properties": {} }),
        Arc::new(|_id, _params, _signal, _on_update| {
            boxed(async { Ok(AgentToolResult::text("ran")) })
        }),
    );
    let context = AgentContext {
        messages: Vec::new(),
        tools: Some(vec![Arc::new(tool)]),
    };
    let config = AgentLoopConfig {
        before_tool_call: Some(before_hook(|_context, _signal| {
            boxed(async {
                Some(BeforeToolCallResult {
                    block: Some(true),
                    reason: Some(String::new()),
                    terminate: Some(true),
                })
            })
        })),
        ..config()
    };
    let stream_fn: StreamFn = Arc::new(|_model, _context, _options| tool_use_stream("any_tool"));

    let stream = agent_loop(
        vec![user_message("run tool")],
        context,
        config,
        None,
        Some(stream_fn),
    );
    let messages = stream.result().await;

    assert_eq!(
        tool_result_texts(&messages),
        vec!["Tool execution was blocked".to_string()]
    );
}

/// 对 `agent-loop.ts:126-133`、`:155-158`：默认流函数在开头几条事件发出去之后才解析，
/// 所以没有配置时，这几条事件已经发了，然后才报错。
#[tokio::test]
async fn resolves_the_default_stream_function_after_the_opening_events() {
    set_default_stream_fn(None);

    let (emit, seen) = recording_sink();
    let error = run_agent_loop(
        vec![user_message("hi")],
        empty_context(),
        config(),
        emit,
        None,
        None,
    )
    .await
    .expect_err("no stream function is configured");
    assert_eq!(error.message(), NO_DEFAULT_STREAM_FN);
    assert_eq!(
        *seen.lock().expect("seen"),
        vec!["agent_start", "turn_start", "message_start", "message_end"]
    );

    let (emit, seen) = recording_sink();
    let context = AgentContext {
        messages: vec![user_message("hi")],
        tools: None,
    };
    let error = run_agent_loop_continue(context, config(), emit, None, None)
        .await
        .expect_err("no stream function is configured");
    assert_eq!(error.message(), NO_DEFAULT_STREAM_FN);
    assert_eq!(
        *seen.lock().expect("seen"),
        vec!["agent_start", "turn_start"]
    );
}
