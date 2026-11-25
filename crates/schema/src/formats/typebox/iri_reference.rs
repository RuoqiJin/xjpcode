//! 对应上游 `packages/schema/src/formats/typebox/iri_reference.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use std::sync::LazyLock;

use regress::Regex;
use url::Url;

/// 一对引用的基地址，与上游 `URL.canParse(value, "http://example.com")` 的 base 相同。
const BASE: &str = "http://example.com";

// 拒绝未编码空格（0x20）、反斜杠、控制字符（0x00-0x1F、0x7F）和坏掉的百分号编码。
static INVALID_IRI_CHARS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::with_flags(r"[\x00-\x20\x7F\\]|%(?![0-9a-fA-F]{2})", "").expect("invalid iri chars")
});
// 检出缺少冒号的坏 scheme/authority 形状（例如 "httpx//example.com"）。
static MALFORMED_SCHEME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::with_flags(r"^[a-zA-Z][a-zA-Z0-9+\-.]*\/\/", "").expect("malformed scheme")
});

/// 值是不是一个合法的 IRI 引用。
///
/// https://tools.ietf.org/html/rfc3987
pub fn is_iri_reference(value: &str) -> bool {
    if INVALID_IRI_CHARS.find(value).is_some() || MALFORMED_SCHEME.find(value).is_some() {
        return false;
    }
    let base = Url::parse(BASE).expect("iri reference base");
    base.join(value).is_ok()
}
