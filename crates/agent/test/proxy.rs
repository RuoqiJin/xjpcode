//! 移植自上游 `packages/agent/test/proxy.test.ts`。
//!
//! 上游用 `vi.stubGlobal("fetch", …)` 换掉全局 fetch；Rust 侧没有可换的全局，改为起一台只应答一次的
//! 本地 HTTP 服务（同一份 SSE 正文、同样的连接行为）。断言与用例逐条照旧。

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use xjpcode_agent::{stream_proxy, ProxyAssistantMessageEvent, ProxyStreamOptions};
use xjpcode_ai::providers::model_schema::{ModelCost, ModelInputModality};
use xjpcode_ai::{
    normalize_context, AssistantMessage, AssistantMessageEvent, Context, JsonObject, Model,
    StopReason, ToolCall, Usage, UsageCost,
};

fn model() -> Model {
    Model {
        id: "gpt-5.4".to_string(),
        name: "GPT-5.4".to_string(),
        api: "openai-responses".to_string(),
        provider: "openai".to_string(),
        base_url: "https://api.openai.com/v1".to_string(),
        input: vec![ModelInputModality::Text],
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

fn usage() -> Usage {
    Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: UsageCost::default(),
    }
}

fn context() -> xjpcode_ai::TranscriptContext {
    normalize_context(Context {
        system_prompt: None,
        messages: Vec::new(),
        tools: None,
    })
}

/// 起一台只应答一次的本地服务，把 `body` 原样写回去。
async fn serve_once(body: String) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        // 读到请求头结束就够了；正文不关心（`Connection: close`）。
        let mut received = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let read = socket.read(&mut chunk).await.expect("read");
            if read == 0 {
                break;
            }
            received.extend_from_slice(&chunk[..read]);
            if received.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.expect("write");
        socket.flush().await.expect("flush");
    });
    (format!("http://{address}"), handle)
}

fn proxy_options(proxy_url: String) -> ProxyStreamOptions {
    ProxyStreamOptions {
        auth_token: "test-token".to_string(),
        proxy_url,
        ..Default::default()
    }
}

fn sse_body(events: &[ProxyAssistantMessageEvent]) -> String {
    events
        .iter()
        .map(|event| format!("data: {}\n\n", serde_json::to_string(event).expect("event")))
        .collect()
}

async fn next_event(
    consumer: &mut xjpcode_ai::Consumer<AssistantMessageEvent, AssistantMessage>,
) -> Option<AssistantMessageEvent> {
    consumer.next().await
}

fn object(value: Value) -> JsonObject {
    value.as_object().expect("object").clone()
}

#[tokio::test]
async fn preserves_tool_call_metadata_received_only_on_toolcall_end() {
    let proxy_events = vec![
        ProxyAssistantMessageEvent::Start,
        ProxyAssistantMessageEvent::ToolcallStart {
            content_index: 0,
            id: "call_test|fc_test".to_string(),
            tool_name: "lookup".to_string(),
        },
        ProxyAssistantMessageEvent::ToolcallDelta {
            content_index: 0,
            delta: "{\"value\":\"hello\"}".to_string(),
        },
        ProxyAssistantMessageEvent::ToolcallEnd {
            content_index: 0,
            tool_call: {
                let mut tool_call = ToolCall::new(
                    "call_test|fc_test",
                    "lookup",
                    object(json!({ "value": "hello" })),
                );
                tool_call.namespace = Some("dynamic_tools".to_string());
                tool_call
            },
        },
        ProxyAssistantMessageEvent::Done {
            reason: StopReason::ToolUse,
            usage: usage(),
            provider_thinking_level: None,
        },
    ];
    let (proxy_url, server) = serve_once(sse_body(&proxy_events)).await;

    let stream = stream_proxy(model(), context(), proxy_options(proxy_url));
    let mut consumer = stream.consumer();
    let mut events = Vec::new();
    while let Some(event) = next_event(&mut consumer).await {
        events.push(event);
    }
    let result = stream.result().await;
    server.await.expect("server");

    let end_event = events
        .iter()
        .find(|event| matches!(event, AssistantMessageEvent::ToolcallEnd { .. }))
        .expect("toolcall_end");
    match end_event {
        AssistantMessageEvent::ToolcallEnd { tool_call, .. } => {
            assert_eq!(tool_call.namespace.as_deref(), Some("dynamic_tools"));
        }
        _ => unreachable!(),
    }
    match result.content.first().expect("content") {
        xjpcode_ai::AssistantContentBlock::ToolCall(tool_call) => {
            assert_eq!(tool_call.namespace.as_deref(), Some("dynamic_tools"));
            assert_eq!(tool_call.arguments, object(json!({ "value": "hello" })));
        }
        other => panic!("expected a tool call, got {other:?}"),
    }
}

