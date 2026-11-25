//! 移植自上游 `packages/agent/test/e2e.test.ts`。
//!
//! 上游这里用 pi-ai 的 `registerFauxProvider` + `streamSimple`：模型目录与 provider 适配是后面
//! 那一步的事。这里换成测试自己的假流函数（同样的分块输出、同样的 abort 语义），断言与用例逐条
//! 照旧；provider 层自己的分块规则不在本文件范围内（见 `PORTING.md` 差异表 #75）。

#[path = "utils/calculate.rs"]
mod calculate;
#[path = "utils/get_current_time.rs"]
mod get_current_time;
#[path = "utils/tools.rs"]
mod tools;
#[path = "utils/wait_for_tick.rs"]
mod wait_for_tick;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use xjpcode_agent::{Agent, AgentEvent, AgentMessage, AgentOptions, StreamFn, ThinkingLevel};
use xjpcode_ai::providers::model_schema::{ModelCost, ModelInputModality};
use xjpcode_ai::{
    create_assistant_message_event_stream, AbortSignal, AssistantContentBlock, AssistantMessage,
    AssistantMessageEvent, AssistantMessageEventStream, JsonObject, Model, StopReason, TextContent,
    ThinkingContent, ToolCall, ToolResultContentBlock, UserContent, UserMessage, UserRole,
};

use tools::text_block;

fn model(id: &str, reasoning: bool) -> Model {
    Model {
        id: id.to_string(),
        name: id.to_string(),
        api: "faux".to_string(),
        provider: "faux".to_string(),
        base_url: "https://faux.invalid".to_string(),
        input: vec![ModelInputModality::Text],
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        r#type: None,
        reasoning,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 8192,
        max_tokens: 2048,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

/// 一条假响应：思考块、文本块、工具调用各一份，按上游 `fauxAssistantMessage` 的写法给。
#[derive(Clone)]
struct FauxResponse {
    thinking: Option<String>,
    text: String,
    tool_calls: Vec<ToolCall>,
    stop_reason: StopReason,
}

impl FauxResponse {
    fn text(text: &str) -> Self {
        Self {
            thinking: None,
            text: text.to_string(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::Stop,
        }
    }
}

/// 上游 `registerFauxProvider` 的替身：一条响应队列 + 一个流函数。
#[derive(Clone)]
struct FauxProvider {
    responses:
        Arc<Mutex<VecDeque<Box<dyn Fn(&[xjpcode_ai::Message]) -> FauxResponse + Send + Sync>>>>,
    model: Model,
    tokens_per_second: u64,
    token_size: usize,
}

impl FauxProvider {
    fn new(id: &str) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::new())),
            model: model(id, false),
            tokens_per_second: 2_000,
            token_size: 1,
        }
    }

    fn reasoning(mut self) -> Self {
        self.model = model("faux-reasoning", true);
        self
    }

    fn slow(mut self, tokens_per_second: u64, token_size: usize) -> Self {
        self.tokens_per_second = tokens_per_second;
        self.token_size = token_size;
        self
    }

    fn respond_with(self, response: FauxResponse) -> Self {
        self.responses
            .lock()
            .expect("responses")
            .push_back(Box::new(move |_context| response.clone()));
        self
    }

    fn respond_dynamically(
        self,
        respond: impl Fn(&[xjpcode_ai::Message]) -> FauxResponse + Send + Sync + 'static,
    ) -> Self {
        self.responses
            .lock()
            .expect("responses")
            .push_back(Box::new(respond));
        self
    }

    fn get_model(&self) -> Model {
        self.model.clone()
    }

    /// 上游 `streamSimple` 的替身。
    fn stream_fn(&self) -> StreamFn {
        let responses = Arc::clone(&self.responses);
        let model = self.model.clone();
        let tokens_per_second = self.tokens_per_second;
        let token_size = self.token_size;
        Arc::new(move |_model, context, options| {
            let respond = responses
                .lock()
                .expect("responses")
                .pop_front()
                .expect("a queued faux response");
            let response = respond(&context.messages);
            stream_response(
                model.clone(),
                response,
                tokens_per_second,
                token_size,
                options.signal.clone(),
            )
        })
    }
}

