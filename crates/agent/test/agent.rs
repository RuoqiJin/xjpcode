//! 移植自上游 `packages/agent/test/agent.test.ts`。
//!
//! 上游这里有五处用到模型目录（`getModel("openai", "gpt-4o-mini")` 等）与 `compat.ts` 的再导出；
//! 模型目录是 provider 那一步的事，这里换成同形状的测试模型（`createModel()`），断言改成按 id 认。
//! `state` 在 Rust 侧是快照（差异 #75）：上游 `agent.state.messages.push(...)` 在这里是
//! `append_message`，断言照旧。

#[path = "utils/tools.rs"]
mod tools;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use xjpcode_agent::{
    empty_assistant_message, set_default_stream_fn, Agent, AgentEvent, AgentMessage, AgentOptions,
    AgentTool, AgentToolResult, AgentToolUpdateCallback, CustomAgentMessage, PromptInput, StreamFn,
    ThinkingLevel, ToolExecutionMode,
};
use xjpcode_ai::providers::model_schema::{ModelCost, ModelInputModality};
use xjpcode_ai::{
    create_assistant_message_event_stream, AbortSignal, AssistantContentBlock, AssistantMessage,
    AssistantMessageEvent, AssistantMessageEventStream, ClientIdentity, Model, StopReason,
    SystemContent, SystemMessage, SystemRole, TextContent, Tool, ToolCall, ToolResultContentBlock,
    ToolResultMessage, ToolResultRole, UserContent, UserMessage, UserRole, PI_ATTRIBUTION,
};

use tools::{boxed, text_block, TestTool};

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

fn user_message(text: &str) -> AgentMessage {
    AgentMessage::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp: 0,
    })
}

fn assistant_message(text: &str) -> AssistantMessage {
    let mut message = empty_assistant_message("openai-responses", "openai", "mock");
    message.content = vec![AssistantContentBlock::Text(TextContent::new(text))];
    message.stop_reason = StopReason::Stop;
    message
}

fn assistant_tool_use_message(tool_calls: Vec<ToolCall>) -> AssistantMessage {
    let mut message = empty_assistant_message("openai-responses", "openai", "mock");
    message.content = tool_calls
        .into_iter()
        .map(AssistantContentBlock::ToolCall)
        .collect();
    message.stop_reason = StopReason::ToolUse;
    message
}

fn tool_call(id: &str, name: &str, arguments: Value) -> ToolCall {
    ToolCall::new(id, name, arguments.as_object().expect("object").clone())
}

fn create_tool(name: &str) -> Arc<dyn AgentTool> {
    let name = name.to_string();
    let name_for_run = name.clone();
    Arc::new(TestTool::new(
        name.clone(),
        name.clone(),
        format!("{name} tool"),
        json!({ "type": "object", "properties": {} }),
        Arc::new(move |_id, _params, _signal, _update| {
            let name = name_for_run.clone();
            boxed(async move { Ok(AgentToolResult::text(name)) })
        }),
    ))
}

fn unused_stream_function() -> StreamFn {
    Arc::new(|_model, _context, _options| panic!("Unexpected stream call"))
}

fn done_stream(message: AssistantMessage, reason: StopReason) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    stream.push(AssistantMessageEvent::Done { reason, message });
    stream
}

/// 等 run 真正开始（Rust 的 future 要等到被 poll 才建 `activeRun`）。
async fn wait_until_streaming(agent: &Agent) {
    for _ in 0..1_000 {
        if agent.state().is_streaming {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    panic!("the agent never started streaming");
}

/// 一次性的等待点（上游的 `createDeferred`）。
fn deferred() -> Arc<tokio::sync::Notify> {
    Arc::new(tokio::sync::Notify::new())
}

#[tokio::test]
async fn uses_the_configured_default_when_a_legacy_caller_omits_stream_fn() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&calls);
    set_default_stream_fn(Some(Arc::new(move |_model, _context, _options| {
        calls_in_stream.fetch_add(1, Ordering::SeqCst);
        done_stream(assistant_message("fallback"), StopReason::Stop)
    })));

    let agent = Agent::new(AgentOptions::default()).expect("agent");
    let _ = agent.prompt("Hello").await;
    set_default_stream_fn(None);

    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn creates_an_agent_with_default_state() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .expect("agent");

    let state = agent.state();
    assert_eq!(state.model.id, "unknown");
    assert_eq!(state.thinking_level, ThinkingLevel::Off);
    assert!(state.tools.is_empty());
    assert!(state.messages.is_empty());
    assert!(!state.is_streaming);
    assert_eq!(state.streaming_message, None);
    assert!(state.pending_tool_calls.is_empty());
    assert_eq!(state.error_message, None);
}

#[tokio::test]
async fn creates_an_agent_with_custom_initial_state() {
    let custom_model = model();
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(unused_stream_function()),
        initial_state: Some(xjpcode_agent::AgentInitialState {
            system_prompt: Some("You are a helpful assistant.".to_string()),
            model: Some(custom_model.clone()),
            thinking_level: Some(ThinkingLevel::Low),
            ..Default::default()
        }),
        ..Default::default()
    })
    .expect("agent");

    let state = agent.state();
    assert_eq!(state.messages.len(), 1);
    match &state.messages[0] {
        AgentMessage::System(message) => {
            assert_eq!(
                message.content,
                SystemContent::Text("You are a helpful assistant.".to_string())
            );
            assert_eq!(message.timestamp, 0);
        }
        other => panic!("expected a system message, got {:?}", other.role()),
    }
    assert_eq!(state.model.id, "mock");
    assert_eq!(state.thinking_level, ThinkingLevel::Low);
}

