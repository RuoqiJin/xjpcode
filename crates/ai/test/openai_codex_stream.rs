//! 对应上游 `packages/ai/test/openai-codex-stream.test.ts` 里走到 SSE 的分支：
//! 请求头与请求体的形状、session-id / prompt_cache_key、思考档（configuration_update）、
//! 流在没有结尾空行时也收尾、429 额度用完的提示、工具调用。
//!
//! 上游用 `vi.stubGlobal("fetch")`；这里起一台本机 HTTP 服务，模型 baseUrl 指过去。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use xjpcode_ai::api::openai_codex_responses::{stream, OpenAICodexResponsesOptions};
use xjpcode_ai::providers::compat_schema::OpenAICompletionsCompat;
use xjpcode_ai::providers::model_schema::{
    ModelCost, ModelInputModality, ModelThinkingLevel, ThinkingLevelMap,
};
use xjpcode_ai::{
    AbortController, AssistantContentBlock, AssistantMessageEvent, Model, StopReason, SystemContent,
    SystemMessage, SystemRole, TextContent, Tool, ToolCall, ToolResultContentBlock,
    ToolResultMessage, ToolResultRole, TranscriptContext, Usage, UsageCost, UserContent,
    UserMessage, UserRole,
};

#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Value,
}

struct FakeCodex {
    base_url: String,
    records: Arc<Mutex<Vec<Recorded>>>,
    controller: AbortController,
}

impl FakeCodex {
    async fn start(
        respond: impl Fn(&Recorded) -> (u16, String) + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let records = Arc::new(Mutex::new(Vec::new()));
        let records_for_task = Arc::clone(&records);
        let controller = AbortController::new();
        let shutdown = controller.signal();
        let respond = Arc::new(respond);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else { break };
                        let records = Arc::clone(&records_for_task);
                        let respond = Arc::clone(&respond);
                        tokio::spawn(async move {
                            let mut buffer = Vec::new();
                            let mut chunk = [0u8; 8192];
                            loop {
                                if buffer.windows(4).any(|w| w == b"\r\n\r\n") { break; }
                                let read = stream.read(&mut chunk).await.unwrap_or(0);
                                if read == 0 { break; }
                                buffer.extend_from_slice(&chunk[..read]);
                            }
                            let text = String::from_utf8_lossy(&buffer).to_string();
                            let mut lines = text.lines();
                            let mut request = lines.next().unwrap_or("").split_whitespace();
                            let method = request.next().unwrap_or("").to_string();
                            let path = request.next().unwrap_or("").to_string();
                            let headers: BTreeMap<String, String> = text
                                .lines()
                                .skip(1)
                                .take_while(|line| !line.is_empty())
                                .filter_map(|line| line.split_once(':'))
                                .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_string()))
                                .collect();
                            let headers_len = text.find("\r\n\r\n").map(|i| i + 4).unwrap_or(text.len());
                            let content_length = headers
                                .get("content-length")
                                .and_then(|value| value.parse::<usize>().ok())
                                .unwrap_or(0);
                            let mut body = buffer[headers_len.min(buffer.len())..].to_vec();
                            while body.len() < content_length {
                                let read = stream.read(&mut chunk).await.unwrap_or(0);
                                if read == 0 { break; }
                                body.extend_from_slice(&chunk[..read]);
                            }
                            let body = if headers
                                .get("content-encoding")
                                .map(|value| value.eq_ignore_ascii_case("zstd"))
                                .unwrap_or(false)
                            {
                                zstd::bulk::decompress(&body, 1 << 22).unwrap_or_default()
                            } else {
                                body
                            };
                            let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                            let record = Recorded { method, path, headers, body: parsed };
                            {
                                records.lock().unwrap().push(record.clone());
                            }
                            let (status, body) = respond(&record);
                            let response = format!(
                                "HTTP/1.1 {status} X\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = stream.write_all(response.as_bytes()).await;
                        });
                    }
                }
            }
        });
        Self {
            base_url: format!("http://127.0.0.1:{port}"),
            records,
            controller,
        }
    }

    fn records(&self) -> Vec<Recorded> {
        self.records.lock().unwrap().clone()
    }
}

impl Drop for FakeCodex {
    fn drop(&mut self) {
        self.controller.abort();
    }
}

