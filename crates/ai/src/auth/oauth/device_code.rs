//! 对应上游 `packages/ai/src/auth/oauth/device-code.ts`。
//!
//! RFC 8628 的设备码轮询：间隔至少 1 秒（服务端没给就 5 秒），`slow_down` 把间隔加 5 秒，
//! 到期或取消就停。错误文案照搬，因为界面会原样显示。

use std::sync::Arc;

use crate::auth::types::{ModelsError, ModelsErrorCode};
use crate::types::BoxFuture;
use crate::utils::abort::AbortSignal;

pub const CANCEL_MESSAGE: &str = "Login cancelled";
pub const TIMEOUT_MESSAGE: &str = "Device flow timed out";
pub const SLOW_DOWN_TIMEOUT_MESSAGE: &str = "Device flow timed out after one or more slow_down responses. This is often caused by clock drift in WSL or VM environments. Please sync or restart the VM clock and try again.";
const MINIMUM_INTERVAL_MS: u64 = 1000;
// RFC 8628 section 3.2：服务端没给 `interval` 时用 5 秒。
const DEFAULT_POLL_INTERVAL_SECONDS: f64 = 5.0;
// RFC 8628 section 3.5：`slow_down` 表示轮询间隔要加 5 秒。
const SLOW_DOWN_INTERVAL_INCREMENT_MS: u64 = 5000;

/// 用单调时钟（上游的 `Date.now()` 在测试里被假时钟接管；这里用 tokio 的时钟，
/// `start_paused` 的测试同样能拨快它）。
fn now_instant() -> tokio::time::Instant {
    tokio::time::Instant::now()
}

/// pi: `OAuthDeviceCodePollResult<T>`
#[derive(Debug, Clone, PartialEq)]
pub enum OAuthDeviceCodePollResult<T> {
    Pending,
    SlowDown { interval_seconds: Option<f64> },
    Failed { message: String },
    Complete(T),
}

/// pi: `OAuthDeviceCodePollOptions<T>`
pub struct OAuthDeviceCodePollOptions<T> {
    pub interval_seconds: Option<f64>,
    pub expires_in_seconds: Option<f64>,
    pub wait_before_first_poll: bool,
    pub poll: Arc<dyn Fn() -> BoxFuture<'static, Result<OAuthDeviceCodePollResult<T>, ModelsError>> + Send + Sync>,
    pub signal: AbortSignal,
}

/// pi: `abortableSleep`
pub async fn abortable_sleep(
    ms: u64,
    signal: AbortSignal,
    cancel_message: &str,
) -> Result<(), ModelsError> {
    if signal.aborted() {
        return Err(ModelsError::login(cancel_message));
    }
    tokio::select! {
        _ = tokio::time::sleep(std::time::Duration::from_millis(ms)) => Ok(()),
        _ = signal.cancelled() => Err(ModelsError::login(cancel_message)),
    }
}

/// pi: `pollOAuthDeviceCodeFlow`
pub async fn poll_oauth_device_code_flow<T>(
    options: OAuthDeviceCodePollOptions<T>,
) -> Result<T, ModelsError> {
    let deadline = options
        .expires_in_seconds
        .map(|seconds| now_instant() + std::time::Duration::from_micros((seconds * 1_000_000.0) as u64));
    let mut interval_ms = MINIMUM_INTERVAL_MS.max(
        (options.interval_seconds.unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS) * 1000.0).floor() as u64,
    );

    let mut slow_down_responses = 0u32;
    let remaining_ms = |deadline: Option<tokio::time::Instant>| -> Option<u64> {
        deadline.map(|deadline| {
            deadline
                .saturating_duration_since(now_instant())
                .as_millis() as u64
        })
    };
    if options.wait_before_first_poll {
        if let Some(remaining) = remaining_ms(deadline).filter(|remaining| *remaining > 0) {
            abortable_sleep(
                interval_ms.min(remaining),
                options.signal.clone(),
                CANCEL_MESSAGE,
            )
            .await?;
        }
    }

    while remaining_ms(deadline).map(|remaining| remaining > 0).unwrap_or(true) {
        if options.signal.aborted() {
            return Err(ModelsError::login(CANCEL_MESSAGE));
        }

        let result = (options.poll)().await?;
        match result {
            OAuthDeviceCodePollResult::Complete(value) => return Ok(value),
            OAuthDeviceCodePollResult::Failed { message } => {
                return Err(ModelsError::login(message))
            }
            OAuthDeviceCodePollResult::SlowDown { interval_seconds } => {
                slow_down_responses += 1;
                // 服务端给了新间隔就用它（GitHub 在 `interval` 里报新的下限）；否则照 RFC 8628
                // 第 3.5 节加 5 秒。
                interval_ms = match interval_seconds {
                    Some(seconds) if seconds.is_finite() && seconds > 0.0 => {
                        MINIMUM_INTERVAL_MS.max((seconds * 1000.0).floor() as u64)
                    }
                    _ => MINIMUM_INTERVAL_MS.max(interval_ms + SLOW_DOWN_INTERVAL_INCREMENT_MS),
                };
            }
            OAuthDeviceCodePollResult::Pending => {}
        }

        let Some(remaining) = remaining_ms(deadline) else {
            abortable_sleep(interval_ms, options.signal.clone(), CANCEL_MESSAGE).await?;
            continue;
        };
        if remaining == 0 {
            break;
        }
        abortable_sleep(
            interval_ms.min(remaining),
            options.signal.clone(),
            CANCEL_MESSAGE,
        )
        .await?;
    }

    Err(ModelsError::new(
        ModelsErrorCode::Auth,
        if slow_down_responses > 0 {
            SLOW_DOWN_TIMEOUT_MESSAGE
        } else {
            TIMEOUT_MESSAGE
        },
    ))
}