#[tokio::test]
async fn converts_initial_prompt_and_tools_into_transcript_state() {
    let agent = Agent::new(AgentOptions {
        initial_state: Some(xjpcode_agent::AgentInitialState {
            system_prompt: Some("You are helpful.".to_string()),
            tools: Some(vec![create_tool("echo")]),
            ..Default::default()
        }),
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .expect("agent");

    let state = agent.state();
    match &state.messages[0] {
        AgentMessage::System(message) => {
            assert_eq!(
                message.content,
                SystemContent::Text("You are helpful.".to_string())
            );
            let tools: Vec<String> = message
                .tools_added
                .clone()
                .unwrap_or_default()
                .iter()
                .map(|tool| tool.name.clone())
                .collect();
            assert_eq!(tools, vec!["echo".to_string()]);
        }
        other => panic!("expected a system message, got {:?}", other.role()),
    }
}

#[tokio::test]
async fn declares_tool_loadout_changes_before_the_next_request() {
    let requests: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let requests_in_stream = Arc::clone(&requests);
    let agent = Agent::new(AgentOptions {
        initial_state: Some(xjpcode_agent::AgentInitialState {
            system_prompt: Some("You are helpful.".to_string()),
            tools: Some(vec![create_tool("first")]),
            ..Default::default()
        }),
        stream_fn: Some(Arc::new(move |_model, context, _options| {
            let mut seen = Vec::new();
            for message in &context.messages {
                if let xjpcode_ai::Message::System(message) = message {
                    let added: Vec<String> = message
                        .tools_added
                        .clone()
                        .unwrap_or_default()
                        .iter()
                        .map(|tool| tool.name.clone())
                        .collect();
                    let removed: Vec<String> = message
                        .tools_removed
                        .clone()
                        .unwrap_or_default()
                        .iter()
                        .map(|tool| tool.name.clone())
                        .collect();
                    seen.push(format!("+{}", added.join(",")));
                    seen.push(format!("-{}", removed.join(",")));
                }
            }
            requests_in_stream.lock().expect("requests").push(seen);
            done_stream(assistant_message("done"), StopReason::Stop)
        })),
        ..Default::default()
    })
    .expect("agent");

    let _ = agent.prompt("one").await;
    agent.set_tools(vec![create_tool("second")]);
    let _ = agent.prompt("two").await;
    let _ = agent.prompt("three").await;

    assert_eq!(
        *requests.lock().expect("requests"),
        vec![
            vec!["+first".to_string(), "-".to_string()],
            vec![
                "+first".to_string(),
                "-".to_string(),
                "+second".to_string(),
                "-first".to_string()
            ],
            vec![
                "+first".to_string(),
                "-".to_string(),
                "+second".to_string(),
                "-first".to_string()
            ],
        ]
    );

    let messages = agent.state().messages;
    let update = messages
        .iter()
        .find_map(|message| match message {
            AgentMessage::System(message) if message.tools_removed.is_some() => Some(message),
            _ => None,
        })
        .expect("tool update message");
    assert_eq!(update.content, SystemContent::Text(String::new()));
    assert_eq!(
        update
            .tools_added
            .clone()
            .unwrap_or_default()
            .iter()
            .map(|tool| (tool.name.clone(), tool.description.clone()))
            .collect::<Vec<_>>(),
        vec![("second".to_string(), "second tool".to_string())]
    );
    assert_eq!(
        update
            .tools_removed
            .clone()
            .unwrap_or_default()
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>(),
        vec!["first".to_string()]
    );
    // 记录里的工具声明没有可执行的字段。
    match &messages[0] {
        AgentMessage::System(message) => {
            let declaration: &Tool = &message.tools_added.as_ref().expect("tools")[0];
            assert_eq!(declaration.name, "first");
            assert_eq!(
                declaration.parameters,
                json!({ "type": "object", "properties": {} })
            );
        }
        other => panic!(
            "expected the initial system message, got {:?}",
            other.role()
        ),
    }
}

#[tokio::test]
async fn merges_tool_changes_into_a_pending_system_message() {
    let agent = Agent::new(AgentOptions {
        initial_state: Some(xjpcode_agent::AgentInitialState {
            system_prompt: Some("You are helpful.".to_string()),
            ..Default::default()
        }),
        stream_fn: Some(Arc::new(|_model, context, _options| {
            let system_count = context
                .messages
                .iter()
                .filter(|message| message.role() == "system")
                .count();
            assert_eq!(system_count, 2);
            done_stream(assistant_message("done"), StopReason::Stop)
        })),
        ..Default::default()
    })
    .expect("agent");

    agent.set_tools(vec![create_tool("echo")]);
    let sections: indexmap::IndexMap<String, Option<String>> =
        [("skills".to_string(), Some("<skills>x</skills>".to_string()))]
            .into_iter()
            .collect();
    let pending = AgentMessage::System(SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text(String::new()),
        sections: Some(sections),
        tools_added: None,
        tools_removed: None,
        timestamp: 1,
    });
    let _ = agent
        .prompt(PromptInput::Messages(vec![pending, user_message("hi")]))
        .await;

    let messages = agent.state().messages;
    match &messages[1] {
        AgentMessage::System(message) => {
            assert_eq!(message.content, SystemContent::Text(String::new()));
            assert_eq!(
                message
                    .sections
                    .as_ref()
                    .and_then(|sections| sections.get("skills"))
                    .cloned()
                    .flatten(),
                Some("<skills>x</skills>".to_string())
            );
            assert_eq!(message.timestamp, 1);
            assert_eq!(
                message
                    .tools_added
                    .clone()
                    .unwrap_or_default()
                    .iter()
                    .map(|tool| tool.name.clone())
                    .collect::<Vec<_>>(),
                vec!["echo".to_string()]
            );
        }
        other => panic!(
            "expected the pending system message, got {:?}",
            other.role()
        ),
    }
}

#[tokio::test]
async fn rewrites_pending_tool_declarations_to_match_the_executable_set() {
    let agent = Agent::new(AgentOptions {
        initial_state: Some(xjpcode_agent::AgentInitialState {
            system_prompt: Some("You are helpful.".to_string()),
            tools: Some(vec![create_tool("first")]),
            ..Default::default()
        }),
        stream_fn: Some(Arc::new(|_model, _context, _options| {
            done_stream(assistant_message("done"), StopReason::Stop)
        })),
        ..Default::default()
    })
    .expect("agent");

    // 待发的系统消息说自己加 `second`、删 `first`，但可执行集合仍是 `first`：以可执行集合为准。
    let second_declaration = Tool {
        name: "second".to_string(),
        description: "second tool".to_string(),
        parameters: json!({ "type": "object", "properties": {} }),
        constrained_sampling: None,
    };
    let pending = AgentMessage::System(SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text(String::new()),
        sections: Some(
            [("note".to_string(), Some("<note>x</note>".to_string()))]
                .into_iter()
                .collect(),
        ),
        tools_added: Some(vec![second_declaration]),
        tools_removed: Some(vec![xjpcode_ai::ToolReference {
            name: "first".to_string(),
        }]),
        timestamp: 1,
    });
    let _ = agent
        .prompt(PromptInput::Messages(vec![pending, user_message("hi")]))
        .await;

    let messages = agent.state().messages;
    match &messages[1] {
        AgentMessage::System(message) => {
            assert_eq!(message.content, SystemContent::Text(String::new()));
            assert_eq!(message.timestamp, 1);
            assert_eq!(
                message.sections.as_ref().map(|sections| sections.len()),
                Some(1)
            );
            assert_eq!(message.tools_added, None);
            assert_eq!(message.tools_removed, None);
        }
        other => panic!(
            "expected the pending system message, got {:?}",
            other.role()
        ),
    }
    let system_messages: Vec<xjpcode_ai::Message> = messages
        .iter()
        .filter_map(|message| match message {
            AgentMessage::System(message) => Some(xjpcode_ai::Message::System(message.clone())),
            _ => None,
        })
        .collect();
    let current = xjpcode_ai::get_current_system_message(&system_messages).expect("system message");
    assert_eq!(
        current
            .tools_added
            .unwrap_or_default()
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>(),
        vec!["first".to_string()]
    );
}