fn model(base_url: &str, mid_convo_effort: bool) -> Model {
    Model {
        id: "gpt-6.2-astra".to_string(),
        name: "GPT-6.2 Astra".to_string(),
        api: "openai-codex-responses".to_string(),
        provider: "openai-codex".to_string(),
        base_url: base_url.to_string(),
        input: vec![ModelInputModality::Text],
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        r#type: None,
        reasoning: true,
        thinking_level_map: Some(ThinkingLevelMap::from([
            (ModelThinkingLevel::Off, None),
            (ModelThinkingLevel::Minimal, Some("low".to_string())),
            (ModelThinkingLevel::Low, Some("low".to_string())),
            (ModelThinkingLevel::Medium, Some("medium".to_string())),
            (ModelThinkingLevel::High, Some("high".to_string())),
            (ModelThinkingLevel::Xhigh, Some("xhigh".to_string())),
            (ModelThinkingLevel::Max, Some("max".to_string())),
        ])),
        prompt_cache: None,
        context_window: 272_000,
        max_tokens: 128_000,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: Some(OpenAICompletionsCompat {
            supports_mid_convo_system_messages: Some(true),
            supports_additional_tools: Some(true),
            supports_tool_search: Some(true),
            supports_mid_convo_effort: Some(mid_convo_effort),
            ..Default::default()
        }),
    }
}

fn access_token() -> String {
    use base64::Engine;
    let payload = base64::engine::general_purpose::STANDARD.encode(
        serde_json::json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "acc_test" }
        })
        .to_string(),
    );
    format!("aaa.{payload}.bbb")
}

fn system_with_tools(text: &str) -> SystemMessage {
    SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text(text.to_string()),
        sections: None,
        tools_added: Some(vec![Tool {
            name: "bash".to_string(),
            description: "Run a command".to_string(),
            parameters: serde_json::json!({"type":"object","properties":{"command":{"type":"string"}}}),
            constrained_sampling: None,
        }]),
        tools_removed: None,
        timestamp: 0,
    }
}

fn user(text: &str) -> UserMessage {
    UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp: 1,
    }
}

fn options(api_key: &str) -> OpenAICodexResponsesOptions {
    OpenAICodexResponsesOptions {
        api_key: Some(api_key.to_string()),
        ..Default::default()
    }
}

fn text_sse() -> String {
    [
        r#"data: {"type":"response.created","response":{"id":"resp_1"}}"#,
        r#"data: {"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"msg_1","role":"assistant","status":"in_progress","content":[]}}"#,
        r#"data: {"type":"response.output_text.delta","output_index":0,"delta":"Hello"}"#,
        r#"data: {"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"msg_1","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Hello"}]}}"#,
        r#"data: {"type":"response.completed","response":{"id":"resp_1","status":"completed","usage":{"input_tokens":5,"output_tokens":3,"total_tokens":8}}}"#,
    ]
    .join("\n\n")
    // 故意不带结尾空行：上游也在 EOF 时收下最后一帧。
}

async fn collect(mut consumer: xjpcode_ai::Consumer<AssistantMessageEvent, xjpcode_ai::AssistantMessage>) -> Vec<AssistantMessageEvent> {
    let mut events = Vec::new();
    while let Some(event) = consumer.next().await {
        events.push(event);
    }
    events
}

