//! 对应上游 `packages/schema/src/formats/typebox/ipv4.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use std::sync::LazyLock;

use regress::Regex;

static IPV4: LazyLock<Regex> = LazyLock::new(|| {
    Regex::with_flags(
        r#"^(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)$"#,
        "",
    )
    .expect("IPv4 format pattern")
});

/// Returns true if the value is a IPV4 address
pub fn is_ipv4(value: &str) -> bool {
    IPV4.find(value).is_some()
}