#[tokio::test]
async fn restores_the_transcript_baseline_when_reset() {
    let agent = Agent::new(AgentOptions {
        initial_state: Some(xjpcode_agent::AgentInitialState {
            system_prompt: Some("You are helpful.".to_string()),
            tools: Some(vec![create_tool("echo")]),
            messages: Some(vec![user_message("old")]),
            ..Default::default()
        }),
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .expect("agent");

    agent.reset().expect("reset");

    let messages = agent.state().messages;
    assert_eq!(messages.len(), 1);
    match &messages[0] {
        AgentMessage::System(message) => {
            assert_eq!(
                message.content,
                SystemContent::Text("You are helpful.".to_string())
            );
            assert_eq!(
                message
                    .tools_added
                    .clone()
                    .unwrap_or_default()
                    .iter()
                    .map(|tool| tool.name.clone())
                    .collect::<Vec<_>>(),
                vec!["echo".to_string()]
            );
        }
        other => panic!(
            "expected the initial system message, got {:?}",
            other.role()
        ),
    }
}

#[tokio::test]
async fn subscribes_to_events() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .expect("agent");

    let event_count = Arc::new(AtomicUsize::new(0));
    let events = Arc::clone(&event_count);
    let subscription = agent.subscribe(move |_event, _signal| {
        events.fetch_add(1, Ordering::SeqCst);
        boxed(async {})
    });
    assert_eq!(event_count.load(Ordering::SeqCst), 0);

    // 直接改状态不发事件。
    agent.set_thinking_level(ThinkingLevel::Low);
    assert_eq!(event_count.load(Ordering::SeqCst), 0);
    assert_eq!(agent.state().thinking_level, ThinkingLevel::Low);

    drop(subscription);
    agent.set_thinking_level(ThinkingLevel::High);
    assert_eq!(event_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn emits_full_lifecycle_events_for_thrown_run_failures() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(Arc::new(|_model, _context, _options| {
            panic!("provider exploded")
        })),
        ..Default::default()
    })
    .expect("agent");
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let events_in_listener = Arc::clone(&events);
    let _subscription = agent.subscribe(move |event, _signal| {
        events_in_listener
            .lock()
            .expect("events")
            .push(event.r#type().to_string());
        boxed(async {})
    });

    let _ = agent.prompt("hello").await;

    assert_eq!(
        *events.lock().expect("events"),
        vec![
            "agent_start".to_string(),
            "turn_start".to_string(),
            "message_start".to_string(),
            "message_end".to_string(),
            "message_start".to_string(),
            "message_end".to_string(),
            "turn_end".to_string(),
            "agent_end".to_string(),
        ]
    );
    let state = agent.state();
    let last = state.messages.last().expect("last message");
    match last {
        AgentMessage::Assistant(message) => {
            assert_eq!(message.stop_reason, StopReason::Error);
            assert_eq!(message.error_message.as_deref(), Some("provider exploded"));
        }
        other => panic!("expected an assistant message, got {:?}", other.role()),
    }
    assert_eq!(state.error_message.as_deref(), Some("provider exploded"));
}

#[tokio::test]
async fn awaits_async_subscribers_before_prompt_resolves() {
    let barrier = deferred();
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(Arc::new(|_model, _context, _options| {
            done_stream(assistant_message("ok"), StopReason::Stop)
        })),
        ..Default::default()
    })
    .expect("agent");

    let listener_finished = Arc::new(AtomicUsize::new(0));
    let listener_finished_in_listener = Arc::clone(&listener_finished);
    let barrier_in_listener = Arc::clone(&barrier);
    let _subscription = agent.subscribe(move |event, _signal| {
        let listener_finished = Arc::clone(&listener_finished_in_listener);
        let barrier = Arc::clone(&barrier_in_listener);
        let is_end = matches!(event, AgentEvent::AgentEnd { .. });
        boxed(async move {
            if is_end {
                barrier.notified().await;
                listener_finished.store(1, Ordering::SeqCst);
            }
        })
    });

    let prompt = agent.prompt("hello");
    tokio::pin!(prompt);
    tokio::select! {
        _ = &mut prompt => panic!("prompt must wait for the listener"),
        () = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
    }

    assert_eq!(listener_finished.load(Ordering::SeqCst), 0);
    assert!(agent.state().is_streaming);

    barrier.notify_waiters();
    prompt.await.expect("prompt");

    assert_eq!(listener_finished.load(Ordering::SeqCst), 1);
    assert!(!agent.state().is_streaming);
}

#[tokio::test]
async fn wait_for_idle_waits_for_async_subscribers() {
    let barrier = deferred();
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(Arc::new(|_model, _context, _options| {
            done_stream(assistant_message("ok"), StopReason::Stop)
        })),
        ..Default::default()
    })
    .expect("agent");

    let barrier_in_listener = Arc::clone(&barrier);
    let _subscription = agent.subscribe(move |event, _signal| {
        let barrier = Arc::clone(&barrier_in_listener);
        let is_assistant_end = matches!(
            event,
            AgentEvent::MessageEnd {
                message: AgentMessage::Assistant(_)
            }
        );
        boxed(async move {
            if is_assistant_end {
                barrier.notified().await;
            }
        })
    });

    // Rust 的 future 在被 poll 之前什么也不做；先让 run 起来（上游是同步起 run 的）。
    let prompt_agent = agent.clone();
    let prompt = tokio::spawn(async move { prompt_agent.prompt("hello").await });
    wait_until_streaming(&agent).await;

    let idle = agent.wait_for_idle();
    tokio::pin!(idle);
    tokio::select! {
        () = &mut idle => panic!("idle must wait for the listener"),
        () = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
    }

    assert!(agent.state().is_streaming);

    barrier.notify_waiters();
    let (prompt_result, ()) = tokio::join!(prompt, &mut idle);
    prompt_result.expect("prompt task").expect("prompt");

    assert!(!agent.state().is_streaming);
}

#[tokio::test]
async fn passes_the_active_abort_signal_to_subscribers() {
    let received_signal: Arc<Mutex<Option<AbortSignal>>> = Arc::new(Mutex::new(None));
    let received_in_stream = Arc::clone(&received_signal);
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(Arc::new(move |_model, _context, options| {
            let stream = create_assistant_message_event_stream();
            let task_stream = stream.clone();
            let signal = options.signal.clone();
            *received_in_stream.lock().expect("signal") = signal.clone();
            tokio::spawn(async move {
                task_stream.push(AssistantMessageEvent::Start {
                    partial: assistant_message(""),
                });
                if let Some(signal) = signal {
                    signal.cancelled().await;
                }
                let mut message = assistant_message("Aborted");
                message.stop_reason = StopReason::Aborted;
                message.error_message = Some("Request aborted by user".to_string());
                task_stream.push(AssistantMessageEvent::Error {
                    reason: StopReason::Aborted,
                    error: message,
                });
            });
            stream
        })),
        ..Default::default()
    })
    .expect("agent");

    let subscriber_signal: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
    let subscriber_signal_in_listener = Arc::clone(&subscriber_signal);
    let _subscription = agent.subscribe(move |event, signal| {
        if matches!(event, AgentEvent::AgentStart) {
            *subscriber_signal_in_listener.lock().expect("signal") = Some(signal.aborted());
        }
        boxed(async {})
    });

    let prompt = agent.prompt("hello");
    tokio::pin!(prompt);
    tokio::select! {
        _ = &mut prompt => panic!("the stream must wait for the abort"),
        () = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
    }

    assert_eq!(*subscriber_signal.lock().expect("signal"), Some(false));
    let stream_signal = received_signal
        .lock()
        .expect("signal")
        .clone()
        .expect("stream saw a signal");
    assert!(!stream_signal.aborted());

    agent.abort();
    let _ = prompt.await;

    assert!(stream_signal.aborted());
    assert_eq!(*subscriber_signal.lock().expect("signal"), Some(false));
}

