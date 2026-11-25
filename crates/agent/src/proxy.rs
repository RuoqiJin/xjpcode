//! 对应上游 `packages/agent/src/proxy.ts`。
//!
//! 上游用 `fetch` + `ReadableStream` 读服务端发回的 SSE；Rust 侧用 `reqwest` 的流式响应体，
//! 逐行处理 `data: ` 开头的那一行。取消照上游的 `AbortSignal`：请求还没回来时取消请求，读流时
//! 取消读（见 `PORTING.md` 差异表 #75）。

use std::collections::BTreeMap;
use std::collections::HashMap;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use xjpcode_ai::{
    create_assistant_message_event_stream, AbortSignal, AssistantContentBlock, AssistantMessage,
    AssistantMessageEvent, AssistantMessageEventStream, JsonValue, Model, SamplingParams,
    StopReason, ThinkingBudgets, ThinkingContent, ToolCall, ToolCallArgumentsStream,
    TranscriptContext, Transport,
};

use crate::types::empty_assistant_message;

/// pi: `ProxyAssistantMessageEvent`——服务端发来的事件（`partial` 字段被去掉，省带宽）。
///
/// 字段名是线上的写法（`contentIndex`、`toolName`、`toolCall`、`contentSignature`、`errorMessage`、
/// `providerThinkingLevel`），所以整个枚举的字段统一转成 camelCase。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum ProxyAssistantMessageEvent {
    #[serde(rename = "start")]
    Start,
    #[serde(rename = "text_start")]
    TextStart { content_index: usize },
    #[serde(rename = "text_delta")]
    TextDelta { content_index: usize, delta: String },
    #[serde(rename = "text_end")]
    TextEnd {
        content_index: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_signature: Option<String>,
    },
    #[serde(rename = "thinking_start")]
    ThinkingStart { content_index: usize },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { content_index: usize, delta: String },
    #[serde(rename = "thinking_end")]
    ThinkingEnd {
        content_index: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_signature: Option<String>,
    },
    #[serde(rename = "toolcall_start")]
    ToolcallStart {
        content_index: usize,
        id: String,
        tool_name: String,
    },
    #[serde(rename = "toolcall_delta")]
    ToolcallDelta { content_index: usize, delta: String },
    #[serde(rename = "toolcall_end")]
    ToolcallEnd {
        content_index: usize,
        tool_call: ToolCall,
    },
    #[serde(rename = "done")]
    Done {
        reason: StopReason,
        usage: xjpcode_ai::Usage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_thinking_level: Option<String>,
    },
    #[serde(rename = "error")]
    Error {
        reason: StopReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
        usage: xjpcode_ai::Usage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_thinking_level: Option<String>,
    },
}

/// pi: `ProxySerializableStreamOptions` = `Pick<SimpleStreamOptions, …>`。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxySerializableStreamOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<SamplingParams>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<xjpcode_ai::providers::model_schema::ThinkingLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<xjpcode_ai::providers::model_schema::CacheRetention>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_continuation: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, Option<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, JsonValue>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budgets: Option<ThinkingBudgets>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<u64>,
}

/// pi: `ProxyStreamOptions extends ProxySerializableStreamOptions`
///
/// `extends` 落成字段重复（差异 #19）。
#[derive(Debug, Clone, Default)]
pub struct ProxyStreamOptions {
    // 来自 ProxySerializableStreamOptions
    pub temperature: Option<f64>,
    pub sampling_params: Option<SamplingParams>,
    pub max_tokens: Option<u64>,
    pub reasoning: Option<xjpcode_ai::providers::model_schema::ThinkingLevel>,
    pub cache_retention: Option<xjpcode_ai::providers::model_schema::CacheRetention>,
    pub session_id: Option<String>,
    pub disable_continuation: Option<bool>,
    pub headers: Option<BTreeMap<String, Option<String>>>,
    pub metadata: Option<BTreeMap<String, JsonValue>>,
    pub transport: Option<Transport>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub max_retry_delay_ms: Option<u64>,
    // 自有字段
    /// 本次 proxy 请求的取消信号。
    pub signal: Option<AbortSignal>,
    /// proxy 服务的鉴权令牌。
    pub auth_token: String,
    /// proxy 服务的地址（例如 `https://genai.example.com`）。
    pub proxy_url: String,
}

