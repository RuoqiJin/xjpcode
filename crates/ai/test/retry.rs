//! 移植自上游 `packages/ai/test/retry.test.ts`。
//!
//! provider 的写法与分类逐条照旧；`retryAssistantCall` 的用例用同样的假响应，
//! 只看调用次数、退避毫秒数与回调顺序。

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use xjpcode_ai::utils::retry::{
    is_retryable_assistant_error, retry_assistant_call, retry_delay_ms, RetryCallbacks, RetryPolicy,
};
use xjpcode_ai::{
    AssistantMessage, AssistantRole, BoxFuture, ProviderId, StopReason, Usage, UsageCost,
};

fn assistant(stop_reason: StopReason, error_message: Option<&str>) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: "openai-completions".to_string(),
        provider: ProviderId::from("ollama"),
        model: "qwen3.5:35b".to_string(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage {
            cost: UsageCost::default(),
            ..Usage::default()
        },
        stop_reason,
        deferred: None,
        error_message: error_message.map(ToString::to_string),
        raw_stop_reason: None,
        end_turn: None,
        timestamp: 0,
        duration_ms: None,
    }
}

fn error_message(text: &str) -> AssistantMessage {
    assistant(StopReason::Error, Some(text))
}

fn retryable(text: &str) -> bool {
    is_retryable_assistant_error(&error_message(text))
}

#[test]
fn matches_the_codex_websocket_connection_lifetime_limit() {
    for text in [
        "Codex error: Responses websocket connection limit reached (60 minutes). Create a new websocket connection to continue.",
        "Codex error: websocket_connection_limit_reached",
    ] {
        assert!(retryable(text), "{text}");
    }
}

#[test]
fn does_not_match_a_generic_connection_limit() {
    assert!(!retryable("connection limit reached"));
}

#[test]
fn matches_explicit_provider_retry_guidance() {
    for text in [
        "An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID req_******** in your message.",
        "{\"message\":\"The system encountered an unexpected error during processing. Try your request again.\"}",
        "ResourceExhausted: Worker local total request limit reached (288/48)",
    ] {
        assert!(retryable(text), "{text}");
    }
}

#[test]
fn matches_bun_fetch_socket_drop_wording() {
    assert!(retryable(
        "The socket connection was closed unexpectedly. For more information, pass `verbose: true` in the second argument to fetch()"
    ));
}

#[test]
fn matches_upstream_request_buffer_exhaustion_wording() {
    assert!(retryable(
        "Error: exceeded request buffer limit while retrying upstream"
    ));
}

#[test]
fn matches_dns_transport_failure_wording() {
    for text in [
        "The pending stream has been canceled (caused by: getaddrinfo ENOTFOUND bedrock-runtime.us-east-1.amazonaws.com)",
        "connect ENOTFOUND api.example.com",
        "EAI_AGAIN api.example.com",
        "getaddrinfo failed for api.example.com",
    ] {
        assert!(retryable(text), "{text}");
    }
}

#[test]
fn matches_node_socket_error_codes() {
    for text in [
        "read ECONNRESET",
        "connect ETIMEDOUT 140.82.113.3:443",
        "write EPIPE",
        "connect ECONNREFUSED 127.0.0.1:443",
        "ECONNABORTED",
        "Connection reset by peer",
    ] {
        assert!(retryable(text), "{text}");
    }
}

#[test]
fn does_not_match_socket_error_codes_inside_longer_identifiers() {
    for text in [
        "Invalid value EXECONNRESET",
        "Unknown field ETIMEDOUTX",
        "Invalid key ERR_EPIPE_ID",
    ] {
        assert!(!retryable(text), "{text}");
    }
}

#[test]
fn matches_http2_pending_stream_cancellation() {
    for text in [
        "The pending stream has been canceled",
        "The pending stream has been canceled (caused by: socket closed)",
    ] {
        assert!(retryable(text), "{text}");
    }
}

#[test]
fn matches_service_unavailable_wording() {
    for text in [
        "The service is temporarily unavailable. Please retry later.",
        "Service unavailable: The service is temporarily unavailable.",
    ] {
        assert!(retryable(text), "{text}");
    }
}

#[test]
fn matches_node_fetch_request_failure_wording() {
    for text in [
        "request to https://oauth2.googleapis.com/token failed, reason:",
        "request to https://oauth2.googleapis.com/token failed, reason: connect ECONNREFUSED 127.0.0.1:9",
    ] {
        assert!(retryable(text), "{text}");
    }
}