#[tokio::test]
async fn ignores_tool_updates_after_the_tool_execution_settles() {
    let delayed_update: Arc<Mutex<Option<AgentToolUpdateCallback>>> = Arc::new(Mutex::new(None));
    let delayed_in_tool = Arc::clone(&delayed_update);
    let tool = TestTool::new(
        "delayed_tool",
        "Delayed Tool",
        "Captures progress callbacks",
        json!({ "type": "object", "properties": {} }),
        Arc::new(move |_id, _params, _signal, on_update| {
            let delayed_in_tool = Arc::clone(&delayed_in_tool);
            let update = on_update.cloned();
            boxed(async move {
                if let Some(update) = update {
                    delayed_in_tool
                        .lock()
                        .expect("update")
                        .replace(Arc::clone(&update));
                    update(AgentToolResult {
                        content: vec![text_block("running")],
                        details: Some(json!({ "status": "running" })),
                        structured_content: None,
                        usage: None,
                        is_error: None,
                        terminate: None,
                    })
                    .await;
                }
                Ok(AgentToolResult {
                    content: vec![text_block("ok")],
                    details: Some(json!({ "status": "done" })),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: Some(true),
                })
            })
        }),
    );
    let agent = Agent::new(AgentOptions {
        initial_state: Some(xjpcode_agent::AgentInitialState {
            tools: Some(vec![Arc::new(tool)]),
            ..Default::default()
        }),
        stream_fn: Some(Arc::new(|_model, _context, _options| {
            done_stream(
                assistant_tool_use_message(vec![tool_call("call-1", "delayed_tool", json!({}))]),
                StopReason::ToolUse,
            )
        })),
        ..Default::default()
    })
    .expect("agent");
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_in_listener = Arc::clone(&events);
    let _subscription = agent.subscribe(move |event, _signal| {
        events_in_listener
            .lock()
            .expect("events")
            .push(event.clone());
        boxed(async {})
    });

    let _ = agent.prompt("run tool").await;
    let event_count_after_prompt = events.lock().expect("events").len();

    let update = delayed_update
        .lock()
        .expect("update")
        .clone()
        .expect("captured update");
    update(AgentToolResult {
        content: vec![text_block("late")],
        details: Some(json!({ "status": "late" })),
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(0)).await;

    let events = events.lock().expect("events");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolExecutionUpdate { .. }))
            .count(),
        1
    );
    assert_eq!(events.len(), event_count_after_prompt);
}

#[tokio::test]
async fn ignores_a_settled_parallel_tool_update_while_another_tool_is_still_running() {
    // 上游用两个 Promise 当闸门；`Notify` 不存许可，先通知再等就丢了，所以用 `Semaphore`。
    let slow_started = Arc::new(tokio::sync::Semaphore::new(0));
    let settled_tool_ended = Arc::new(tokio::sync::Semaphore::new(0));
    let release_slow = Arc::new(tokio::sync::Semaphore::new(0));
    let settled_update: Arc<Mutex<Option<AgentToolUpdateCallback>>> = Arc::new(Mutex::new(None));

    let settled_in_tool = Arc::clone(&settled_update);
    let settled_tool = TestTool::new(
        "settled_tool",
        "Settled Tool",
        "Captures progress callbacks",
        json!({ "type": "object", "properties": {} }),
        Arc::new(move |_id, _params, _signal, on_update| {
            let settled_in_tool = Arc::clone(&settled_in_tool);
            let update = on_update.cloned();
            boxed(async move {
                if let Some(update) = update {
                    settled_in_tool
                        .lock()
                        .expect("update")
                        .replace(Arc::clone(&update));
                }
                Ok(AgentToolResult {
                    content: vec![text_block("done")],
                    details: Some(json!({ "status": "done" })),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: Some(true),
                })
            })
        }),
    );
    let slow_started_in_tool = Arc::clone(&slow_started);
    let release_slow_in_tool = Arc::clone(&release_slow);
    let slow_tool = TestTool::new(
        "slow_tool",
        "Slow Tool",
        "Keeps the agent run active",
        json!({ "type": "object", "properties": {} }),
        Arc::new(move |_id, _params, _signal, _update| {
            let slow_started = Arc::clone(&slow_started_in_tool);
            let release_slow = Arc::clone(&release_slow_in_tool);
            boxed(async move {
                slow_started.add_permits(1);
                release_slow.acquire().await.expect("release").forget();
                Ok(AgentToolResult {
                    content: vec![text_block("done")],
                    details: Some(json!({ "status": "done" })),
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: Some(true),
                })
            })
        }),
    );
    let agent = Agent::new(AgentOptions {
        initial_state: Some(xjpcode_agent::AgentInitialState {
            tools: Some(vec![Arc::new(settled_tool), Arc::new(slow_tool)]),
            ..Default::default()
        }),
        stream_fn: Some(Arc::new(|_model, _context, _options| {
            done_stream(
                assistant_tool_use_message(vec![
                    tool_call("call-1", "settled_tool", json!({})),
                    tool_call("call-2", "slow_tool", json!({})),
                ]),
                StopReason::ToolUse,
            )
        })),
        ..Default::default()
    })
    .expect("agent");
    let events: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_in_listener = Arc::clone(&events);
    let settled_tool_ended_in_listener = Arc::clone(&settled_tool_ended);
    let _subscription = agent.subscribe(move |event, _signal| {
        events_in_listener
            .lock()
            .expect("events")
            .push(event.clone());
        if let AgentEvent::ToolExecutionEnd { tool_call_id, .. } = event {
            if tool_call_id == "call-1" {
                settled_tool_ended_in_listener.add_permits(1);
            }
        }
        boxed(async {})
    });

    let prompt = agent.prompt("run tools");
    tokio::pin!(prompt);
    let waited = async {
        let (slow, settled) = tokio::join!(slow_started.acquire(), settled_tool_ended.acquire());
        slow.expect("slow started").forget();
        settled.expect("settled tool ended").forget();
    };
    tokio::select! {
        _ = waited => {}
        _ = &mut prompt => panic!("the run must wait for the slow tool"),
    }
    let event_count_before_late_update = events.lock().expect("events").len();

    let update = settled_update
        .lock()
        .expect("update")
        .clone()
        .expect("captured update");
    update(AgentToolResult {
        content: vec![text_block("late")],
        details: Some(json!({ "status": "late" })),
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(0)).await;
    assert_eq!(
        events.lock().expect("events").len(),
        event_count_before_late_update
    );

    release_slow.add_permits(1);
    let _ = prompt.await;
    assert_eq!(
        events
            .lock()
            .expect("events")
            .iter()
            .filter(|event| matches!(event, AgentEvent::ToolExecutionUpdate { .. }))
            .count(),
        0
    );
}

#[tokio::test]
async fn updates_state_with_mutators() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .expect("agent");

    let new_model = Model {
        id: "replacement".to_string(),
        ..model()
    };
    agent.set_model(new_model.clone());
    assert_eq!(agent.state().model.id, "replacement");

    agent.set_thinking_level(ThinkingLevel::High);
    assert_eq!(agent.state().thinking_level, ThinkingLevel::High);

    agent.set_tools(vec![create_tool("test")]);
    let tools = agent.state().tools;
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name(), "test");

    agent.set_messages(vec![user_message("Hello")]);
    assert_eq!(agent.state().messages.len(), 1);

    agent.append_message(AgentMessage::Assistant(assistant_message("Hi")));
    assert_eq!(agent.state().messages.len(), 2);

    agent.set_messages(Vec::new());
    assert!(agent.state().messages.is_empty());
    assert_eq!(new_model.id, "replacement");
}

