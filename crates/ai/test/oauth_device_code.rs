//! 对应上游 `packages/ai/test/oauth-device-code.test.ts`。
//!
//! 上游用 vitest 的假时钟；这里用 tokio 的 `start_paused`，拨快的是同一份时钟
//! （`crates/ai/src/auth/oauth/device_code.rs` 的截止时间用 tokio 的瞬时）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use xjpcode_ai::auth::oauth::device_code::{
    poll_oauth_device_code_flow, OAuthDeviceCodePollOptions, OAuthDeviceCodePollResult,
    SLOW_DOWN_TIMEOUT_MESSAGE, TIMEOUT_MESSAGE,
};
use xjpcode_ai::AbortSignal;

type PollResult = OAuthDeviceCodePollResult<String>;

async fn settle() {
    tokio::task::yield_now().await;
}

#[tokio::test(start_paused = true)]
async fn polls_immediately_and_returns_the_completed_value() {
    let start = tokio::time::Instant::now();
    let poll_times: Arc<Mutex<Vec<Duration>>> = Arc::new(Mutex::new(Vec::new()));
    let times = Arc::clone(&poll_times);
    let task = tokio::spawn(poll_oauth_device_code_flow(OAuthDeviceCodePollOptions {
        interval_seconds: Some(2.0),
        expires_in_seconds: Some(30.0),
        wait_before_first_poll: false,
        signal: AbortSignal::never(),
        poll: Arc::new(move || {
            let times = Arc::clone(&times);
            Box::pin(async move {
                let mut recorded = times.lock().unwrap();
                recorded.push(tokio::time::Instant::now() - start);
                let result = if recorded.len() == 1 {
                    PollResult::Pending
                } else {
                    PollResult::Complete("token".to_string())
                };
                Ok(result)
            })
        }),
    }));

    settle().await;
    assert_eq!(*poll_times.lock().unwrap(), vec![Duration::ZERO]);
    tokio::time::advance(Duration::from_millis(1999)).await;
    settle().await;
    assert_eq!(poll_times.lock().unwrap().len(), 1);
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(task.await.unwrap().unwrap(), "token");
    assert_eq!(
        *poll_times.lock().unwrap(),
        vec![Duration::ZERO, Duration::from_millis(2000)]
    );
}

#[tokio::test(start_paused = true)]
async fn can_wait_before_the_first_poll() {
    let start = tokio::time::Instant::now();
    let poll_times: Arc<Mutex<Vec<Duration>>> = Arc::new(Mutex::new(Vec::new()));
    let times = Arc::clone(&poll_times);
    let task = tokio::spawn(poll_oauth_device_code_flow(OAuthDeviceCodePollOptions {
        interval_seconds: Some(2.0),
        expires_in_seconds: Some(30.0),
        wait_before_first_poll: true,
        signal: AbortSignal::never(),
        poll: Arc::new(move || {
            let times = Arc::clone(&times);
            Box::pin(async move {
                times.lock().unwrap().push(tokio::time::Instant::now() - start);
                Ok(PollResult::Complete("token".to_string()))
            })
        }),
    }));

    settle().await;
    tokio::time::advance(Duration::from_millis(1999)).await;
    settle().await;
    assert!(poll_times.lock().unwrap().is_empty());
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(task.await.unwrap().unwrap(), "token");
    assert_eq!(
        *poll_times.lock().unwrap(),
        vec![Duration::from_millis(2000)]
    );
}