impl ProxyStreamOptions {
    /// pi: `buildProxyRequestOptions`
    fn serializable(&self) -> ProxySerializableStreamOptions {
        ProxySerializableStreamOptions {
            temperature: self.temperature,
            sampling_params: self.sampling_params.clone(),
            max_tokens: self.max_tokens,
            reasoning: self.reasoning,
            cache_retention: self.cache_retention,
            session_id: self.session_id.clone(),
            disable_continuation: self.disable_continuation,
            headers: self.headers.clone(),
            metadata: self.metadata.clone(),
            transport: self.transport,
            thinking_budgets: self.thinking_budgets.clone(),
            max_retry_delay_ms: self.max_retry_delay_ms,
        }
    }
}

#[derive(Serialize)]
struct ProxyRequestBody<'a> {
    model: &'a Model,
    context: &'a TranscriptContext,
    options: ProxySerializableStreamOptions,
}

/// pi: `streamProxy`——经一台服务端转发的流函数。
///
/// 上游把活干在 `(async () => {...})()` 里，同步返回流；Rust 侧用 `tokio::spawn` 跑同一件事。
pub fn stream_proxy(
    model: Model,
    context: TranscriptContext,
    options: ProxyStreamOptions,
) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    let task_stream = stream.clone();
    tokio::spawn(async move {
        run_proxy(model, context, options, task_stream).await;
    });
    stream
}

async fn run_proxy(
    model: Model,
    context: TranscriptContext,
    options: ProxyStreamOptions,
    stream: AssistantMessageEventStream,
) {
    // 一边收事件一边拼出来的半成品消息。
    let mut partial =
        empty_assistant_message(model.api.clone(), model.provider.clone(), model.id.clone());

    let result = read_proxy_stream(&model, &context, &options, &mut partial, &stream).await;
    if let Err(error) = result {
        let aborted = options.signal.as_ref().is_some_and(AbortSignal::aborted);
        let reason = if aborted {
            StopReason::Aborted
        } else {
            StopReason::Error
        };
        partial.stop_reason = reason;
        partial.error_message = Some(error);
        stream.push(AssistantMessageEvent::Error {
            reason,
            error: partial,
        });
    }
    stream.end(None);
}

/// 请求、读流、把每个事件交给 `process_proxy_event`。
async fn read_proxy_stream(
    model: &Model,
    context: &TranscriptContext,
    options: &ProxyStreamOptions,
    partial: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
) -> Result<(), String> {
    let client = reqwest::Client::new();
    let request = client
        .post(format!("{}/api/stream", options.proxy_url))
        .bearer_auth(&options.auth_token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(
            serde_json::to_string(&ProxyRequestBody {
                model,
                context,
                options: options.serializable(),
            })
            .map_err(|error| error.to_string())?,
        );
    let response = match &options.signal {
        Some(signal) => {
            tokio::select! {
                response = request.send() => response.map_err(|error| error.to_string())?,
                () = signal.cancelled() => return Err("Request aborted by user".to_string()),
            }
        }
        None => request.send().await.map_err(|error| error.to_string())?,
    };

    if !response.status().is_success() {
        let status = response.status();
        let mut error_message = format!(
            "Proxy error: {} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        )
        .trim_end()
        .to_string();
        if let Ok(error_body) = response.json::<JsonValue>().await {
            if let Some(error) = error_body.get("error").and_then(JsonValue::as_str) {
                error_message = format!("Proxy error: {error}");
            }
        }
        return Err(error_message);
    }

    let mut body = response.bytes_stream();
    let mut pending: Vec<u8> = Vec::new();
    let mut saw_terminal_event = false;
    // 未结束的工具调用参数流，按内容下标放；不挂在内容块上。
    let mut tool_arguments: HashMap<usize, ToolCallArgumentsStream> = HashMap::new();

    loop {
        let next = match &options.signal {
            Some(signal) => {
                tokio::select! {
                    next = body.next() => next,
                    () = signal.cancelled() => return Err("Request aborted by user".to_string()),
                }
            }
            None => body.next().await,
        };
        let Some(chunk) = next else {
            break;
        };
        let chunk = chunk.map_err(|error| error.to_string())?;
        pending.extend_from_slice(&chunk);

        // 只在完整的行上解析：一段字节可能把一个多字节字符劈成两半。
        while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&line);
            process_proxy_line(
                &line,
                partial,
                &mut tool_arguments,
                stream,
                &mut saw_terminal_event,
            )?;
        }
    }

    if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
        return Err("Request aborted by user".to_string());
    }

    // 最后一个事件可能没有换行结尾：把剩下的字节也处理掉。
    if !pending.is_empty() {
        let line = String::from_utf8_lossy(&pending).into_owned();
        process_proxy_line(
            &line,
            partial,
            &mut tool_arguments,
            stream,
            &mut saw_terminal_event,
        )?;
    }

    if !saw_terminal_event {
        // 干净地 EOF 却没有 done/error：服务端把响应丢在半路。报错，别让消费者一直等一个
        // 永远不会来的结果。
        partial.stop_reason = StopReason::Error;
        partial.error_message =
            Some("Connection closed by proxy server before the response completed".to_string());
        stream.push(AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: partial.clone(),
        });
    }
    Ok(())
}