#[tokio::test]
async fn supports_a_steering_message_queue() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .expect("agent");

    agent.steer(user_message("Steering message"));
    assert!(agent.state().messages.is_empty());
    assert_eq!(agent.peek_queued_messages().len(), 1);
}

#[tokio::test]
async fn supports_a_follow_up_message_queue() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .expect("agent");

    agent.follow_up(user_message("Follow-up message"));
    assert!(agent.state().messages.is_empty());
    assert_eq!(agent.peek_queued_messages().len(), 1);
}

#[tokio::test]
async fn handles_abort_controller_without_a_run() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(unused_stream_function()),
        ..Default::default()
    })
    .expect("agent");
    agent.abort();
}

#[tokio::test]
async fn rejects_reset_while_processing_without_corrupting_the_transcript() {
    let stream_started = deferred();
    let release_response = deferred();
    let started_in_stream = Arc::clone(&stream_started);
    let release_in_stream = Arc::clone(&release_response);
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(Arc::new(move |_model, _context, _options| {
            let stream = create_assistant_message_event_stream();
            let task_stream = stream.clone();
            let started = Arc::clone(&started_in_stream);
            let release = Arc::clone(&release_in_stream);
            tokio::spawn(async move {
                task_stream.push(AssistantMessageEvent::Start {
                    partial: assistant_message(""),
                });
                started.notify_waiters();
                release.notified().await;
                task_stream.push(AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: assistant_message("Done"),
                });
            });
            stream
        })),
        ..Default::default()
    })
    .expect("agent");

    let prompt = agent.prompt("Hello");
    tokio::pin!(prompt);
    tokio::select! {
        () = stream_started.notified() => {}
        _ = &mut prompt => panic!("the stream must wait for the release"),
    }

    assert!(agent.state().is_streaming);
    let roles: Vec<String> = agent
        .state()
        .messages
        .iter()
        .map(|message| message.role().to_string())
        .collect();
    assert_eq!(roles, vec!["user".to_string()]);
    let error = agent.reset().expect_err("reset while processing must fail");
    assert_eq!(
        error.message(),
        "Agent is already processing. Wait for completion before resetting."
    );
    assert!(agent.state().is_streaming);
    let roles: Vec<String> = agent
        .state()
        .messages
        .iter()
        .map(|message| message.role().to_string())
        .collect();
    assert_eq!(roles, vec!["user".to_string()]);

    release_response.notify_waiters();
    let _ = prompt.await;

    assert!(!agent.state().is_streaming);
    let roles: Vec<String> = agent
        .state()
        .messages
        .iter()
        .map(|message| message.role().to_string())
        .collect();
    assert_eq!(roles, vec!["user".to_string(), "assistant".to_string()]);
}

#[tokio::test]
async fn rejects_prompt_while_streaming() {
    let stream_started = deferred();
    let started_in_stream = Arc::clone(&stream_started);
    let abort_signals: Arc<Mutex<Option<AbortSignal>>> = Arc::new(Mutex::new(None));
    let signals_in_stream = Arc::clone(&abort_signals);
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(Arc::new(move |_model, _context, options| {
            let stream = create_assistant_message_event_stream();
            let task_stream = stream.clone();
            let started = Arc::clone(&started_in_stream);
            let task_signal = options.signal.clone();
            *signals_in_stream.lock().expect("signals") = task_signal.clone();
            tokio::spawn(async move {
                task_stream.push(AssistantMessageEvent::Start {
                    partial: assistant_message(""),
                });
                started.notify_waiters();
                if let Some(signal) = task_signal {
                    signal.cancelled().await;
                }
                let mut message = assistant_message("Aborted");
                message.stop_reason = StopReason::Aborted;
                message.error_message = Some("Request aborted by user".to_string());
                task_stream.push(AssistantMessageEvent::Error {
                    reason: StopReason::Aborted,
                    error: message,
                });
            });
            stream
        })),
        ..Default::default()
    })
    .expect("agent");

    let first_prompt = agent.prompt("First message");
    tokio::pin!(first_prompt);
    tokio::select! {
        () = stream_started.notified() => {}
        _ = &mut first_prompt => panic!("the stream must wait for the abort"),
    }
    assert!(agent.state().is_streaming);

    let error = agent
        .prompt("Second message")
        .await
        .expect_err("a second prompt while streaming must fail");
    assert_eq!(
        error.message(),
        "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion."
    );

    agent.abort();
    let _ = first_prompt.await;
}

#[tokio::test]
async fn rejects_continue_while_streaming() {
    let stream_started = deferred();
    let started_in_stream = Arc::clone(&stream_started);
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(Arc::new(move |_model, _context, options| {
            let stream = create_assistant_message_event_stream();
            let task_stream = stream.clone();
            let started = Arc::clone(&started_in_stream);
            let task_signal = options.signal.clone();
            tokio::spawn(async move {
                task_stream.push(AssistantMessageEvent::Start {
                    partial: assistant_message(""),
                });
                started.notify_waiters();
                if let Some(signal) = task_signal {
                    signal.cancelled().await;
                }
                let mut message = assistant_message("Aborted");
                message.stop_reason = StopReason::Aborted;
                message.error_message = Some("Request aborted by user".to_string());
                task_stream.push(AssistantMessageEvent::Error {
                    reason: StopReason::Aborted,
                    error: message,
                });
            });
            stream
        })),
        ..Default::default()
    })
    .expect("agent");

    let first_prompt = agent.prompt("First message");
    tokio::pin!(first_prompt);
    tokio::select! {
        () = stream_started.notified() => {}
        _ = &mut first_prompt => panic!("the stream must wait for the abort"),
    }
    assert!(agent.state().is_streaming);

    let error = agent
        .r#continue()
        .await
        .expect_err("continue while streaming must fail");
    assert_eq!(
        error.message(),
        "Agent is already processing. Wait for completion before continuing."
    );

    agent.abort();
    let _ = first_prompt.await;
}

