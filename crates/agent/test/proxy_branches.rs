//! 上游没有测到的两条；这些用例是对着 `packages/agent/src/proxy.ts` 写的，每条开头注明对的是哪几行。
//!
//! 搬来的 `test/proxy.rs` 里，服务端发来的事件是用 Rust 这边的序列化造出来的，两头用的是同一套字段名，
//! 所以字段名和上游线上的写法对不对，它测不出来。这里的正文是照上游的类型（`proxy.ts:23-44`）手写的。

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use xjpcode_agent::{stream_proxy, ProxyStreamOptions};
use xjpcode_ai::providers::model_schema::{ModelCost, ModelInputModality};
use xjpcode_ai::{normalize_context, AssistantContentBlock, Context, Model, StopReason};

const USAGE: &str = r#"{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}"#;

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

fn context() -> xjpcode_ai::TranscriptContext {
    normalize_context(Context {
        system_prompt: None,
        messages: Vec::new(),
        tools: None,
    })
}

/// 起一台只应答一次的本地服务，把 `body` 原样写回去（与 `test/proxy.rs` 里的那台一样）。
async fn serve_once(body: String) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
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

/// 把几行 JSON 拼成服务端发来的正文。
fn sse_body(events: &[String]) -> String {
    events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}

async fn run(body: String) -> xjpcode_ai::AssistantMessage {
    let (proxy_url, server) = serve_once(body).await;
    let stream = stream_proxy(
        model(),
        context(),
        ProxyStreamOptions {
            auth_token: "test-token".to_string(),
            proxy_url,
            ..Default::default()
        },
    );
    let result = stream.result().await;
    server.await.expect("server");
    result
}

/// 对 `proxy.ts:23-44`：线上的字段名是 `contentSignature`、`toolName`、`toolCall`、`providerThinkingLevel`。
#[tokio::test]
async fn reads_events_in_the_field_names_the_proxy_server_sends() {
    let result = run(sse_body(&[
        r#"{"type":"start"}"#.to_string(),
        r#"{"type":"text_start","contentIndex":0}"#.to_string(),
        r#"{"type":"text_delta","contentIndex":0,"delta":"Hi"}"#.to_string(),
        r#"{"type":"text_end","contentIndex":0,"contentSignature":"sig-1"}"#.to_string(),
        r#"{"type":"toolcall_start","contentIndex":1,"id":"call-1","toolName":"lookup"}"#.to_string(),
        r#"{"type":"toolcall_delta","contentIndex":1,"delta":"{\"value\":\"hello\"}"}"#.to_string(),
        r#"{"type":"toolcall_end","contentIndex":1,"toolCall":{"type":"toolCall","id":"call-1","name":"lookup","arguments":{"value":"hello"}}}"#.to_string(),
        format!(r#"{{"type":"done","reason":"toolUse","usage":{USAGE},"providerThinkingLevel":"high"}}"#),
    ]))
    .await;

    assert_eq!(result.error_message, None);
    assert_eq!(result.stop_reason, StopReason::ToolUse);
    assert_eq!(result.provider_thinking_level.as_deref(), Some("high"));
    match &result.content[0] {
        AssistantContentBlock::Text(text) => {
            assert_eq!(text.text, "Hi");
            assert_eq!(text.text_signature.as_deref(), Some("sig-1"));
        }
        other => panic!("expected a text block, got {other:?}"),
    }
    match &result.content[1] {
        AssistantContentBlock::ToolCall(tool_call) => {
            assert_eq!(tool_call.name, "lookup");
            assert_eq!(
                tool_call
                    .arguments
                    .get("value")
                    .and_then(|value| value.as_str()),
                Some("hello")
            );
        }
        other => panic!("expected a tool call, got {other:?}"),
    }
}

/// 对 `proxy.ts:384-389`：`error` 事件里的 `errorMessage` 落到最终消息上。
#[tokio::test]
async fn keeps_the_error_text_the_proxy_server_sends() {
    let result = run(sse_body(&[
        r#"{"type":"start"}"#.to_string(),
        format!(r#"{{"type":"error","reason":"error","errorMessage":"upstream exploded","usage":{USAGE}}}"#),
    ]))
    .await;

    assert_eq!(result.stop_reason, StopReason::Error);
    assert_eq!(result.error_message.as_deref(), Some("upstream exploded"));
}

/// 对 `proxy.ts:391-395`：不认识的事件类型跳过，流照常往下走。
#[tokio::test]
async fn skips_an_event_type_it_does_not_know() {
    let result = run(sse_body(&[
        r#"{"type":"start"}"#.to_string(),
        r#"{"type":"ping"}"#.to_string(),
        r#"{"type":"text_start","contentIndex":0}"#.to_string(),
        r#"{"type":"text_delta","contentIndex":0,"delta":"Hi"}"#.to_string(),
        r#"{"type":"text_end","contentIndex":0}"#.to_string(),
        format!(r#"{{"type":"done","reason":"stop","usage":{USAGE}}}"#),
    ]))
    .await;

    assert_eq!(result.error_message, None);
    assert_eq!(result.stop_reason, StopReason::Stop);
    match &result.content[0] {
        AssistantContentBlock::Text(text) => assert_eq!(text.text, "Hi"),
        other => panic!("expected a text block, got {other:?}"),
    }
}