#[tokio::test]
async fn sends_the_codex_request_shape_and_finishes_without_a_trailing_blank_line() {
    let server = FakeCodex::start(|_| (200, text_sse())).await;
    let model = model(&server.base_url, true);
    let context = TranscriptContext {
        messages: vec![
            xjpcode_ai::Message::System(system_with_tools("You are concise.")),
            xjpcode_ai::Message::User(user("hi")),
        ],
    };
    let mut options = options(&access_token());
    options.session_id = Some("session-1".to_string());
    options.max_tokens = Some(32_000);
    options.reasoning_effort = Some(ModelThinkingLevel::Low);
    let stream = stream(model.clone(), context, Some(options), None);
    let events = collect(stream.consumer()).await;

    assert!(events.iter().any(|event| matches!(event, AssistantMessageEvent::TextDelta { delta, .. } if delta == "Hello")));
    assert!(matches!(events.last(), Some(AssistantMessageEvent::Done { reason: StopReason::Stop, .. })));

    let record = &server.records()[0];
    assert_eq!(record.method, "POST");
    assert_eq!(record.path, "/codex/responses");
    assert_eq!(record.headers.get("authorization").map(String::as_str), Some(format!("Bearer {}", access_token()).as_str()));
    assert_eq!(record.headers.get("chatgpt-account-id").map(String::as_str), Some("acc_test"));
    assert_eq!(record.headers.get("originator").map(String::as_str), Some("pi"));
    assert_eq!(record.headers.get("openai-beta").map(String::as_str), Some("responses=experimental"));
    assert_eq!(record.headers.get("accept").map(String::as_str), Some("text/event-stream"));
    assert_eq!(record.headers.get("session-id").map(String::as_str), Some("session-1"));
    assert_eq!(record.headers.get("x-client-request-id").map(String::as_str), Some("session-1"));
    assert_eq!(record.headers.get("content-encoding").map(String::as_str), Some("zstd"));

    let body = &record.body;
    assert_eq!(body["model"], "gpt-6.2-astra");
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body["prompt_cache_key"], "session-1");
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["parallel_tool_calls"], true);
    assert_eq!(body["max_output_tokens"], 32_000);
    assert_eq!(body["text"]["verbosity"], "low");
    assert_eq!(body["include"][0], "reasoning.encrypted_content");
    // 系统提示词作为 input 里的 developer 项，不放在顶层 instructions。
    assert!(body.get("instructions").is_none());
    assert!(body.get("tools").is_none());
    let input = body["input"].as_array().unwrap();
    assert_eq!(input[0]["type"], "additional_tools");
    assert_eq!(input[0]["tools"][0]["name"], "bash");
    assert_eq!(input[1]["role"], "developer");
    assert_eq!(input[1]["content"], "You are concise.");
    assert_eq!(input[2]["role"], "user");
    // supportsMidConvoEffort：请求固定 medium，档位由 configuration_update 报。
    assert_eq!(body["reasoning"]["effort"], "medium");
    assert_eq!(body["reasoning"]["summary"], "auto");
    let last = input.last().unwrap();
    assert_eq!(last["type"], "configuration_update");
    assert_eq!(last["reasoning"]["effort"], "low");
}

#[tokio::test]
async fn omits_cache_affinity_when_cache_retention_is_none() {
    let server = FakeCodex::start(|_| (200, text_sse())).await;
    let model = model(&server.base_url, true);
    let context = TranscriptContext {
        messages: vec![xjpcode_ai::Message::User(user("hi"))],
    };
    let mut options = options(&access_token());
    options.session_id = Some("session-2".to_string());
    options.cache_retention = Some(xjpcode_ai::providers::model_schema::CacheRetention::None);
    let stream = stream(model, context, Some(options), None);
    let _ = collect(stream.consumer()).await;
    let record = &server.records()[0];
    assert!(record.body.get("prompt_cache_key").is_none());
    assert!(record.headers.get("session-id").is_none());
    assert!(record.headers.get("x-client-request-id").is_none());
}

#[tokio::test]
async fn sends_thinking_levels_as_reasoning_effort_without_mid_convo_effort() {
    let server = FakeCodex::start(|_| (200, text_sse())).await;
    let model = model(&server.base_url, false);
    let context = TranscriptContext {
        messages: vec![xjpcode_ai::Message::User(user("hi"))],
    };
    let mut options = options(&access_token());
    options.reasoning_effort = Some(ModelThinkingLevel::Xhigh);
    let stream = stream(model, context, Some(options), None);
    let _ = collect(stream.consumer()).await;
    let body = &server.records()[0].body;
    assert_eq!(body["reasoning"]["effort"], "xhigh");
    assert!(!body["input"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["type"] == "configuration_update"));
}

#[tokio::test]
async fn reports_the_chatgpt_usage_limit_in_a_friendly_way() {
    let server = FakeCodex::start(|_| {
        (
            429,
            serde_json::json!({
                "error": {
                    "code": "usage_limit_reached",
                    "message": "usage limit reached",
                    "plan_type": "plus",
                }
            })
            .to_string(),
        )
    })
    .await;
    let model = model(&server.base_url, true);
    let context = TranscriptContext {
        messages: vec![xjpcode_ai::Message::User(user("hi"))],
    };
    let stream = stream(model, context, Some(options(&access_token())), None);
    let events = collect(stream.consumer()).await;
    match events.last().unwrap() {
        AssistantMessageEvent::Error { reason, error } => {
            assert_eq!(*reason, StopReason::Error);
            assert_eq!(
                error.error_message.as_deref(),
                Some("You have hit your ChatGPT usage limit (plus plan).")
            );
        }
        other => panic!("unexpected last event {other:?}"),
    }
}