#[tokio::test]
async fn continue_processes_queued_follow_up_messages_after_an_assistant_turn() {
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(Arc::new(|_model, _context, _options| {
            done_stream(assistant_message("Processed"), StopReason::Stop)
        })),
        ..Default::default()
    })
    .expect("agent");

    agent.set_messages(vec![
        AgentMessage::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![xjpcode_ai::UserContentBlock::Text(
                TextContent::new("Initial"),
            )]),
            timestamp: 0,
        }),
        AgentMessage::Assistant(assistant_message("Initial response")),
    ]);
    agent.follow_up(AgentMessage::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Blocks(vec![xjpcode_ai::UserContentBlock::Text(TextContent::new(
            "Queued follow-up",
        ))]),
        timestamp: 1,
    }));

    agent.r#continue().await.expect("continue");

    let messages = agent.state().messages;
    let has_queued_follow_up = messages.iter().any(|message| match message {
        AgentMessage::User(message) => match &message.content {
            UserContent::Text(text) => text == "Queued follow-up",
            UserContent::Blocks(blocks) => {
                xjpcode_ai::content_text(blocks, "") == "Queued follow-up"
            }
        },
        _ => false,
    });
    assert!(has_queued_follow_up);
    assert_eq!(messages.last().map(AgentMessage::role), Some("assistant"));
}

#[tokio::test]
async fn continue_keeps_queue_mode_steering_semantics_for_an_assistant_tail_fallback() {
    for (mode, expected_requests) in [
        (xjpcode_agent::QueueMode::OneAtATime, 2_usize),
        (xjpcode_agent::QueueMode::All, 1_usize),
    ] {
        let requests: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let requests_in_stream = Arc::clone(&requests);
        let agent = Agent::new(AgentOptions {
            steering_mode: Some(mode),
            stream_fn: Some(Arc::new(move |_model, context, _options| {
                requests_in_stream.lock().expect("requests").push(
                    context
                        .messages
                        .iter()
                        .filter_map(|message| match message {
                            xjpcode_ai::Message::User(message) => match &message.content {
                                UserContent::Text(text) => Some(text.clone()),
                                UserContent::Blocks(blocks) => {
                                    Some(xjpcode_ai::content_text(blocks, ""))
                                }
                            },
                            _ => None,
                        })
                        .collect(),
                );
                done_stream(assistant_message("Processed"), StopReason::Stop)
            })),
            ..Default::default()
        })
        .expect("agent");
        agent.set_messages(vec![
            user_message("Initial"),
            AgentMessage::Assistant(assistant_message("Initial response")),
        ]);
        agent.steer(user_message("Steering 1"));
        agent.steer(user_message("Steering 2"));

        agent.r#continue().await.expect("continue");

        let requests = requests.lock().expect("requests").clone();
        assert_eq!(requests.len(), expected_requests, "{mode:?}");
        assert!(requests[0].contains(&"Steering 1".to_string()));
        if mode == xjpcode_agent::QueueMode::OneAtATime {
            assert!(!requests[0].contains(&"Steering 2".to_string()));
            assert!(requests[1].contains(&"Steering 2".to_string()));
        } else {
            assert!(requests[0].contains(&"Steering 2".to_string()));
        }
    }
}