/// 把一条假响应按 token 流出去；`signal` 取消时改发 `error`。
fn stream_response(
    model: Model,
    response: FauxResponse,
    tokens_per_second: u64,
    token_size: usize,
    signal: Option<AbortSignal>,
) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let task_stream = stream.clone();
    tokio::spawn(async move {
        let mut partial = xjpcode_agent::empty_assistant_message("faux", "faux", model.id.clone());
        partial.usage = xjpcode_agent::empty_usage();
        let token_delay = Duration::from_micros(1_000_000 / tokens_per_second.max(1));
        task_stream.push(AssistantMessageEvent::Start {
            partial: partial.clone(),
        });

        let mut content_index = 0;
        if let Some(thinking) = &response.thinking {
            let block = AssistantContentBlock::Thinking(ThinkingContent {
                r#type: xjpcode_ai::ThinkingContentType::Thinking,
                thinking: String::new(),
                thinking_signature: None,
                redacted: None,
            });
            set_block(&mut partial, content_index, block.clone());
            task_stream.push(AssistantMessageEvent::ThinkingStart {
                content_index,
                partial: partial.clone(),
            });
            for token in tokens(thinking, token_size) {
                if aborted_then_error(&task_stream, &partial, &signal) {
                    return;
                }
                match partial.content.get_mut(content_index) {
                    Some(AssistantContentBlock::Thinking(content)) => {
                        content.thinking.push_str(&token)
                    }
                    _ => unreachable!("thinking block"),
                }
                task_stream.push(AssistantMessageEvent::ThinkingDelta {
                    content_index,
                    delta: token,
                    partial: partial.clone(),
                });
                tokio::time::sleep(token_delay).await;
            }
            let thinking_text = match partial.content.get(content_index) {
                Some(AssistantContentBlock::Thinking(content)) => content.thinking.clone(),
                _ => String::new(),
            };
            task_stream.push(AssistantMessageEvent::ThinkingEnd {
                content_index,
                content: thinking_text,
                partial: partial.clone(),
            });
            content_index += 1;
        }

        if !response.text.is_empty() {
            set_block(
                &mut partial,
                content_index,
                AssistantContentBlock::Text(TextContent::new("")),
            );
            task_stream.push(AssistantMessageEvent::TextStart {
                content_index,
                partial: partial.clone(),
            });
            for token in tokens(&response.text, token_size) {
                if aborted_then_error(&task_stream, &partial, &signal) {
                    return;
                }
                match partial.content.get_mut(content_index) {
                    Some(AssistantContentBlock::Text(content)) => content.text.push_str(&token),
                    _ => unreachable!("text block"),
                }
                task_stream.push(AssistantMessageEvent::TextDelta {
                    content_index,
                    delta: token,
                    partial: partial.clone(),
                });
                tokio::time::sleep(token_delay).await;
            }
            let text = match partial.content.get(content_index) {
                Some(AssistantContentBlock::Text(content)) => content.text.clone(),
                _ => String::new(),
            };
            task_stream.push(AssistantMessageEvent::TextEnd {
                content_index,
                content: text,
                partial: partial.clone(),
            });
            content_index += 1;
        }

        for tool_call in &response.tool_calls {
            set_block(
                &mut partial,
                content_index,
                AssistantContentBlock::ToolCall(ToolCall::new(
                    &tool_call.id,
                    &tool_call.name,
                    JsonObject::new(),
                )),
            );
            task_stream.push(AssistantMessageEvent::ToolcallStart {
                content_index,
                partial: partial.clone(),
            });
            let arguments = serde_json::to_string(&tool_call.arguments).unwrap_or_default();
            let split = arguments.len() / 2;
            for delta in [
                arguments[..split].to_string(),
                arguments[split..].to_string(),
            ] {
                if let Some(AssistantContentBlock::ToolCall(content)) =
                    partial.content.get_mut(content_index)
                {
                    content.arguments = serde_json::from_str(&arguments).unwrap_or_default();
                }
                task_stream.push(AssistantMessageEvent::ToolcallDelta {
                    content_index,
                    delta,
                    partial: partial.clone(),
                });
            }
            set_block(
                &mut partial,
                content_index,
                AssistantContentBlock::ToolCall(tool_call.clone()),
            );
            task_stream.push(AssistantMessageEvent::ToolcallEnd {
                content_index,
                tool_call: tool_call.clone(),
                partial: partial.clone(),
            });
            content_index += 1;
        }

        if aborted_then_error(&task_stream, &partial, &signal) {
            return;
        }
        partial.stop_reason = response.stop_reason;
        task_stream.push(AssistantMessageEvent::Done {
            reason: response.stop_reason,
            message: partial,
        });
    });
    stream
}

