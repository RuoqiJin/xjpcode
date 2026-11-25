//! 对应上游 `packages/ai/src/utils/overflow.ts`。
//!
//! 认两类「上下文装不下」：provider 明说的报错（各家写法不同），以及不报错但用量已经超窗
//! （静默截断）和「length 停止但一个字也没输出」（输入填满窗口）。类型分得比能否重试更细。
//!
//! 上游用正则表达式的列表；Rust 侧不用正则，改成小写子串与状态码边界判断（同 `retry.rs`）。

use crate::types::{AssistantMessage, StopReason};

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn starts_with_number(text: &str, number: &str) -> bool {
    let Some(rest) = text.strip_prefix(number) else {
        return false;
    };
    rest.chars()
        .next()
        .is_none_or(|character| !character.is_ascii_digit())
}

/// 上游 `^(?:[\w -]+ API error \(413\):|413(?:\s|:|$))`。
fn mentions_413(text: &str) -> bool {
    let text = &text.to_lowercase();
    if starts_with_number(text, "413") {
        let rest = &text[3..];
        let next = rest.chars().next();
        if next.is_none_or(|character| character.is_whitespace() || character == ':') {
            return true;
        }
    }
    if let Some(index) = text.find(" api error (413):") {
        let prefix = &text[..index];
        if !prefix.is_empty()
            && prefix
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || matches!(character, ' ' | '-' | '_'))
        {
            return true;
        }
    }
    false
}

fn is_non_overflow(text: &str) -> bool {
    text.starts_with("throttling error:")
        || text.starts_with("service unavailable:")
        || text.contains("rate limit")
        || text.contains("too many requests")
}

/// pi: `isContextOverflow`
pub fn is_context_overflow(message: &AssistantMessage, context_window: Option<u64>) -> bool {
    if message.stop_reason == StopReason::Error {
        if let Some(error_message) = message.error_message.as_deref() {
            let text = error_message.to_lowercase();
            if !is_non_overflow(&text) {
                if contains_any(
                    &text,
                    &[
                        "prompt too long",
                        "prompt is too long",
                        "prompt exceeds max length",
                        "request_too_large",
                        "input is too long for requested model",
                        "exceeds the context window",
                        "exceeds model's maximum context length",
                        "exceeds the model's maximum context length",
                        "exceeds maximum context length",
                        "exceeds the maximum context length",
                        "context window exceeded",
                        "maximum prompt length is",
                        "reduce the length of the messages",
                        "maximum context length is",
                        "exceeds the maximum allowed input length of",
                        "exceeds maximum allowed input length of",
                        "is longer than the model's context length",
                        "exceeds the limit of",
                        "exceeds limit of",
                        "exceeds the available context size",
                        "greater than the context length",
                        "context window exceeds limit",
                        "exceeded model token limit",
                        "too large for model with",
                        "model_context_window_exceeded",
                        "prompt too long; exceeded",
                        "range of input length should be",
                        "context_length_exceeded",
                        "context length exceeded",
                        "too many tokens",
                        "token limit exceeded",
                    ],
                ) {
                    return true;
                }
                if text.contains("input token count") && text.contains("exceeds the maximum") {
                    return true;
                }
                if text.contains("prompt has")
                    && text.contains("tokens")
                    && text.contains("configured context size is")
                {
                    return true;
                }
                if mentions_413(&text) {
                    return true;
                }
                if message.provider == "cerebras"
                    && (text.starts_with("400 status code (no body)")
                        || text.starts_with("413 status code (no body)")
                        || text.starts_with("400 (no body)")
                        || text.starts_with("413 (no body)"))
                {
                    return true;
                }
            }
        }
    }

    // 静默溢出（z.ai 一类）：成功返回，但输入已经超过窗口。
    if let Some(context_window) = context_window.filter(|window| *window > 0) {
        let input_tokens = message.usage.input + message.usage.cache_read;
        if message.stop_reason == StopReason::Stop && input_tokens > context_window {
            return true;
        }
        // length 停止且没有输出，输入填满了窗口（Xiaomi MiMo 一类）。
        if message.stop_reason == StopReason::Length
            && message.usage.output == 0
            && (input_tokens as f64) >= context_window as f64 * 0.99
        {
            return true;
        }
    }

    false
}

/// pi: `isRecoverableLength`——长度停止发生在原本要的输出上限之下，可能是上下文压力。
pub fn is_recoverable_length(message: &AssistantMessage, desired_max_output: u64) -> bool {
    message.stop_reason == StopReason::Length
        && desired_max_output > 0
        && message.usage.output < desired_max_output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_boundaries() {
        assert!(mentions_413("413"));
        assert!(mentions_413("413: too big"));
        assert!(mentions_413("custom provider API error (413): nope"));
        assert!(!mentions_413("4130 failed"));
        assert!(!mentions_413("custom-provider API error (4130): nope"));
        assert!(!mentions_413(
            "custom-provider API error (400): upstream mentioned OpenAI API error (413): invalid input"
        ));
    }
}