#[tokio::test(start_paused = true)]
async fn increases_the_interval_by_five_seconds_after_slow_down() {
    let start = tokio::time::Instant::now();
    let poll_times: Arc<Mutex<Vec<Duration>>> = Arc::new(Mutex::new(Vec::new()));
    let times = Arc::clone(&poll_times);
    let task = tokio::spawn(poll_oauth_device_code_flow(OAuthDeviceCodePollOptions {
        interval_seconds: Some(2.0),
        expires_in_seconds: Some(900.0),
        wait_before_first_poll: false,
        signal: AbortSignal::never(),
        poll: Arc::new(move || {
            let times = Arc::clone(&times);
            Box::pin(async move {
                let mut recorded = times.lock().unwrap();
                recorded.push(tokio::time::Instant::now() - start);
                if recorded.len() == 1 {
                    Ok(PollResult::SlowDown {
                        interval_seconds: None,
                    })
                } else {
                    Ok(PollResult::Complete("token".to_string()))
                }
            })
        }),
    }));

    settle().await;
    tokio::time::advance(Duration::from_millis(6999)).await;
    settle().await;
    assert_eq!(poll_times.lock().unwrap().len(), 1);
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(task.await.unwrap().unwrap(), "token");
    assert_eq!(
        *poll_times.lock().unwrap(),
        vec![Duration::ZERO, Duration::from_millis(7000)]
    );
}

#[tokio::test(start_paused = true)]
async fn honors_a_server_provided_slow_down_interval() {
    let start = tokio::time::Instant::now();
    let poll_times: Arc<Mutex<Vec<Duration>>> = Arc::new(Mutex::new(Vec::new()));
    let times = Arc::clone(&poll_times);
    let task = tokio::spawn(poll_oauth_device_code_flow(OAuthDeviceCodePollOptions {
        interval_seconds: Some(2.0),
        expires_in_seconds: Some(900.0),
        wait_before_first_poll: false,
        signal: AbortSignal::never(),
        poll: Arc::new(move || {
            let times = Arc::clone(&times);
            Box::pin(async move {
                let mut recorded = times.lock().unwrap();
                recorded.push(tokio::time::Instant::now() - start);
                if recorded.len() == 1 {
                    Ok(PollResult::SlowDown {
                        interval_seconds: Some(30.0),
                    })
                } else {
                    Ok(PollResult::Complete("token".to_string()))
                }
            })
        }),
    }));

    settle().await;
    tokio::time::advance(Duration::from_millis(29999)).await;
    settle().await;
    assert_eq!(poll_times.lock().unwrap().len(), 1);
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(task.await.unwrap().unwrap(), "token");
    assert_eq!(
        *poll_times.lock().unwrap(),
        vec![Duration::ZERO, Duration::from_millis(30000)]
    );
}

#[tokio::test(start_paused = true)]
async fn cancels_an_in_flight_wait() {
    let controller = xjpcode_ai::AbortController::new();
    let signal = controller.signal();
    let task = tokio::spawn(poll_oauth_device_code_flow(OAuthDeviceCodePollOptions {
        interval_seconds: Some(5.0),
        expires_in_seconds: Some(30.0),
        wait_before_first_poll: false,
        signal,
        poll: Arc::new(|| Box::pin(async { Ok(PollResult::Pending) })),
    }));
    settle().await;
    controller.abort();
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.message, "Login cancelled");
}

#[tokio::test(start_paused = true)]
async fn times_out_and_reports_slow_down_timeouts_differently() {
    let task = tokio::spawn(poll_oauth_device_code_flow(OAuthDeviceCodePollOptions {
        interval_seconds: Some(1.0),
        expires_in_seconds: Some(3.0),
        wait_before_first_poll: false,
        signal: AbortSignal::never(),
        poll: Arc::new(|| Box::pin(async { Ok(PollResult::Pending) })),
    }));
    settle().await;
    tokio::time::advance(Duration::from_millis(3000)).await;
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.message, TIMEOUT_MESSAGE);

    let task = tokio::spawn(poll_oauth_device_code_flow(OAuthDeviceCodePollOptions {
        interval_seconds: Some(1.0),
        expires_in_seconds: Some(2.0),
        wait_before_first_poll: false,
        signal: AbortSignal::never(),
        poll: Arc::new(|| {
            Box::pin(async {
                Ok(PollResult::SlowDown {
                    interval_seconds: None,
                })
            })
        }),
    }));
    settle().await;
    tokio::time::advance(Duration::from_millis(2000)).await;
    let error = task.await.unwrap().unwrap_err();
    assert_eq!(error.message, SLOW_DOWN_TIMEOUT_MESSAGE);
}
