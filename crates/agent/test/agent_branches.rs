//! 上游没有测到的四条；这些用例是对着 `packages/agent/src/agent.ts` 写的，每条开头注明对的是哪几行。
//!
//! 前三条在上游是语言给的：`finally` 一定会跑，单线程里「先查后设」中间插不进别的调用。
//! Rust 这边的 future 可以被半路丢掉、可以 panic、可以同时从几个线程进来，所以要自己保证。

#[path = "utils/tools.rs"]
mod tools;

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use xjpcode_agent::{
    empty_assistant_message, Agent, AgentEvent, AgentMessage, AgentOptions, StreamFn,
};
use xjpcode_ai::{
    create_assistant_message_event_stream, AssistantContentBlock, AssistantMessage,
    AssistantMessageEvent, AssistantMessageEventStream, StopReason, TextContent,
};

use tools::boxed;

fn text_message(text: &str) -> AssistantMessage {
    let mut message = empty_assistant_message("openai-responses", "openai", "mock");
    message.content = vec![AssistantContentBlock::Text(TextContent::new(text))];
    message.stop_reason = StopReason::Stop;
    message
}

fn done_stream(message: AssistantMessage) -> AssistantMessageEventStream {
    let stream = create_assistant_message_event_stream();
    stream.push(AssistantMessageEvent::Done {
        reason: StopReason::Stop,
        message,
    });
    stream
}

fn agent_with(stream_fn: StreamFn) -> Agent {
    Agent::new(AgentOptions {
        stream_fn: Some(stream_fn),
        ..Default::default()
    })
    .expect("agent")
}

/// 对 `agent.ts:533-539`：`finally { this.finishRun() }`。
///
/// Rust 的 future 可以被半路丢掉（这里用超时）；丢掉以后 agent 也要回到空闲，能接下一句。
#[tokio::test]
async fn returns_to_idle_when_a_run_is_dropped_halfway() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&calls);
    let agent = agent_with(Arc::new(move |_model, _context, _options| {
        if calls_in_stream.fetch_add(1, Ordering::SeqCst) == 0 {
            // 第一次：一条永远不来事件的流，run 一直等在这里。
            create_assistant_message_event_stream()
        } else {
            done_stream(text_message("second"))
        }
    }));

    let first = tokio::time::timeout(Duration::from_millis(50), agent.prompt("first")).await;
    assert!(
        first.is_err(),
        "the first run is still waiting when it is dropped"
    );

    assert!(!agent.state().is_streaming);
    tokio::time::timeout(Duration::from_secs(1), agent.wait_for_idle())
        .await
        .expect("wait_for_idle returns");
    agent
        .prompt("second")
        .await
        .expect("the agent takes a new prompt");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// 对 `agent.ts:533-539`：处理失败的途中再出错，`finally` 照样收尾。
#[tokio::test]
async fn returns_to_idle_when_a_listener_panics_while_a_failure_is_reported() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_in_stream = Arc::clone(&calls);
    let agent = agent_with(Arc::new(move |_model, _context, _options| {
        if calls_in_stream.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("provider exploded");
        }
        done_stream(text_message("second"))
    }));
    let armed = Arc::new(AtomicBool::new(true));
    let armed_in_listener = Arc::clone(&armed);
    let _subscription = agent.subscribe(move |event, _signal| {
        let reports_the_failure = matches!(
            event,
            AgentEvent::MessageStart { message: AgentMessage::Assistant(message) }
                if message.error_message.is_some()
        );
        if reports_the_failure && armed_in_listener.load(Ordering::SeqCst) {
            panic!("listener exploded");
        }
        boxed(async {})
    });

    let first = AssertUnwindSafe(agent.prompt("first")).catch_unwind().await;
    assert!(first.is_err(), "the listener's panic reaches the caller");
    armed.store(false, Ordering::SeqCst);

    assert!(!agent.state().is_streaming);
    agent
        .prompt("second")
        .await
        .expect("the agent takes a new prompt");
}

/// 对 `agent.ts:517-527`：「没有在跑」的检查和登记之间插不进别的调用（上游是单线程）。
///
/// 几个线程同时来 prompt，只能有一个开始跑。赢的那一个被流卡住，直到其余的都已经被拒。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn starts_only_one_run_when_several_prompts_arrive_at_once() {
    const CONTENDERS: usize = 8;
    for _ in 0..200 {
        let calls = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let calls_in_stream = Arc::clone(&calls);
        let release_in_stream = Arc::clone(&release);
        let agent = agent_with(Arc::new(move |_model, _context, _options| {
            calls_in_stream.fetch_add(1, Ordering::SeqCst);
            let stream = create_assistant_message_event_stream();
            let pending = stream.clone();
            let release = Arc::clone(&release_in_stream);
            tokio::spawn(async move {
                release.acquire().await.expect("release").forget();
                pending.push(AssistantMessageEvent::Done {
                    reason: StopReason::Stop,
                    message: text_message("done"),
                });
            });
            stream
        }));

        let barrier = Arc::new(tokio::sync::Barrier::new(CONTENDERS));
        let (finished, mut results) = tokio::sync::mpsc::unbounded_channel::<bool>();
        for _ in 0..CONTENDERS {
            let agent = agent.clone();
            let barrier = Arc::clone(&barrier);
            let finished = finished.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                let started = agent.prompt("go").await.is_ok();
                let _ = finished.send(started);
            });
        }

        // 被拒的那几个马上就回来；等到它们（最多半秒），再放开赢的那个。
        let mut outcomes = Vec::new();
        while outcomes.len() < CONTENDERS - 1 {
            match tokio::time::timeout(Duration::from_millis(500), results.recv()).await {
                Ok(Some(started)) => outcomes.push(started),
                _ => break,
            }
        }
        release.add_permits(CONTENDERS);
        while outcomes.len() < CONTENDERS {
            let started = tokio::time::timeout(Duration::from_secs(2), results.recv())
                .await
                .expect("every prompt returns")
                .expect("a result");
            outcomes.push(started);
        }

        assert_eq!(calls.load(Ordering::SeqCst), 1, "exactly one run started");
        assert_eq!(outcomes.iter().filter(|started| **started).count(), 1);
    }
}

/// 对 `agent.ts:603-607`：`turn_end` 上的 `errorMessage` 按真假值判断，空字符串不记。
#[tokio::test]
async fn does_not_record_an_empty_error_text() {
    let agent = agent_with(Arc::new(|_model, _context, _options| {
        let mut message = text_message("");
        message.stop_reason = StopReason::Error;
        message.error_message = Some(String::new());
        let stream = create_assistant_message_event_stream();
        stream.push(AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: message,
        });
        stream
    }));

    let _ = agent.prompt("hi").await;

    assert_eq!(agent.state().error_message, None);
}