#[test]
fn matches_gateway_upstream_failure_wording() {
    for text in [
        "unknown: Unable to forward upstream stream",
        "upstream_error: Upstream request failed",
        "upstream_stream_read_error: Upstream response stream was interrupted",
        "Upstream HTTP/2 stream failed",
    ] {
        assert!(retryable(text), "{text}");
    }
}

#[test]
fn matches_gateway_wording_for_a_stream_cut_short_mid_reply() {
    assert!(retryable(
        "Claude Code's stream from Anthropic was cut short after the reply had begun"
    ));
}

#[test]
fn matches_gateway_provider_failure_wording() {
    for text in [
        "The provider is temporarily unavailable. Retry shortly.",
        "The upstream provider is temporarily unavailable. Try again shortly.",
        "The provider ended the response before it was complete.",
        "The connection to the provider was reset. Retry shortly.",
        "The provider reported a failure while generating the response.",
    ] {
        assert!(retryable(text), "{text}");
    }
}

#[test]
fn does_not_match_lasting_provider_unavailable_wording() {
    for text in [
        "Provider unavailable for this model",
        "The provider is unavailable in your region",
    ] {
        assert!(!retryable(text), "{text}");
    }
}

#[test]
fn matches_gateway_wording_for_a_closed_connection() {
    assert!(retryable(
        "{\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"Connection closed.\"}}"
    ));
}

#[test]
fn matches_openai_responses_streams_that_end_before_terminal_events() {
    assert!(retryable(
        "OpenAI Responses stream ended before a terminal response event"
    ));
}

#[test]
fn matches_azure_peak_load_capacity_errors() {
    assert!(retryable(
        "The system is currently experiencing high demand and cannot process your request. Your request exceeds the maximum usage size allowed during peak load. For improved capacity reliability, consider switching to Provisioned Throughput."
    ));
}

#[test]
fn keeps_provider_limit_errors_non_retryable() {
    assert!(!retryable("429 quota exceeded"));
}

#[test]
fn keeps_the_chatgpt_subscription_usage_limit_non_retryable() {
    assert!(!retryable(
        "OpenAI API error (429): {\"code\":\"subscription_sharing_usage_limit_exceeded\",\"message\":\"Usage limit reached.\"}"
    ));
}

#[test]
fn keeps_the_chatgpt_business_workspace_usage_limit_non_retryable() {
    assert!(!retryable(
        "OpenAI API error (429): {\"code\":\"subscription_sharing_workplace_individual_usage_limit_exceeded\",\"message\":\"Usage limit reached.\"}"
    ));
}

#[test]
fn retries_temporary_chatgpt_subscription_errors() {
    for text in [
        "subscription_sharing_usage_unavailable: Usage cannot be checked.",
        "subscription_sharing_user_unavailable: User cannot be loaded.",
    ] {
        assert!(retryable(text), "{text}");
    }
}

#[test]
fn classifies_assistant_error_messages() {
    assert!(retryable("overloaded_error"));
    assert!(retryable("Provider finish_reason: network_error"));
    assert!(retryable("520 status code (no body)"));
    assert!(retryable("524 status code (no body)"));
    assert!(retryable("525 <!DOCTYPE html><title>SSL handshake failed</title>"));
    assert!(!is_retryable_assistant_error(&assistant(
        StopReason::Stop,
        None
    )));
}

#[test]
fn does_not_match_status_codes_inside_ids_or_longer_numbers() {
    for text in [
        "401 {\"type\":\"error\",\"error\":{\"type\":\"authentication_error\",\"message\":\"invalid x-api-key\"},\"request_id\":\"req_011CU503xyz\"}",
        "400 image exceeds 5 MB maximum: 5316784 bytes > 5242880 bytes",
    ] {
        assert!(!retryable(text), "{text}");
    }
}

#[test]
fn retries_temporary_provider_failures() {
    for text in [
        "Unable to process request. Repeat the request later.",
        "waited 73.4s in queue, reaching this model's admission ceiling of 60s; retry after the hinted delay",
        "{\"type\":\"error\",\"error\":{\"details\":null,\"type\":\"api_error\",\"message\":\"Unable to complete this request right now. Please try again.\"},\"request_id\":\"req_011CgfE1bTgPTDsor4TsApFv\"}",
        "Provider finish_reason: error",
        "Provider returned an empty response",
        "Upstream error from Relace: The model stopped before completing the response.",
    ] {
        assert!(retryable(text), "{text}");
    }
}