#[tokio::test]
async fn processes_terminal_metadata_when_the_event_is_not_newline_terminated() {
    let start = format!(
        "data: {}\n\n",
        serde_json::to_string(&ProxyAssistantMessageEvent::Start).expect("start")
    );
    let done = format!(
        "data: {}",
        serde_json::to_string(&ProxyAssistantMessageEvent::Done {
            reason: StopReason::Stop,
            usage: usage(),
            provider_thinking_level: Some("high".to_string()),
        })
        .expect("done")
    );
    let (proxy_url, server) = serve_once(format!("{start}{done}")).await;

    let stream = stream_proxy(model(), context(), proxy_options(proxy_url));
    let mut consumer = stream.consumer();
    let mut events = Vec::new();
    while let Some(event) = next_event(&mut consumer).await {
        events.push(event);
    }
    let result = stream.result().await;
    server.await.expect("server");

    let types: Vec<&'static str> = events
        .iter()
        .map(|event| match event {
            AssistantMessageEvent::Start { .. } => "start",
            AssistantMessageEvent::Done { .. } => "done",
            AssistantMessageEvent::Error { .. } => "error",
            _ => "other",
        })
        .collect();
    assert_eq!(types, vec!["start", "done"]);
    assert_eq!(result.stop_reason, StopReason::Stop);
    assert_eq!(result.provider_thinking_level.as_deref(), Some("high"));
}

#[tokio::test]
async fn emits_an_error_instead_of_hanging_when_the_stream_ends_without_a_terminal_event() {
    let body = format!(
        "data: {}\n\n",
        serde_json::to_string(&ProxyAssistantMessageEvent::Start).expect("start")
    );
    let (proxy_url, server) = serve_once(body).await;

    let stream = stream_proxy(model(), context(), proxy_options(proxy_url));
    let mut consumer = stream.consumer();
    let mut events = Vec::new();
    while let Some(event) = next_event(&mut consumer).await {
        events.push(event);
    }
    let result = stream.result().await;
    server.await.expect("server");

    let types: Vec<&'static str> = events
        .iter()
        .map(|event| match event {
            AssistantMessageEvent::Start { .. } => "start",
            AssistantMessageEvent::Done { .. } => "done",
            AssistantMessageEvent::Error { .. } => "error",
            _ => "other",
        })
        .collect();
    assert_eq!(types, vec!["start", "error"]);
    assert_eq!(result.stop_reason, StopReason::Error);
    assert!(result
        .error_message
        .unwrap_or_default()
        .contains("Connection closed by proxy server"));
}

#[tokio::test]
async fn shows_growing_partial_arguments_without_keeping_the_raw_text_on_the_block() {
    let content = "line \"quoted\" \\ end\n".repeat(20_000);
    let text = serde_json::to_string(&json!({ "path": "a.ts", "content": content })).expect("json");
    let mut proxy_events = vec![
        ProxyAssistantMessageEvent::Start,
        ProxyAssistantMessageEvent::ToolcallStart {
            content_index: 0,
            id: "call_1".to_string(),
            tool_name: "write".to_string(),
        },
    ];
    let mut index = 0;
    while index < text.len() {
        let end = (index + 32).min(text.len());
        proxy_events.push(ProxyAssistantMessageEvent::ToolcallDelta {
            content_index: 0,
            delta: text[index..end].to_string(),
        });
        index = end;
    }
    proxy_events.push(ProxyAssistantMessageEvent::ToolcallEnd {
        content_index: 0,
        tool_call: ToolCall::new(
            "call_1",
            "write",
            object(json!({ "path": "a.ts", "content": content })),
        ),
    });
    proxy_events.push(ProxyAssistantMessageEvent::Done {
        reason: StopReason::ToolUse,
        usage: usage(),
        provider_thinking_level: None,
    });
    let (proxy_url, server) = serve_once(sse_body(&proxy_events)).await;

    let started = Instant::now();
    let stream = stream_proxy(model(), context(), proxy_options(proxy_url));
    let mut consumer = stream.consumer();
    let mut shown = 0_usize;
    let mut deltas = 0_usize;
    while let Some(event) = next_event(&mut consumer).await {
        let AssistantMessageEvent::ToolcallDelta { partial, .. } = event else {
            continue;
        };
        let block = partial.content.first().expect("content block");
        let xjpcode_ai::AssistantContentBlock::ToolCall(block) = block else {
            panic!("expected a tool call block");
        };
        let partial_content = block
            .arguments
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        // 每段都比全文会让这个测试自己变二次方。
        if deltas % 500 == 0 {
            assert!(content.starts_with(partial_content));
        }
        deltas += 1;
        assert!(partial_content.len() >= shown);
        shown = partial_content.len();
    }
    let result = stream.result().await;
    server.await.expect("server");

    assert!(shown > content.len() - 64);
    assert_eq!(
        result.content,
        vec![xjpcode_ai::AssistantContentBlock::ToolCall(ToolCall::new(
            "call_1",
            "write",
            object(json!({ "path": "a.ts", "content": content })),
        ))]
    );
    // 每段都把累计的 520KB 参数重新读一遍时，这个测试要跑几分钟。
    assert!(started.elapsed() < Duration::from_secs(10));
}