fn process_proxy_line(
    line: &str,
    partial: &mut AssistantMessage,
    tool_arguments: &mut HashMap<usize, ToolCallArgumentsStream>,
    stream: &AssistantMessageEventStream,
    saw_terminal_event: &mut bool,
) -> Result<(), String> {
    let Some(data) = line.strip_prefix("data: ") else {
        return Ok(());
    };
    let data = data.trim();
    if data.is_empty() {
        return Ok(());
    }
    let value: JsonValue = serde_json::from_str(data).map_err(|error| error.to_string())?;
    // 上游 `processProxyEvent` 的 `default` 分支：不认识的事件类型打一句警告就跳过，流照常往下走
    // （`proxy.ts:391-395`）。这里的事件是有类型的枚举，装不下不认识的类型，所以在解析之前拦下。
    // 警告没有落处（这个包不往终端写字），只跳过。
    if !is_known_proxy_event(&value) {
        return Ok(());
    }
    let proxy_event: ProxyAssistantMessageEvent =
        serde_json::from_value(value).map_err(|error| error.to_string())?;
    if let Some(event) = process_proxy_event(proxy_event, partial, tool_arguments)? {
        if matches!(
            event,
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
        ) {
            *saw_terminal_event = true;
        }
        stream.push(event);
    }
    Ok(())
}

/// 这个 JSON 的 `type` 是不是 `ProxyAssistantMessageEvent` 里有的那十二种。
fn is_known_proxy_event(value: &JsonValue) -> bool {
    matches!(
        value.get("type").and_then(JsonValue::as_str),
        Some(
            "start"
                | "text_start"
                | "text_delta"
                | "text_end"
                | "thinking_start"
                | "thinking_delta"
                | "thinking_end"
                | "toolcall_start"
                | "toolcall_delta"
                | "toolcall_end"
                | "done"
                | "error"
        )
    )
}