#[test]
fn does_not_retry_these() {
    for text in [
        "The operation was aborted",
        "{\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"messages: at least one message is required\"}}",
        "Provider finish_reason: content_filter",
        "{\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"Monthly quota exceeded\"}}",
    ] {
        assert!(!retryable(text), "{text}");
    }
}

#[test]
fn caps_agent_retry_delay() {
    let policy = RetryPolicy {
        enabled: true,
        max_retries: 3,
        base_delay_ms: 2000,
        max_agent_delay_ms: None,
    };
    assert_eq!(retry_delay_ms(&policy, 6), 60_000);
    let policy = RetryPolicy {
        max_agent_delay_ms: Some(5000),
        ..policy
    };
    assert_eq!(retry_delay_ms(&policy, 5), 5000);
    let policy = RetryPolicy {
        max_agent_delay_ms: Some(0),
        ..policy
    };
    assert_eq!(retry_delay_ms(&policy, 5), 0);
}

fn immediate_callbacks(log: Arc<Mutex<Vec<String>>>) -> RetryCallbacks {
    let scheduled = Arc::clone(&log);
    let started = Arc::clone(&log);
    let finished = Arc::clone(&log);
    RetryCallbacks {
        on_retry_scheduled: Some(Arc::new(move |attempt, max_attempts, delay_ms, error| {
            scheduled.lock().unwrap().push(format!(
                "retry:{attempt}/{max_attempts}/{delay_ms}/{error}"
            ));
            Box::pin(async {}) as BoxFuture<'static, ()>
        })),
        on_retry_attempt_start: Some(Arc::new(move || {
            started.lock().unwrap().push("attempt-start".to_string());
            Box::pin(async {}) as BoxFuture<'static, ()>
        })),
        on_retry_finished: Some(Arc::new(move |success, attempt, final_error| {
            finished.lock().unwrap().push(format!(
                "finished:{success}/{attempt}/{}",
                final_error.unwrap_or_default()
            ));
            Box::pin(async {}) as BoxFuture<'static, ()>
        })),
    }
}

#[tokio::test]
async fn retries_a_transient_error_up_to_max_retries_then_returns_the_final_error() {
    let calls = Arc::new(AtomicU32::new(0));
    let produce = {
        let calls = Arc::clone(&calls);
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { error_message("terminated") }) as BoxFuture<'static, _>
        }
    };
    let log = Arc::new(Mutex::new(Vec::new()));
    let policy = RetryPolicy {
        enabled: true,
        max_retries: 3,
        base_delay_ms: 0,
        max_agent_delay_ms: None,
    };
    let response =
        retry_assistant_call(produce, Some(&policy), None, &immediate_callbacks(log)).await;
    assert_eq!(response.stop_reason, StopReason::Error);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn stops_retrying_once_a_call_succeeds() {
    let calls = Arc::new(AtomicU32::new(0));
    let produce = {
        let calls = Arc::clone(&calls);
        move || {
            let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
            Box::pin(async move {
                if call < 4 {
                    error_message("terminated")
                } else {
                    assistant(StopReason::Stop, None)
                }
            }) as BoxFuture<'static, _>
        }
    };
    let log = Arc::new(Mutex::new(Vec::new()));
    let policy = RetryPolicy {
        enabled: true,
        max_retries: 3,
        base_delay_ms: 0,
        max_agent_delay_ms: None,
    };
    let response =
        retry_assistant_call(produce, Some(&policy), None, &immediate_callbacks(log.clone())).await;
    assert_eq!(response.stop_reason, StopReason::Stop);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        log.lock().unwrap().last().unwrap(),
        "finished:true/3/"
    );
}

