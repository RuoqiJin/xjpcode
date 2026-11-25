//! 对应上游 `packages/schema/src/formats/typebox/uri_template.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use std::sync::LazyLock;

use regress::Regex;

static URI_TEMPLATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::with_flags(
        r##"^(?:(?:[^\x00-\x20"<>%\\^`{|}\x7f]|%[0-9a-f]{2})|\{[+#./;?&=,!@|]?(?:[a-z0-9_]|%[0-9a-f]{2})+(?:\.(?:[a-z0-9_]|%[0-9a-f]{2})+)*(?::[1-9]\d{0,3}|\*)?(?:,(?:[a-z0-9_]|%[0-9a-f]{2})+(?:\.(?:[a-z0-9_]|%[0-9a-f]{2})+)*(?::[1-9]\d{0,3}|\*)?)*\})*$"##,
        "i",
    )
    .expect("UriTemplate format pattern")
});

/// Returns true if the value is a valid Uri Template
pub fn is_uri_template(value: &str) -> bool {
    URI_TEMPLATE.find(value).is_some()
}
