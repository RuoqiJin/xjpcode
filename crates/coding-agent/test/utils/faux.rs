#![allow(dead_code)]
//! 假模型：一条响应队列 + 一个流函数，照上游 `registerFauxProvider` 的用法简化。
//! `test/utils/` 不是测试目标，由用到的测试按模块引入。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use xjpcode_agent::{empty_assistant_message, empty_usage, AgentMessage, StreamFn};
use xjpcode_ai::providers::model_schema::ModelCost;
use xjpcode_ai::{
    create_assistant_message_event_stream, AssistantContentBlock, AssistantMessage,
    AssistantMessageEvent, JsonObject, Model, StopReason, TextContent, ToolCall, Usage, UsageCost,
};

/// 一条假响应。
#[derive(Clone)]
pub struct FauxResponse {
    pub content: Vec<AssistantContentBlock>,
    pub stop_reason: StopReason,
    pub error_message: Option<String>,
    pub usage: Usage,
    /// 推 `done` 之前等多少毫秒（给「跑到一半插话」的用例）。
    pub delay_ms: u64,
}

impl FauxResponse {
    pub fn text(text: &str) -> Self {
        Self {
            content: vec![AssistantContentBlock::Text(TextContent::new(text))],
            stop_reason: StopReason::Stop,
            error_message: None,
            usage: usage(100, 20),
            delay_ms: 0,
        }
    }

    pub fn delayed(mut self, delay_ms: u64) -> Self {
        self.delay_ms = delay_ms;
        self
    }

    pub fn text_with_usage(text: &str, usage: Usage) -> Self {
        Self {
            content: vec![AssistantContentBlock::Text(TextContent::new(text))],
            stop_reason: StopReason::Stop,
            error_message: None,
            usage,
            delay_ms: 0,
        }
    }

    pub fn error(message: &str) -> Self {
        Self {
            content: Vec::new(),
            stop_reason: StopReason::Error,
            error_message: Some(message.to_string()),
            usage: empty_usage(),
            delay_ms: 0,
        }
    }

    pub fn tool_call(id: &str, name: &str, arguments: JsonObject) -> Self {
        Self {
            content: vec![AssistantContentBlock::ToolCall(ToolCall::new(
                id, name, arguments,
            ))],
            stop_reason: StopReason::ToolUse,
            error_message: None,
            usage: usage(100, 20),
            delay_ms: 0,
        }
    }

    pub fn tool_call_with_text(
        text: &str,
        id: &str,
        name: &str,
        arguments: JsonObject,
    ) -> Self {
        Self {
            content: vec![
                AssistantContentBlock::Text(TextContent::new(text)),
                AssistantContentBlock::ToolCall(ToolCall::new(id, name, arguments)),
            ],
            stop_reason: StopReason::ToolUse,
            error_message: None,
            usage: usage(100, 20),
            delay_ms: 0,
        }
    }

    pub fn length(text: &str, output: u64) -> Self {
        let mut response = Self::text(text);
        response.stop_reason = StopReason::Length;
        response.usage.output = output;
        response.usage.total_tokens = response.usage.input + output;
        response
    }
}

pub fn usage(input: u64, output: u64) -> Usage {
    Usage {
        input,
        output,
        cache_read: 0,
        cache_write: 0,
        total_tokens: input + output,
        cost: UsageCost::default(),
        ..Usage::default()
    }
}

pub fn model(context_window: u64, max_tokens: u64) -> Model {
    Model {
        id: "faux-model".to_string(),
        name: "Faux".to_string(),
        api: "faux".to_string(),
        provider: "faux".to_string(),
        base_url: "https://faux.invalid".to_string(),
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

#[derive(Clone)]
pub struct FauxProvider {
    responses: Arc<Mutex<VecDeque<FauxResponse>>>,
    calls: Arc<AtomicU32>,
    contexts: Arc<Mutex<Vec<Vec<xjpcode_ai::Message>>>>,
    model: Model,
}

impl FauxProvider {
    pub fn new(model: Model) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::new())),
            calls: Arc::new(AtomicU32::new(0)),
            contexts: Arc::new(Mutex::new(Vec::new())),
            model,
        }
    }

    pub fn set_responses(&self, responses: Vec<FauxResponse>) {
        *self.responses.lock().unwrap() = responses.into();
    }

    pub fn append_response(&self, response: FauxResponse) {
        self.responses.lock().unwrap().push_back(response);
    }

    pub fn call_count(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }

    pub fn model(&self) -> Model {
        self.model.clone()
    }

    pub fn contexts(&self) -> Vec<Vec<xjpcode_ai::Message>> {
        self.contexts.lock().unwrap().clone()
    }

    pub fn stream_fn(&self) -> StreamFn {
        let responses = Arc::clone(&self.responses);
        let calls = Arc::clone(&self.calls);
        let contexts = Arc::clone(&self.contexts);
        let model = self.model.clone();
        Arc::new(move |_model, context, options| {
            calls.fetch_add(1, Ordering::SeqCst);
            contexts.lock().unwrap().push(context.messages.clone());
            let response = responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| FauxResponse::error("no faux response queued"));
            stream_response(model.clone(), response, options.signal.clone())
        })
    }
}

fn assistant_message(model: &Model, response: &FauxResponse) -> AssistantMessage {
    let mut message = empty_assistant_message(model.api.clone(), model.provider.clone(), model.id.clone());
    message.content = response.content.clone();
    message.stop_reason = response.stop_reason;
    message.error_message = response.error_message.clone();
    message.usage = response.usage.clone();
    message
}

fn stream_response(
    model: Model,
    response: FauxResponse,
    signal: Option<xjpcode_ai::AbortSignal>,
) -> xjpcode_ai::AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let final_message = assistant_message(&model, &response);
    let mut partial = empty_assistant_message(model.api.clone(), model.provider.clone(), model.id.clone());
    partial.usage = response.usage.clone();
    if signal.is_some_and(|signal| signal.aborted()) {
        let mut aborted = final_message;
        aborted.stop_reason = StopReason::Aborted;
        stream.push(AssistantMessageEvent::Error {
            reason: StopReason::Aborted,
            error: aborted.clone(),
        });
        stream.end(Some(aborted));
        return stream;
    }
    stream.push(AssistantMessageEvent::Start {
        partial: partial.clone(),
    });
    let task_stream = stream.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(response.delay_ms)).await;
        task_stream.push(AssistantMessageEvent::Done {
            reason: response.stop_reason,
            message: final_message.clone(),
        });
        task_stream.end(Some(final_message));
    });
    stream
}

/// 取出一条消息的文本（测试断言用）。
pub fn message_text(message: &AgentMessage) -> String {
    match message {
        AgentMessage::User(user) => match &user.content {
            xjpcode_ai::UserContent::Text(text) => text.clone(),
            xjpcode_ai::UserContent::Blocks(blocks) => {
                xjpcode_ai::content_text(blocks.as_slice(), "")
            }
        },
        AgentMessage::Assistant(assistant) => xjpcode_ai::content_text(&assistant.content, ""),
        AgentMessage::ToolResult(result) => xjpcode_ai::content_text(result.content.as_slice(), ""),
        AgentMessage::System(system) => xjpcode_ai::get_system_message_text(system),
        AgentMessage::Custom(custom) => custom
            .payload
            .get("content")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
    }
}