fn set_block(partial: &mut AssistantMessage, index: usize, block: AssistantContentBlock) {
    while partial.content.len() <= index {
        partial
            .content
            .push(AssistantContentBlock::Text(TextContent::new("")));
    }
    partial.content[index] = block;
}

/// 已经取消就发 `error`，返回 `true`。
fn aborted_then_error(
    stream: &AssistantMessageEventStream,
    partial: &AssistantMessage,
    signal: &Option<AbortSignal>,
) -> bool {
    if !signal.as_ref().is_some_and(AbortSignal::aborted) {
        return false;
    }
    let mut message = partial.clone();
    message.stop_reason = StopReason::Aborted;
    message.error_message = Some("Request aborted by user".to_string());
    stream.push(AssistantMessageEvent::Error {
        reason: StopReason::Aborted,
        error: message,
    });
    true
}

fn tokens(text: &str, size: usize) -> Vec<String> {
    let characters: Vec<char> = text.chars().collect();
    characters
        .chunks(size.max(1))
        .map(|chunk| chunk.iter().collect())
        .collect()
}

fn text_of(message: &AgentMessage) -> String {
    match message {
        AgentMessage::Assistant(message) => xjpcode_ai::content_text(&message.content, "\n"),
        AgentMessage::ToolResult(message) => xjpcode_ai::content_text(&message.content, "\n"),
        AgentMessage::User(message) => match &message.content {
            UserContent::Text(text) => text.clone(),
            UserContent::Blocks(blocks) => xjpcode_ai::content_text(blocks, "\n"),
        },
        AgentMessage::System(message) => xjpcode_ai::content_text(&message.content, "\n"),
        AgentMessage::Custom(message) => message
            .payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    }
}

fn user_message(text: &str) -> AgentMessage {
    AgentMessage::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp: 0,
    })
}

fn base_options(
    stream_fn: StreamFn,
    system_prompt: &str,
    model: Model,
    tools: Vec<Arc<dyn xjpcode_agent::AgentTool>>,
) -> AgentOptions {
    AgentOptions {
        stream_fn: Some(stream_fn),
        initial_state: Some(xjpcode_agent::AgentInitialState {
            system_prompt: Some(system_prompt.to_string()),
            model: Some(model),
            thinking_level: Some(ThinkingLevel::Off),
            tools: Some(tools),
            messages: None,
        }),
        ..Default::default()
    }
}

#[tokio::test]
async fn handles_a_basic_text_prompt() {
    let faux = FauxProvider::new("faux").respond_with(FauxResponse::text("4"));
    let agent = Agent::new(base_options(
        faux.stream_fn(),
        "You are a helpful assistant. Keep your responses concise.",
        faux.get_model(),
        Vec::new(),
    ))
    .expect("agent");

    let _ = agent
        .prompt("What is 2+2? Answer with just the number.")
        .await;

    let state = agent.state();
    assert!(!state.is_streaming);
    assert_eq!(state.messages.len(), 3);
    assert_eq!(state.messages[0].role(), "system");
    assert_eq!(state.messages[1].role(), "user");
    assert_eq!(state.messages[2].role(), "assistant");
    assert!(text_of(&state.messages[2]).contains('4'));
}

