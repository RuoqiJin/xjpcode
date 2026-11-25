//! 移植自上游 `packages/ai/test/event-stream.test.ts`。
//!
//! 与上游一样是异步用例（`for await` ↔ `next().await`、`await result()` ↔ `result().await`），
//! 用 tokio 运行时跑。涉及"两个消费者同时等待"的用例用 `tokio::spawn`。

use std::time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH};

use tokio::time::{sleep, Duration};

use xjpcode_ai::types::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, StopReason,
    Usage,
};
use xjpcode_ai::utils::event_stream::{AssistantMessageEventStream, Consumer, EventStream};

/// 把消费者的剩余事件收干净，等价于上游的 `for await (const event of stream)`。
async fn drain<T, R>(consumer: &mut Consumer<T, R>) -> Vec<T> {
    let mut events = Vec::new();
    while let Some(event) = consumer.next().await {
        events.push(event);
    }
    events
}

/// 等到有 `expected` 个消费者挂到流上；超时说明实现没在等待，直接失败而不是挂住整个测试。
async fn wait_for_consumers<T, R>(stream: &EventStream<T, R>, expected: usize) {
    let deadline = Instant::now() + StdDuration::from_secs(5);
    while stream.pending_consumers() < expected {
        assert!(
            Instant::now() < deadline,
            "consumer did not start waiting (expected {expected})"
        );
        tokio::task::yield_now().await;
    }
}

// ---------------------------------------------------------------------------
// 上游 describe("EventStream")
// 回归用例：https://github.com/earendil-works/pi/issues/9055
// ---------------------------------------------------------------------------

#[tokio::test]
async fn drains_buffered_events_in_order_and_ignores_events_pushed_after_completion() {
    let stream = EventStream::new(|event: &i32| *event == 3, |event: &i32| *event);
    stream.push(1);
    stream.push(2);
    stream.push(3);
    stream.push(4);

    assert_eq!(stream.result().await, 3);

    let events = drain(&mut stream.consumer()).await;
    assert_eq!(events, vec![1, 2, 3]);
}

#[tokio::test]
async fn preserves_order_when_events_arrive_after_buffered_draining_starts() {
    let stream = EventStream::new(|_: &i32| false, |event: &i32| *event);
    stream.push(1);
    stream.push(2);

    let mut iterator = stream.consumer();
    assert_eq!(iterator.next().await, Some(1));

    stream.push(3);
    assert_eq!(iterator.next().await, Some(2));
    assert_eq!(iterator.next().await, Some(3));

    stream.end(Some(3));
    assert_eq!(iterator.next().await, None);
}

#[tokio::test]
async fn delivers_events_to_waiting_consumers_in_registration_order() {
    let stream = EventStream::new(|_: &i32| false, |event: &i32| *event);
    let mut first = stream.consumer();
    let mut second = stream.consumer();

    let first_event = tokio::spawn(async move { first.next().await });
    wait_for_consumers(&stream, 1).await;
    let second_event = tokio::spawn(async move { second.next().await });
    wait_for_consumers(&stream, 2).await;

    stream.push(1);
    stream.push(2);

    assert_eq!(first_event.await.expect("first consumer"), Some(1));
    assert_eq!(second_event.await.expect("second consumer"), Some(2));
}

#[tokio::test]
async fn drains_buffered_events_after_end_and_resolves_the_explicit_result() {
    let stream = EventStream::new(|_: &i32| false, |event: &i32| event.to_string());
    stream.push(1);
    stream.push(2);
    stream.end(Some("complete".to_string()));

    assert_eq!(stream.result().await, "complete");

    let events = drain(&mut stream.consumer()).await;
    assert_eq!(events, vec![1, 2]);
}

#[tokio::test]
async fn wakes_all_waiting_consumers_when_ended_without_a_result() {
    let stream = EventStream::new(|_: &i32| false, |event: &i32| *event);
    let mut first = stream.consumer();
    let mut second = stream.consumer();

    let first_event = tokio::spawn(async move { first.next().await });
    wait_for_consumers(&stream, 1).await;
    let second_event = tokio::spawn(async move { second.next().await });
    wait_for_consumers(&stream, 2).await;

    stream.end(None);

    assert_eq!(first_event.await.expect("first consumer"), None);
    assert_eq!(second_event.await.expect("second consumer"), None);
}

// ---------------------------------------------------------------------------
// 上游 describe("AssistantMessageEventStream timing")
// ---------------------------------------------------------------------------

fn wall_clock_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

fn message(timestamp: u64, duration_ms: Option<u64>) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::<AssistantContentBlock>::new(),
        api: "openai-responses".into(),
        provider: "openai".into(),
        model: "m".into(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp,
        duration_ms,
    }
}

fn done(message: AssistantMessage) -> AssistantMessageEvent {
    AssistantMessageEvent::Done {
        reason: StopReason::Stop,
        message,
    }
}

#[tokio::test]
async fn sets_duration_ms_on_the_final_done_or_error_message_of_a_response_it_saw_start() {
    let done_stream = AssistantMessageEventStream::new();
    sleep(Duration::from_millis(20)).await;
    done_stream.push(done(message(wall_clock_ms(), None)));
    let measured = done_stream
        .result()
        .await
        .duration_ms
        .expect("done message is timed");
    assert!(measured >= 15, "measured {measured}ms");

    let failed = AssistantMessageEventStream::new();
    let mut error = message(wall_clock_ms(), None);
    error.stop_reason = StopReason::Error;
    failed.push(AssistantMessageEvent::Error {
        reason: StopReason::Error,
        error,
    });
    assert!(failed.result().await.duration_ms.is_some());

    let ended = AssistantMessageEventStream::new();
    ended.end(Some(message(wall_clock_ms(), None)));
    assert!(ended.result().await.duration_ms.is_some());
}

#[tokio::test]
async fn keeps_an_existing_duration_so_a_forwarding_stream_keeps_the_inner_measurement() {
    let outer = AssistantMessageEventStream::new();
    sleep(Duration::from_millis(20)).await;

    let inner = AssistantMessageEventStream::new();
    inner.push(done(message(wall_clock_ms(), None)));
    let measured = inner.result().await.duration_ms.expect("inner measured");
    assert!(measured < 20, "measured {measured}ms");

    // 转发已经计时过的消息：外层不得覆盖内层的测量。
    outer.push(done(inner.result().await));
    assert_eq!(outer.result().await.duration_ms, Some(measured));

    let preset = AssistantMessageEventStream::new();
    preset.push(done(message(wall_clock_ms(), Some(1234))));
    assert_eq!(preset.result().await.duration_ms, Some(1234));
}

#[tokio::test]
async fn leaves_a_message_untimed_when_it_started_before_the_stream_such_as_a_fetched_deferred_result(
) {
    let fetched = message(wall_clock_ms().saturating_sub(60_000), None);
    let stream = AssistantMessageEventStream::new();
    stream.push(done(fetched));
    assert_eq!(stream.result().await.duration_ms, None);
}

#[tokio::test]
async fn does_not_time_a_message_pushed_after_the_stream_completed() {
    let stream = AssistantMessageEventStream::new();
    let first_timestamp = wall_clock_ms();
    stream.push(done(message(first_timestamp, None)));

    // 上游断言"迟到的那条 durationMs 仍为 undefined"：它就地检查传进去的对象。
    // Rust 的 push 取得所有权，迟到的消息不会再交回调用方，因此断言同一件事的
    // 可观察后果：流已结束，第二次 push 被丢弃，最终结果仍是第一条。
    stream.push(done(message(first_timestamp + 1, None)));
    assert_eq!(stream.result().await.timestamp, first_timestamp);
}
