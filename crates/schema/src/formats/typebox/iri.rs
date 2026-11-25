//! 对应上游 `packages/schema/src/formats/typebox/iri.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use std::sync::LazyLock;

use regress::Regex;
use url::Url;

const IPV_FUTURE_MATCH_MAX_LENGTH: usize = 2048;

// 受 IPV_FUTURE_MATCH_MAX_LENGTH 限制。
static IPV_FUTURE: LazyLock<Regex> =
    LazyLock::new(|| Regex::with_flags(r"\[[vV][0-9a-fA-F]+\.[^\]]+\]", "").expect("ipv future"));
static INVALID_IRI_CHARS: LazyLock<Regex> =
    LazyLock::new(|| Regex::with_flags(r"[\x00-\x20<>^`{|}\\]", "").expect("invalid iri chars"));
static INVALID_PERCENT_ENCODING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::with_flags(r"%(?![0-9a-fA-F]{2})", "").expect("invalid percent encoding")
});

// NarrowIpvFuture：把 IPvFuture 地址换成标准 IPv6 loopback（`[::1]`）。
//
// 原因是原生 URL.canParse（WHATWG）拒绝 IPvFuture 字面量，而它在 RFC 3987 里是合法的。正则替换在
// 长串上可能很贵，所以只对长度低于阈值的输入做。
fn narrow_ipv_future(value: &str) -> String {
    if value.encode_utf16().count() >= IPV_FUTURE_MATCH_MAX_LENGTH {
        return value.to_string();
    }
    match IPV_FUTURE.find(value) {
        None => value.to_string(),
        Some(matched) => {
            let range = matched.range();
            format!("{}[::1]{}", &value[..range.start], &value[range.end..])
        }
    }
}

/// 值是不是一个合法的国际化资源标识符。
///
/// https://datatracker.ietf.org/doc/html/rfc3987
pub fn is_iri(value: &str) -> bool {
    // 1. 拒绝含未编码空白或非法控制字符的串。
    if INVALID_IRI_CHARS.find(value).is_some() {
        return false;
    }
    // 2. 拒绝坏掉的百分号编码。
    if INVALID_PERCENT_ENCODING.find(value).is_some() {
        return false;
    }
    // 3. 交给原生 URL 解析器，事前补上 IPvFuture 这个边角。
    Url::parse(&narrow_ipv_future(value)).is_ok()
}