#[tokio::test]
async fn keeps_the_legacy_prepare_next_turn_signal_callback_behavior() {
    let tool = create_tool("noop");
    let request_count = Arc::new(AtomicUsize::new(0));
    let saw_abort_signal = Arc::new(AtomicUsize::new(0));
    let saw_in_hook = Arc::clone(&saw_abort_signal);
    let agent = Agent::new(AgentOptions {
        initial_state: Some(xjpcode_agent::AgentInitialState {
            tools: Some(vec![tool]),
            ..Default::default()
        }),
        prepare_next_turn: Some(Arc::new(move |signal: Option<&AbortSignal>| {
            let saw = saw_in_hook.clone();
            let present = signal.is_some();
            boxed(async move {
                if present {
                    saw.store(1, Ordering::SeqCst);
                }
                None
            }) as xjpcode_ai::BoxFuture<'_, Option<xjpcode_agent::AgentLoopTurnUpdate>>
        })),
        stream_fn: Some(Arc::new({
            let request_count = Arc::clone(&request_count);
            move |_model, _context, _options| {
                let request = request_count.fetch_add(1, Ordering::SeqCst) + 1;
                if request == 1 {
                    done_stream(
                        assistant_tool_use_message(vec![tool_call("tool-1", "noop", json!({}))]),
                        StopReason::ToolUse,
                    )
                } else {
                    done_stream(assistant_message("done"), StopReason::Stop)
                }
            }
        })),
        ..Default::default()
    })
    .expect("agent");

    let _ = agent.prompt("start").await;

    assert_eq!(request_count.load(Ordering::SeqCst), 2);
    assert_eq!(saw_abort_signal.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn forwards_finish_turn_through_agent_options_with_the_active_abort_signal() {
    let request_count = Arc::new(AtomicUsize::new(0));
    let saw_abort_signal = Arc::new(AtomicUsize::new(0));
    let callback_context_roles: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let saw_in_hook = Arc::clone(&saw_abort_signal);
    let roles_in_hook = Arc::clone(&callback_context_roles);
    let agent = Agent::new(AgentOptions {
        initial_state: Some(xjpcode_agent::AgentInitialState {
            tools: Some(vec![create_tool("noop")]),
            ..Default::default()
        }),
        finish_turn: Some(tools::finish_turn(move |context, signal| {
            if signal.is_some() {
                saw_in_hook.store(1, Ordering::SeqCst);
            }
            *roles_in_hook.lock().expect("roles") = context
                .context
                .messages
                .iter()
                .map(|message| message.role().to_string())
                .collect();
            boxed(async { Some(xjpcode_agent::AgentTurnDecision::End) })
        })),
        stream_fn: Some(Arc::new({
            let request_count = Arc::clone(&request_count);
            move |_model, _context, _options| {
                let request = request_count.fetch_add(1, Ordering::SeqCst) + 1;
                if request == 1 {
                    done_stream(
                        assistant_tool_use_message(vec![tool_call("tool-1", "noop", json!({}))]),
                        StopReason::ToolUse,
                    )
                } else {
                    done_stream(assistant_message("should not run"), StopReason::Stop)
                }
            }
        })),
        ..Default::default()
    })
    .expect("agent");

    let _ = agent.prompt("start").await;

    assert_eq!(request_count.load(Ordering::SeqCst), 1);
    assert_eq!(saw_abort_signal.load(Ordering::SeqCst), 1);
    assert_eq!(
        *callback_context_roles.lock().expect("roles"),
        vec![
            "system".to_string(),
            "user".to_string(),
            "assistant".to_string(),
            "toolResult".to_string()
        ]
    );
}

#[tokio::test]
async fn rejects_a_queued_continuation_from_an_empty_or_system_only_context_without_draining_queues(
) {
    for messages in [
        Vec::new(),
        vec![AgentMessage::System(SystemMessage {
            role: SystemRole::System,
            content: SystemContent::Text("system only".to_string()),
            sections: None,
            tools_added: None,
            tools_removed: None,
            timestamp: 1,
        })],
    ] {
        let agent = Agent::new(AgentOptions {
            initial_state: Some(xjpcode_agent::AgentInitialState {
                messages: Some(messages),
                ..Default::default()
            }),
            stream_fn: Some(unused_stream_function()),
            ..Default::default()
        })
        .expect("agent");
        let steering = user_message("steering");
        let follow_up = user_message("follow-up");
        agent.steer(steering.clone());
        agent.follow_up(follow_up.clone());

        let error = agent
            .r#continue()
            .await
            .expect_err("an empty context must not continue");
        assert_eq!(error.message(), "No messages to continue from");
        assert_eq!(agent.peek_queued_messages(), vec![steering.clone()]);
        agent.clear_steering_queue();
        assert_eq!(agent.peek_queued_messages(), vec![follow_up]);
    }
}

#[tokio::test]
async fn defers_follow_up_input_on_the_first_continuation_request() {
    let tool_result_message = ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "call-1".to_string(),
        tool_name: "noop".to_string(),
        content: vec![ToolResultContentBlock::Text(TextContent::new("done"))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 1,
        duration_ms: None,
    };
    let cases: Vec<Vec<AgentMessage>> = vec![
        vec![user_message("existing user")],
        vec![
            user_message("existing user"),
            AgentMessage::Assistant(assistant_tool_use_message(vec![tool_call(
                "call-1",
                "noop",
                json!({}),
            )])),
            AgentMessage::ToolResult(tool_result_message),
        ],
    ];
    for messages in cases {
        let requests: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let requests_in_stream = Arc::clone(&requests);
        let agent = Agent::new(AgentOptions {
            initial_state: Some(xjpcode_agent::AgentInitialState {
                messages: Some(messages),
                ..Default::default()
            }),
            stream_fn: Some(Arc::new(move |_model, context, _options| {
                requests_in_stream.lock().expect("requests").push(
                    context
                        .messages
                        .iter()
                        .filter_map(|message| match message {
                            xjpcode_ai::Message::User(message) => match &message.content {
                                UserContent::Text(text) => Some(text.clone()),
                                UserContent::Blocks(blocks) => {
                                    Some(xjpcode_ai::content_text(blocks, ""))
                                }
                            },
                            _ => None,
                        })
                        .collect(),
                );
                done_stream(assistant_message("done"), StopReason::Stop)
            })),
            ..Default::default()
        })
        .expect("agent");
        agent.follow_up(user_message("follow-up"));

        agent.r#continue().await.expect("continue");

        let requests = requests.lock().expect("requests").clone();
        assert_eq!(requests.len(), 2);
        assert!(!requests[0].contains(&"follow-up".to_string()));
        assert!(requests[1].contains(&"follow-up".to_string()));
    }
}

#[tokio::test]
async fn polls_queue_mode_steering_at_continuation_startup() {
    for (mode, expected_requests) in [
        (xjpcode_agent::QueueMode::OneAtATime, 2_usize),
        (xjpcode_agent::QueueMode::All, 1_usize),
    ] {
        let requests: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let requests_in_stream = Arc::clone(&requests);
        let agent = Agent::new(AgentOptions {
            initial_state: Some(xjpcode_agent::AgentInitialState {
                messages: Some(vec![user_message("existing")]),
                ..Default::default()
            }),
            steering_mode: Some(mode),
            stream_fn: Some(Arc::new(move |_model, context, _options| {
                requests_in_stream.lock().expect("requests").push(
                    context
                        .messages
                        .iter()
                        .filter_map(|message| match message {
                            xjpcode_ai::Message::User(message) => match &message.content {
                                UserContent::Text(text) => Some(text.clone()),
                                UserContent::Blocks(blocks) => {
                                    Some(xjpcode_ai::content_text(blocks, ""))
                                }
                            },
                            _ => None,
                        })
                        .collect(),
                );
                done_stream(assistant_message("done"), StopReason::Stop)
            })),
            ..Default::default()
        })
        .expect("agent");
        agent.steer(user_message("first"));
        agent.steer(user_message("second"));

        agent.r#continue().await.expect("continue");

        let requests = requests.lock().expect("requests").clone();
        assert_eq!(requests.len(), expected_requests);
        assert!(requests[0].contains(&"first".to_string()));
        if mode == xjpcode_agent::QueueMode::OneAtATime {
            assert!(!requests[0].contains(&"second".to_string()));
            assert!(requests[1].contains(&"second".to_string()));
        } else {
            assert!(requests[0].contains(&"second".to_string()));
        }
    }
}

#[tokio::test]
async fn keeps_steering_ahead_of_follow_up_from_a_non_assistant_tail() {
    let requests: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let requests_in_stream = Arc::clone(&requests);
    let agent = Agent::new(AgentOptions {
        initial_state: Some(xjpcode_agent::AgentInitialState {
            messages: Some(vec![user_message("existing")]),
            ..Default::default()
        }),
        stream_fn: Some(Arc::new(move |_model, context, _options| {
            requests_in_stream.lock().expect("requests").push(
                context
                    .messages
                    .iter()
                    .filter_map(|message| match message {
                        xjpcode_ai::Message::User(message) => match &message.content {
                            UserContent::Text(text) => Some(text.clone()),
                            UserContent::Blocks(blocks) => {
                                Some(xjpcode_ai::content_text(blocks, ""))
                            }
                        },
                        _ => None,
                    })
                    .collect(),
            );
            done_stream(assistant_message("done"), StopReason::Stop)
        })),
        ..Default::default()
    })
    .expect("agent");
    agent.steer(user_message("steering"));
    agent.follow_up(user_message("follow-up"));

    agent.r#continue().await.expect("continue");

    let requests = requests.lock().expect("requests").clone();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains(&"steering".to_string()));
    assert!(!requests[0].contains(&"follow-up".to_string()));
    assert!(requests[1].contains(&"follow-up".to_string()));
}

#[tokio::test]
async fn keeps_queues_on_an_error_or_aborted_response_even_when_finish_turn_requests_continuation()
{
    for stop_reason in [StopReason::Error, StopReason::Aborted] {
        let follow_up = user_message("follow-up");
        let agent = Agent::new(AgentOptions {
            finish_turn: Some(tools::finish_turn(|_context, _signal| {
                boxed(async { Some(xjpcode_agent::AgentTurnDecision::Continue) })
            })),
            stream_fn: Some(Arc::new(move |_model, _context, _options| {
                let mut message = assistant_message(&format!("{stop_reason:?}").to_lowercase());
                message.stop_reason = stop_reason;
                message.error_message = Some(format!("{stop_reason:?}").to_lowercase());
                let stream = create_assistant_message_event_stream();
                stream.push(AssistantMessageEvent::Error {
                    reason: stop_reason,
                    error: message,
                });
                stream
            })),
            ..Default::default()
        })
        .expect("agent");
        agent.follow_up(follow_up.clone());
        let agent_in_listener = agent.clone();
        let _subscription = agent.subscribe(move |event, _signal| {
            let agent = agent_in_listener.clone();
            let is_assistant_end = matches!(
                event,
                AgentEvent::MessageEnd {
                    message: AgentMessage::Assistant(_)
                }
            );
            boxed(async move {
                if is_assistant_end {
                    agent.steer(user_message("steering"));
                }
            })
        });

        let _ = agent.prompt("start").await;

        let queued = agent.peek_queued_messages();
        assert_eq!(queued.len(), 1);
        assert_eq!(agent.state().messages.len(), 2);
        agent.clear_steering_queue();
        assert_eq!(agent.peek_queued_messages(), vec![follow_up]);
    }
}

#[tokio::test]
async fn keeps_queues_when_finish_turn_ends_the_run() {
    let follow_up = user_message("follow-up");
    let agent = Agent::new(AgentOptions {
        finish_turn: Some(tools::finish_turn(|_context, _signal| {
            boxed(async { Some(xjpcode_agent::AgentTurnDecision::End) })
        })),
        stream_fn: Some(Arc::new(|_model, _context, _options| {
            done_stream(assistant_message("done"), StopReason::Stop)
        })),
        ..Default::default()
    })
    .expect("agent");
    agent.follow_up(follow_up.clone());
    let agent_in_listener = agent.clone();
    let _subscription = agent.subscribe(move |event, _signal| {
        let agent = agent_in_listener.clone();
        let is_assistant_end = matches!(
            event,
            AgentEvent::MessageEnd {
                message: AgentMessage::Assistant(_)
            }
        );
        boxed(async move {
            if is_assistant_end {
                agent.steer(user_message("steering"));
            }
        })
    });

    let _ = agent.prompt("start").await;

    let queued = agent.peek_queued_messages();
    assert_eq!(queued.len(), 1);
    agent.clear_steering_queue();
    assert_eq!(agent.peek_queued_messages(), vec![follow_up]);
}

#[tokio::test]
async fn previews_the_next_selected_queued_messages_without_consuming_them() {
    let agent = Agent::new(AgentOptions {
        steering_mode: Some(xjpcode_agent::QueueMode::OneAtATime),
        follow_up_mode: Some(xjpcode_agent::QueueMode::All),
        stream_fn: Some(Arc::new(|_model, _context, _options| {
            create_assistant_message_event_stream()
        })),
        ..Default::default()
    })
    .expect("agent");
    let first = user_message("first steering");
    let second = user_message("second steering");
    let follow_up = user_message("follow-up");
    agent.steer(first.clone());
    agent.steer(second);
    agent.follow_up(follow_up.clone());

    assert_eq!(agent.peek_queued_messages(), vec![first.clone()]);
    assert_eq!(agent.peek_queued_messages(), vec![first]);
    agent.clear_steering_queue();
    assert_eq!(agent.peek_queued_messages(), vec![follow_up]);
}

#[tokio::test]
async fn forwards_provider_stream_event_observers_through_agent_options() {
    let provider_events: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let events_in_callback = Arc::clone(&provider_events);
    let agent = Agent::new(AgentOptions {
        on_provider_stream_event: xjpcode_ai::Callback::new(Arc::new(
            move |data: &Value, _model: &Model| {
                let events = Arc::clone(&events_in_callback);
                let data = data.clone();
                boxed(async move {
                    events.lock().expect("provider events").push(data);
                }) as xjpcode_ai::BoxFuture<'_, ()>
            },
        )),
        stream_fn: Some(Arc::new(|_model, _context, options| {
            let stream = create_assistant_message_event_stream();
            let task_stream = stream.clone();
            let observer = options.options.on_provider_stream_event.arc();
            let model = _model.clone();
            tokio::spawn(async move {
                if let Some(observer) = observer {
                    observer(&json!({ "request_cost": 0.01 }), &model).await;
                }
                task_stream.push(AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: assistant_message("ok"),
                });
            });
            stream
        })),
        ..Default::default()
    })
    .expect("agent");

    let _ = agent.prompt("hello").await;

    let events = provider_events.lock().expect("provider events").clone();
    assert_eq!(events, vec![json!({ "request_cost": 0.01 })]);
}

