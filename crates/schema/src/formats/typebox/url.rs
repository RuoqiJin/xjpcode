//! 对应上游 `packages/schema/src/formats/typebox/url.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use url::Url;

/// 值是不是一个合法 URL。
///
/// https://datatracker.ietf.org/doc/html/rfc3986 、https://datatracker.ietf.org/doc/html/rfc3987 。
///
/// 上游是 `URL.canParse(value)`（WHATWG），Rust 侧用同样按 WHATWG 实现的 `url` crate，见
/// `PORTING.md` 差异表。
pub fn is_url(value: &str) -> bool {
    Url::parse(value).is_ok()
}