#[tokio::test]
async fn parses_a_streamed_tool_call_into_a_tool_use_stop_reason() {
    let events_sse = [
        r#"data: {"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"bash","arguments":""}}"#,
        r#"data: {"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"command\":"}"#,
        r#"data: {"type":"response.function_call_arguments.delta","output_index":0,"delta":"\"echo hi\"}"}"#,
        r#"data: {"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"bash","arguments":"{\"command\":\"echo hi\"}"}}"#,
        r#"data: {"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}"#,
    ]
    .join("\n\n");
    let server = FakeCodex::start(move |_| (200, events_sse.clone())).await;
    let model = model(&server.base_url, true);
    let context = TranscriptContext {
        messages: vec![xjpcode_ai::Message::User(user("run it"))],
    };
    let stream = stream(model, context, Some(options(&access_token())), None);
    let events = collect(stream.consumer()).await;
    let done = events
        .iter()
        .find_map(|event| match event {
            AssistantMessageEvent::Done { message, .. } => Some(message.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(done.stop_reason, StopReason::ToolUse);
    let tool_call = done
        .content
        .iter()
        .find_map(|block| match block {
            AssistantContentBlock::ToolCall(tool_call) => Some(tool_call.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(tool_call.name, "bash");
    assert_eq!(tool_call.arguments.get("command").and_then(Value::as_str), Some("echo hi"));
    assert_eq!(tool_call.id, "call_1|fc_1");
    assert_eq!(done.usage.total_tokens, 15);
    let _ = (ToolCall::new("x", "y", Default::default()), ToolResultMessage::<Value> {
        role: ToolResultRole::ToolResult,
        tool_call_id: "x".to_string(),
        tool_name: "y".to_string(),
        content: vec![ToolResultContentBlock::Text(TextContent::new("z"))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 0,
        duration_ms: None,
    }, Usage::default(), UsageCost::default());
}

#[tokio::test]
async fn finishes_when_the_terminal_event_arrives_even_if_the_connection_stays_open() {
    use tokio::time::{timeout, Duration};

    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = [0u8; 4096];
        let _ = stream.read(&mut buffer).await;
        let body = [
            r#"data: {"type":"response.created","response":{"id":"resp_1"}}"#,
            r#"data: {"type":"response.output_text.delta","output_index":0,"delta":"Hi"}"#,
            r#"data: {"type":"response.completed","response":{"id":"resp_1","status":"completed"}}"#,
        ]
        .join("\n\n");
        // 没有 content-length：正文靠连接结束来收尾。发完结束事件后故意不关。
        let _ = stream
            .write_all(
                format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n{body}\n\n")
                    .as_bytes(),
            )
            .await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let model = model(&format!("http://127.0.0.1:{port}"), true);
    let context = TranscriptContext {
        messages: vec![xjpcode_ai::Message::User(user("hi"))],
    };
    let stream = stream(model, context, Some(options(&access_token())), None);
    let events = timeout(Duration::from_secs(1), collect(stream.consumer()))
        .await
        .expect("terminal event should finish the turn without waiting for the connection to close");
    assert!(matches!(
        events.last(),
        Some(AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            ..
        })
    ));
}

#[tokio::test]
async fn reads_codex_error_fields_from_the_outer_event_or_the_nested_error() {
    for (body, expected) in [
        (
            r#"data: {"type":"error","error":{"code":"context_length_exceeded","message":"prompt too long"}}"#,
            "Codex error: prompt too long",
        ),
        (
            r#"data: {"type":"error","code":"rate_limit_exceeded","message":"slow down"}"#,
            "Codex error: slow down",
        ),
        (
            r#"data: {"type":"response.failed","response":{"error":{"code":"rate_limit_exceeded","message":"rate limit reached"}}}"#,
            "rate limit reached",
        ),
    ] {
        let body = body.to_string();
        let server = FakeCodex::start(move |_| (200, body.clone())).await;
        let model = model(&server.base_url, true);
        let context = TranscriptContext {
            messages: vec![xjpcode_ai::Message::User(user("hi"))],
        };
        let stream = stream(model, context, Some(options(&access_token())), None);
        let events = collect(stream.consumer()).await;
        match events.last().unwrap() {
            AssistantMessageEvent::Error { error, .. } => {
                assert_eq!(error.error_message.as_deref(), Some(expected));
            }
            other => panic!("unexpected last event {other:?}"),
        }
    }
}