#[tokio::test]
async fn forwards_session_id_to_the_stream_function_options() {
    let received_session_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let received_in_stream = Arc::clone(&received_session_id);
    let agent = Agent::new(AgentOptions {
        session_id: Some("session-abc".to_string()),
        stream_fn: Some(Arc::new(move |_model, _context, options| {
            *received_in_stream.lock().expect("session id") = options.options.session_id.clone();
            done_stream(assistant_message("ok"), StopReason::Stop)
        })),
        ..Default::default()
    })
    .expect("agent");

    let _ = agent.prompt("hello").await;
    assert_eq!(
        *received_session_id.lock().expect("session id"),
        Some("session-abc".to_string())
    );

    agent.set_session_id(Some("session-def".to_string()));
    assert_eq!(agent.session_id(), Some("session-def".to_string()));

    let _ = agent.prompt("hello again").await;
    assert_eq!(
        *received_session_id.lock().expect("session id"),
        Some("session-def".to_string())
    );
}

#[tokio::test]
async fn attributes_requests_to_pi_by_default_and_merges_a_configured_identity_over_it() {
    let received: Arc<Mutex<Vec<Option<ClientIdentity>>>> = Arc::new(Mutex::new(Vec::new()));
    let received_in_stream = Arc::clone(&received);
    let agent = Agent::new(AgentOptions {
        stream_fn: Some(Arc::new(move |_model, _context, options| {
            received_in_stream
                .lock()
                .expect("identities")
                .push(options.options.identity.clone());
            done_stream(assistant_message("ok"), StopReason::Stop)
        })),
        ..Default::default()
    })
    .expect("agent");

    let _ = agent.prompt("hello").await;
    let acme = ClientIdentity {
        user_agent: None,
        attribution: Some(Some(xjpcode_ai::ClientAttribution {
            name: "acme".to_string(),
            url: "https://acme.example".to_string(),
            categories: None,
        })),
        version: None,
    };
    agent.set_identity(Some(ClientIdentity {
        user_agent: Some(xjpcode_ai::UserAgentMode::Short),
        attribution: acme.attribution.clone(),
        version: None,
    }));
    let _ = agent.prompt("hello again").await;
    agent.set_identity(Some(ClientIdentity {
        user_agent: Some(xjpcode_ai::UserAgentMode::Short),
        attribution: None,
        version: None,
    }));
    let _ = agent.prompt("and again").await;

    let received = received.lock().expect("identities").clone();
    assert_eq!(received.len(), 3);
    assert_eq!(
        received[0]
            .as_ref()
            .and_then(|identity| identity.attribution.clone()),
        Some(Some(PI_ATTRIBUTION.clone()))
    );
    assert_eq!(
        received[1]
            .as_ref()
            .and_then(|identity| identity.attribution.clone()),
        acme.attribution.clone()
    );
    assert_eq!(
        received[1]
            .as_ref()
            .and_then(|identity| identity.user_agent.clone()),
        Some(xjpcode_ai::UserAgentMode::Short)
    );
    assert_eq!(
        received[2]
            .as_ref()
            .and_then(|identity| identity.attribution.clone()),
        Some(Some(PI_ATTRIBUTION.clone()))
    );
    assert_eq!(
        received[2]
            .as_ref()
            .and_then(|identity| identity.user_agent.clone()),
        Some(xjpcode_ai::UserAgentMode::Short)
    );
}

/// 未用的导入兜底：`ToolExecutionMode` 与 `CustomAgentMessage` 在别的测试里构造消息时用。
const _: fn(ToolExecutionMode, CustomAgentMessage, TextContent) = |_, _, _| {};