/// pi: `processProxyEvent`——把一个 proxy 事件折进半成品消息，给出对应的事件。
fn process_proxy_event(
    proxy_event: ProxyAssistantMessageEvent,
    partial: &mut AssistantMessage,
    tool_arguments: &mut HashMap<usize, ToolCallArgumentsStream>,
) -> Result<Option<AssistantMessageEvent>, String> {
    match proxy_event {
        ProxyAssistantMessageEvent::Start => Ok(Some(AssistantMessageEvent::Start {
            partial: partial.clone(),
        })),
        ProxyAssistantMessageEvent::TextStart { content_index } => {
            set_block(
                partial,
                content_index,
                AssistantContentBlock::Text(xjpcode_ai::TextContent::new("")),
            );
            Ok(Some(AssistantMessageEvent::TextStart {
                content_index,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::TextDelta {
            content_index,
            delta,
        } => match partial.content.get_mut(content_index) {
            Some(AssistantContentBlock::Text(content)) => {
                content.text.push_str(&delta);
                Ok(Some(AssistantMessageEvent::TextDelta {
                    content_index,
                    delta,
                    partial: partial.clone(),
                }))
            }
            _ => Err("Received text_delta for non-text content".to_string()),
        },
        ProxyAssistantMessageEvent::TextEnd {
            content_index,
            content_signature,
        } => match partial.content.get_mut(content_index) {
            Some(AssistantContentBlock::Text(content)) => {
                content.text_signature = content_signature;
                Ok(Some(AssistantMessageEvent::TextEnd {
                    content_index,
                    content: content.text.clone(),
                    partial: partial.clone(),
                }))
            }
            _ => Err("Received text_end for non-text content".to_string()),
        },
        ProxyAssistantMessageEvent::ThinkingStart { content_index } => {
            set_block(
                partial,
                content_index,
                AssistantContentBlock::Thinking(ThinkingContent {
                    r#type: xjpcode_ai::ThinkingContentType::Thinking,
                    thinking: String::new(),
                    thinking_signature: None,
                    redacted: None,
                }),
            );
            Ok(Some(AssistantMessageEvent::ThinkingStart {
                content_index,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::ThinkingDelta {
            content_index,
            delta,
        } => match partial.content.get_mut(content_index) {
            Some(AssistantContentBlock::Thinking(content)) => {
                content.thinking.push_str(&delta);
                Ok(Some(AssistantMessageEvent::ThinkingDelta {
                    content_index,
                    delta,
                    partial: partial.clone(),
                }))
            }
            _ => Err("Received thinking_delta for non-thinking content".to_string()),
        },
        ProxyAssistantMessageEvent::ThinkingEnd {
            content_index,
            content_signature,
        } => match partial.content.get_mut(content_index) {
            Some(AssistantContentBlock::Thinking(content)) => {
                content.thinking_signature = content_signature;
                Ok(Some(AssistantMessageEvent::ThinkingEnd {
                    content_index,
                    content: content.thinking.clone(),
                    partial: partial.clone(),
                }))
            }
            _ => Err("Received thinking_end for non-thinking content".to_string()),
        },
        ProxyAssistantMessageEvent::ToolcallStart {
            content_index,
            id,
            tool_name,
        } => {
            set_block(
                partial,
                content_index,
                AssistantContentBlock::ToolCall(ToolCall::new(id, tool_name, Default::default())),
            );
            tool_arguments.insert(content_index, ToolCallArgumentsStream::new());
            Ok(Some(AssistantMessageEvent::ToolcallStart {
                content_index,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::ToolcallDelta {
            content_index,
            delta,
        } => {
            let has_stream = tool_arguments.contains_key(&content_index);
            match partial.content.get_mut(content_index) {
                Some(AssistantContentBlock::ToolCall(content)) if has_stream => {
                    // 每段文字都换一个新的参数对象，看值的消费方才看得到变化。
                    let arguments = tool_arguments
                        .get_mut(&content_index)
                        .expect("tool arguments stream")
                        .append(&delta);
                    content.arguments = arguments.as_ref().clone();
                    Ok(Some(AssistantMessageEvent::ToolcallDelta {
                        content_index,
                        delta,
                        partial: partial.clone(),
                    }))
                }
                _ => Err("Received toolcall_delta for non-toolCall content".to_string()),
            }
        }
        ProxyAssistantMessageEvent::ToolcallEnd {
            content_index,
            tool_call,
        } => {
            let is_tool_call = matches!(
                partial.content.get(content_index),
                Some(AssistantContentBlock::ToolCall(_))
            );
            if !is_tool_call {
                return Ok(None);
            }
            // 元数据可能只在结尾这一条里给（例如 `namespace`），整块换掉。
            set_block(
                partial,
                content_index,
                AssistantContentBlock::ToolCall(tool_call.clone()),
            );
            tool_arguments.remove(&content_index);
            Ok(Some(AssistantMessageEvent::ToolcallEnd {
                content_index,
                tool_call,
                partial: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::Done {
            reason,
            usage,
            provider_thinking_level,
        } => {
            partial.stop_reason = reason;
            partial.usage = usage;
            if provider_thinking_level.is_some() {
                partial.provider_thinking_level = provider_thinking_level;
            }
            Ok(Some(AssistantMessageEvent::Done {
                reason,
                message: partial.clone(),
            }))
        }
        ProxyAssistantMessageEvent::Error {
            reason,
            error_message,
            usage,
            provider_thinking_level,
        } => {
            partial.stop_reason = reason;
            partial.error_message = error_message;
            partial.usage = usage;
            if provider_thinking_level.is_some() {
                partial.provider_thinking_level = provider_thinking_level;
            }
            Ok(Some(AssistantMessageEvent::Error {
                reason,
                error: partial.clone(),
            }))
        }
    }
}

/// 把内容块放到 `index` 上；短了就补空。
fn set_block(partial: &mut AssistantMessage, index: usize, block: AssistantContentBlock) {
    while partial.content.len() <= index {
        partial
            .content
            .push(AssistantContentBlock::Text(xjpcode_ai::TextContent::new(
                "",
            )));
    }
    partial.content[index] = block;
}