#[tokio::test]
async fn executes_tools_and_tracks_pending_tool_calls() {
    let mut tool_response = FauxResponse::text("Let me calculate that.");
    tool_response.tool_calls = vec![ToolCall::new(
        "calc-1",
        "calculate",
        json!({ "expression": "123 * 456" })
            .as_object()
            .expect("object")
            .clone(),
    )];
    tool_response.stop_reason = StopReason::ToolUse;
    let faux = FauxProvider::new("faux")
        .respond_with(tool_response)
        .respond_with(FauxResponse::text("The result is 56088."));
    let agent = Agent::new(base_options(
        faux.stream_fn(),
        "You are a helpful assistant. Always use the calculator tool for math.",
        faux.get_model(),
        vec![Arc::new(calculate::calculate_tool())],
    ))
    .expect("agent");

    let pending_during_events: Arc<Mutex<Vec<(&'static str, Vec<String>)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let pending_in_listener = Arc::clone(&pending_during_events);
    let agent_in_listener = agent.clone();
    let _subscription = agent.subscribe(move |event, _signal| {
        let agent = agent_in_listener.clone();
        let pending_in_listener = Arc::clone(&pending_in_listener);
        let kind = match event {
            AgentEvent::ToolExecutionStart { .. } => Some("tool_execution_start"),
            AgentEvent::ToolExecutionEnd { .. } => Some("tool_execution_end"),
            _ => None,
        };
        let ids: Vec<String> = match kind {
            Some("tool_execution_start" | "tool_execution_end") => {
                agent.state().pending_tool_calls.iter().cloned().collect()
            }
            _ => Vec::new(),
        };
        tools::boxed(async move {
            if let Some(kind) = kind {
                pending_in_listener
                    .lock()
                    .expect("pending")
                    .push((kind, ids));
            }
        })
    });

    let _ = agent
        .prompt("Calculate 123 * 456 using the calculator tool.")
        .await;

    let state = agent.state();
    assert!(!state.is_streaming);
    assert!(state.messages.len() >= 4);
    let tool_result = state
        .messages
        .iter()
        .find(|message| message.role() == "toolResult")
        .expect("tool result message");
    assert!(text_of(tool_result).contains("123 * 456 = 56088"));

    let final_message = state.messages.last().expect("last message");
    assert_eq!(final_message.role(), "assistant");
    assert!(text_of(final_message).contains("56088"));
    assert!(state.pending_tool_calls.is_empty());
    assert_eq!(
        *pending_during_events.lock().expect("pending"),
        vec![
            ("tool_execution_start", vec!["calc-1".to_string()]),
            ("tool_execution_end", vec![])
        ]
    );
}

#[tokio::test]
async fn handles_abort_during_streaming() {
    let faux = FauxProvider::new("faux")
        .slow(20, 2)
        .respond_with(FauxResponse::text(
        "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen",
    ));
    let agent = Agent::new(base_options(
        faux.stream_fn(),
        "You are a helpful assistant.",
        faux.get_model(),
        Vec::new(),
    ))
    .expect("agent");

    let prompt = agent.prompt("Count slowly from 1 to 20.");
    tokio::pin!(prompt);
    let abort_after_start = async {
        // 等 run 起来再按上游那样等 30ms。
        for _ in 0..1_000 {
            if agent.state().is_streaming {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        agent.abort();
    };
    let (prompt_result, ()) = tokio::join!(&mut prompt, abort_after_start);
    let _ = prompt_result;

    let state = agent.state();
    assert!(!state.is_streaming);
    assert!(state.messages.len() >= 2);
    let last = state.messages.last().expect("last message");
    match last {
        AgentMessage::Assistant(message) => {
            assert_eq!(message.stop_reason, StopReason::Aborted);
            assert!(message.error_message.is_some());
            assert_eq!(state.error_message, message.error_message);
        }
        other => panic!("expected an assistant message, got {:?}", other.role()),
    }
}

#[tokio::test]
async fn emits_lifecycle_updates_while_streaming() {
    let faux = FauxProvider::new("faux")
        .slow(2_000, 1)
        .respond_with(FauxResponse::text("1 2 3 4 5"));
    let agent = Agent::new(base_options(
        faux.stream_fn(),
        "You are a helpful assistant.",
        faux.get_model(),
        Vec::new(),
    ))
    .expect("agent");

    let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let events_in_listener = Arc::clone(&events);
    let _subscription = agent.subscribe(move |event, _signal| {
        events_in_listener
            .lock()
            .expect("events")
            .push(event.r#type());
        tools::boxed(async {})
    });

    let _ = agent.prompt("Count from 1 to 5.").await;

    let events = events.lock().expect("events").clone();
    for expected in [
        "agent_start",
        "turn_start",
        "message_start",
        "message_update",
        "message_end",
        "turn_end",
        "agent_end",
    ] {
        assert!(
            events.contains(&expected),
            "missing {expected} in {events:?}"
        );
    }
    let index = |name: &str| events.iter().position(|event| *event == name).expect(name);
    assert!(index("agent_start") < index("message_start"));
    assert!(index("message_start") < index("message_end"));
    assert!(
        index("message_end")
            < events
                .iter()
                .rposition(|event| *event == "agent_end")
                .expect("agent_end")
    );

    let state = agent.state();
    assert!(!state.is_streaming);
    assert_eq!(state.messages.len(), 3);
}

#[tokio::test]
async fn maintains_context_across_multiple_turns() {
    let faux = FauxProvider::new("faux")
        .respond_with(FauxResponse::text("Nice to meet you, Alice."))
        .respond_dynamically(|context| {
            let has_alice = context.iter().any(|message| match message {
                xjpcode_ai::Message::User(message) => match &message.content {
                    UserContent::Text(text) => text.contains("Alice"),
                    UserContent::Blocks(blocks) => {
                        xjpcode_ai::content_text(blocks, "").contains("Alice")
                    }
                },
                _ => false,
            });
            FauxResponse::text(if has_alice {
                "Your name is Alice."
            } else {
                "I do not know your name."
            })
        });
    let agent = Agent::new(base_options(
        faux.stream_fn(),
        "You are a helpful assistant.",
        faux.get_model(),
        Vec::new(),
    ))
    .expect("agent");

    let _ = agent.prompt("My name is Alice.").await;
    assert_eq!(agent.state().messages.len(), 3);

    let _ = agent.prompt("What is my name?").await;
    let state = agent.state();
    assert_eq!(state.messages.len(), 5);
    let last = state.messages.last().expect("last message");
    assert!(text_of(last).to_lowercase().contains("alice"));
}

#[tokio::test]
async fn preserves_thinking_content_blocks() {
    let mut response = FauxResponse::text("4");
    response.thinking = Some("step by step".to_string());
    let faux = FauxProvider::new("faux").reasoning().respond_with(response);
    let mut options = base_options(
        faux.stream_fn(),
        "You are a helpful assistant.",
        faux.get_model(),
        Vec::new(),
    );
    if let Some(initial_state) = options.initial_state.as_mut() {
        initial_state.thinking_level = Some(ThinkingLevel::Low);
    }
    let agent = Agent::new(options).expect("agent");

    let _ = agent.prompt("What is 2+2?").await;

    let state = agent.state();
    match state.messages.get(2) {
        Some(AgentMessage::Assistant(message)) => {
            assert_eq!(
                message.content,
                vec![
                    AssistantContentBlock::Thinking(ThinkingContent {
                        r#type: xjpcode_ai::ThinkingContentType::Thinking,
                        thinking: "step by step".to_string(),
                        thinking_signature: None,
                        redacted: None,
                    }),
                    AssistantContentBlock::Text(TextContent::new("4")),
                ]
            );
        }
        other => panic!(
            "expected an assistant message, got {:?}",
            other.map(AgentMessage::role)
        ),
    }
}

#[tokio::test]
async fn continue_throws_when_no_messages_in_context() {
    let faux = FauxProvider::new("faux");
    let agent = Agent::new(base_options(
        faux.stream_fn(),
        "Test",
        faux.get_model(),
        Vec::new(),
    ))
    .expect("agent");

    let error = agent
        .r#continue()
        .await
        .expect_err("a system-only context must not continue");
    assert_eq!(error.message(), "No messages to continue from");
}

#[tokio::test]
async fn continue_throws_when_the_last_message_is_assistant() {
    let faux = FauxProvider::new("faux");
    let model = faux.get_model();
    let agent = Agent::new(base_options(
        faux.stream_fn(),
        "Test",
        model.clone(),
        Vec::new(),
    ))
    .expect("agent");

    let mut assistant = xjpcode_agent::empty_assistant_message("faux", "faux", "faux-model");
    assistant.content = vec![AssistantContentBlock::Text(TextContent::new("Hello"))];
    assistant.stop_reason = StopReason::Stop;
    agent.set_messages(vec![AgentMessage::Assistant(assistant)]);

    let error = agent
        .r#continue()
        .await
        .expect_err("an assistant tail must not continue");
    assert_eq!(
        error.message(),
        "Cannot continue from message role: assistant"
    );
}

#[tokio::test]
async fn continue_from_a_user_message() {
    let faux = FauxProvider::new("faux").respond_with(FauxResponse::text("HELLO WORLD"));
    let agent = Agent::new(base_options(
        faux.stream_fn(),
        "You are a helpful assistant. Follow instructions exactly.",
        faux.get_model(),
        Vec::new(),
    ))
    .expect("agent");

    agent.set_messages(vec![user_message("Say exactly: HELLO WORLD")]);
    agent.r#continue().await.expect("continue");

    let state = agent.state();
    assert!(!state.is_streaming);
    assert_eq!(state.messages.len(), 2);
    assert_eq!(state.messages[0].role(), "user");
    assert_eq!(state.messages[1].role(), "assistant");
    assert!(text_of(&state.messages[1])
        .to_uppercase()
        .contains("HELLO WORLD"));
}

#[tokio::test]
async fn continue_from_a_tool_result() {
    let faux = FauxProvider::new("faux").respond_with(FauxResponse::text("The answer is 8."));
    let model = faux.get_model();
    let agent = Agent::new(base_options(
        faux.stream_fn(),
        "You are a helpful assistant. After getting a calculation result, state the answer clearly.",
        model.clone(),
        vec![Arc::new(calculate::calculate_tool())],
    ))
    .expect("agent");

    let mut assistant = xjpcode_agent::empty_assistant_message("faux", "faux", &model.id);
    assistant.content = vec![
        AssistantContentBlock::Text(TextContent::new("Let me calculate that.")),
        AssistantContentBlock::ToolCall(ToolCall::new(
            "calc-1",
            "calculate",
            json!({ "expression": "5 + 3" })
                .as_object()
                .expect("object")
                .clone(),
        )),
    ];
    assistant.stop_reason = StopReason::ToolUse;
    let tool_result = xjpcode_ai::ToolResultMessage {
        role: xjpcode_ai::ToolResultRole::ToolResult,
        tool_call_id: "calc-1".to_string(),
        tool_name: "calculate".to_string(),
        content: vec![ToolResultContentBlock::Text(TextContent::new("5 + 3 = 8"))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 1,
        duration_ms: None,
    };
    agent.set_messages(vec![
        user_message("What is 5 + 3?"),
        AgentMessage::Assistant(assistant),
        AgentMessage::ToolResult(tool_result),
    ]);

    agent.r#continue().await.expect("continue");

    let state = agent.state();
    assert!(!state.is_streaming);
    assert!(state.messages.len() >= 4);
    let last = state.messages.last().expect("last message");
    assert_eq!(last.role(), "assistant");
    assert!(text_of(last).contains('8'));
}

/// 未用的导入兜底：`text_block` 与 `wait_for_tick` 由别的用例/helper 文件用。
const _: fn() = || {
    let _ = text_block("x");
};