#[tokio::test]
async fn does_not_retry_when_policy_is_disabled_or_the_error_is_not_retryable() {
    for policy in [
        RetryPolicy {
            enabled: false,
            ..RetryPolicy::default()
        },
        RetryPolicy {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 0,
            max_agent_delay_ms: None,
        },
    ] {
        let calls = Arc::new(AtomicU32::new(0));
        let produce = {
            let calls = Arc::clone(&calls);
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { error_message("insufficient_quota") }) as BoxFuture<'static, _>
            }
        };
        let log = Arc::new(Mutex::new(Vec::new()));
        let response =
            retry_assistant_call(produce, Some(&policy), None, &immediate_callbacks(log.clone()))
                .await;
        assert_eq!(response.stop_reason, StopReason::Error);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(log.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn reports_capped_retry_delays() {
    let calls = Arc::new(AtomicU32::new(0));
    let produce = {
        let calls = Arc::clone(&calls);
        move || {
            let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
            Box::pin(async move {
                if call < 5 {
                    error_message("terminated")
                } else {
                    assistant(StopReason::Stop, None)
                }
            }) as BoxFuture<'static, _>
        }
    };
    let delays = Arc::new(Mutex::new(Vec::new()));
    let scheduled = Arc::clone(&delays);
    let callbacks = RetryCallbacks {
        on_retry_scheduled: Some(Arc::new(move |_, _, delay_ms, _| {
            scheduled.lock().unwrap().push(delay_ms);
            Box::pin(async {}) as BoxFuture<'static, ()>
        })),
        ..RetryCallbacks::default()
    };
    let policy = RetryPolicy {
        enabled: true,
        max_retries: 4,
        base_delay_ms: 10,
        max_agent_delay_ms: Some(15),
    };
    let response = retry_assistant_call(produce, Some(&policy), None, &callbacks).await;
    assert_eq!(response.stop_reason, StopReason::Stop);
    assert_eq!(*delays.lock().unwrap(), vec![10, 15, 15, 15]);
}

#[tokio::test]
async fn reports_an_aborted_retried_call_as_unsuccessful() {
    let calls = Arc::new(AtomicU32::new(0));
    let produce = {
        let calls = Arc::clone(&calls);
        move || {
            let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
            Box::pin(async move {
                if call == 1 {
                    error_message("terminated")
                } else {
                    assistant(StopReason::Aborted, None)
                }
            }) as BoxFuture<'static, _>
        }
    };
    let log = Arc::new(Mutex::new(Vec::new()));
    let policy = RetryPolicy {
        enabled: true,
        max_retries: 3,
        base_delay_ms: 0,
        max_agent_delay_ms: None,
    };
    let response =
        retry_assistant_call(produce, Some(&policy), None, &immediate_callbacks(log.clone()))
            .await;
    assert_eq!(response.stop_reason, StopReason::Aborted);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(log
        .lock()
        .unwrap()
        .iter()
        .any(|entry| entry == "finished:false/1/"));
}

#[tokio::test]
async fn runs_callbacks_in_order() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicU32::new(0));
    let produce = {
        let calls = Arc::clone(&calls);
        let order = Arc::clone(&order);
        move || {
            let call = calls.fetch_add(1, Ordering::SeqCst);
            order.lock().unwrap().push(format!("produce:{call}"));
            let call = call + 1;
            Box::pin(async move {
                if call < 3 {
                    error_message("terminated")
                } else {
                    assistant(StopReason::Stop, None)
                }
            }) as BoxFuture<'static, _>
        }
    };
    let scheduled = Arc::clone(&order);
    let started = Arc::clone(&order);
    let callbacks = RetryCallbacks {
        on_retry_scheduled: Some(Arc::new(move |attempt, _, _, _| {
            scheduled.lock().unwrap().push(format!("retry:{attempt}"));
            Box::pin(async {}) as BoxFuture<'static, ()>
        })),
        on_retry_attempt_start: Some(Arc::new(move || {
            started.lock().unwrap().push("attempt-start".to_string());
            Box::pin(async {}) as BoxFuture<'static, ()>
        })),
        ..RetryCallbacks::default()
    };
    let policy = RetryPolicy {
        enabled: true,
        max_retries: 3,
        base_delay_ms: 0,
        max_agent_delay_ms: None,
    };
    let response = retry_assistant_call(produce, Some(&policy), None, &callbacks).await;
    assert_eq!(response.stop_reason, StopReason::Stop);
    assert_eq!(
        *order.lock().unwrap(),
        vec![
            "produce:0",
            "retry:1",
            "attempt-start",
            "produce:1",
            "retry:2",
            "attempt-start",
            "produce:2"
        ]
    );
}

#[test]
fn matches_rust_transport_and_premature_stream_end_wording() {
    // 里程碑六：Rust 网络库的两种报错，以及 pi 判定表里的 "ended without"。
    for text in [
        "error sending request for url (https://api.deepseek.com/chat/completions)",
        "error decoding response body",
        "error reading a body from connection",
        "Stream ended without finish_reason",
    ] {
        assert!(retryable(text), "{text}");
    }
}
