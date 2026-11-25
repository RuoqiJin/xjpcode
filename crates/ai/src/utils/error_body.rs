//! 对应上游 `packages/ai/src/utils/error-body.ts`。
//!
//! 把 provider 的失败归一成一段能直接给用户看的文字。上游从 SDK 的错误对象上探形状
//! （`statusCode`/`status`/`body`/`error`/`$metadata`/`$response`）；Rust 侧的错误由自己的传输层
//! 构造，所以这里保留同样的字段与拼法，只把「探形状」换成显式构造（见 `PORTING.md` 差异表 #112）。

/// 错误正文最多留多少个字符（上游的 `MAX_PROVIDER_ERROR_BODY_CHARS`）。
pub const MAX_PROVIDER_ERROR_BODY_CHARS: usize = 4000;

/// pi: `NormalizedProviderError`
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NormalizedProviderError {
    /// 能从错误对象里拿到的 HTTP 状态码。
    pub status: Option<u16>,
    /// 已经 trim 并截断的 HTTP 正文。
    pub body: Option<String>,
    /// `error.message`；非 `Error` 的抛出值是它的 JSON 文本。
    pub message: String,
    /// `message` 里是否已经含有正文（有的话不再重复拼）。
    pub message_carries_body: bool,
}

/// pi: `truncateErrorText`——超长时截断并注明少了多少。
pub fn truncate_error_text(text: &str, max_chars: usize) -> String {
    let length = text.encode_utf16().count();
    if length <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars).collect();
    format!("{kept}... [truncated {} chars]", length - max_chars)
}

/// 把一段 HTTP 正文归一：去空白、空的当没有、按上限截断。
///
/// 上游只认「非空的普通对象」；`{}` 与 `[]` 那种读出来什么都没有的正文按没有处理。
fn normalize_body_text(body_text: &str) -> Option<String> {
    let trimmed = body_text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
        match &parsed {
            serde_json::Value::Object(members) if members.is_empty() => return None,
            serde_json::Value::Array(items) if items.is_empty() => return None,
            _ => {}
        }
    }
    Some(truncate_error_text(trimmed, MAX_PROVIDER_ERROR_BODY_CHARS))
}

/// 传输层拿到的 HTTP 失败：状态码、正文（已经读出来的那一份）、以及 SDK 风格的那句话。
///
/// 上游的 `openai` SDK 对非 2xx 抛 `"<status> status code (no body)"`，正文留在 `error.error`
/// 上；这里由 `openai_completions` 的 HTTP 层照着构造。
pub fn http_error(status: u16, body_text: Option<&str>) -> NormalizedProviderError {
    let message = format!("{status} status code (no body)");
    let body = body_text.and_then(normalize_body_text);
    let message_carries_body = match &body {
        Some(body) => message.contains(body.as_str()),
        None => true,
    };
    NormalizedProviderError {
        status: Some(status),
        body,
        message,
        message_carries_body,
    }
}

/// 连接级失败：没有状态码、没有正文，真正的理由在 cause 链的更深处。
///
/// 上游的 `withCauseDetail` 把最深一层 cause 接在消息后面；Rust 的 `reqwest` 错误已经带着一条
/// 可读的消息，这里把 reason 的 cause 链按同样的规则拼一遍。
pub fn transport_error(message: &str, causes: &[String]) -> NormalizedProviderError {
    let deepest = causes.iter().rev().find(|cause| !cause.trim().is_empty());
    let message = match deepest {
        Some(detail) if !message.contains(detail.as_str()) => {
            if message.is_empty() {
                detail.clone()
            } else {
                format!("{message}: {detail}")
            }
        }
        _ => message.to_string(),
    };
    NormalizedProviderError {
        status: None,
        body: None,
        message,
        message_carries_body: true,
    }
}

/// 不是 provider 失败的一句话（例如自己的配置错误）。
pub fn plain_error(message: impl Into<String>) -> NormalizedProviderError {
    NormalizedProviderError {
        status: None,
        body: None,
        message: message.into(),
        message_carries_body: true,
    }
}

/// pi: `formatProviderError`——有状态与正文时拼成 `"<status>: <body>"`（带前缀时多一段）。
pub fn format_provider_error(error: &NormalizedProviderError, prefix: Option<&str>) -> String {
    if error.message_carries_body || error.status.is_none() || error.body.is_none() {
        return match (prefix, error.status) {
            (Some(prefix), Some(status)) => format!("{prefix} ({status}): {}", error.message),
            _ => error.message.clone(),
        };
    }
    let body = error.body.as_deref().unwrap_or_default();
    match (prefix, error.status) {
        (Some(prefix), Some(status)) => format!("{prefix} ({status}): {body}"),
        (None, Some(status)) => format!("{status}: {body}"),
        _ => error.message.clone(),
    }
}
