//! 对应上游 `packages/schema/src/formats/typebox/idn_email.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use std::sync::LazyLock;

use regress::Regex;
use unicode_normalization::UnicodeNormalization;

static IDN_EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::with_flags(
        r#"^(?:[A-Za-z0-9!#$%&'*+/=?^_`{|}~\u{0080}-\u{10FFFF}-]+(?:\.[A-Za-z0-9!#$%&'*+/=?^_`{|}~\u{0080}-\u{10FFFF}-]+)*|"(?:[^"\\]|\\.)*")@[\p{L}\p{N}](?:[\p{L}\p{N}-]{0,62})(?<!-)(?:\.[\p{L}\p{N}](?:[\p{L}\p{N}-]{0,62})(?<!-))*$"#,
        "iu",
    )
    .expect("idn email pattern")
});

/// 值是不是一个 IdnEmail（先按 NFC 归一再比）。
///
/// https://datatracker.ietf.org/doc/html/rfc4952
pub fn is_idn_email(value: &str) -> bool {
    let normalized: String = value.nfc().collect();
    IDN_EMAIL.find(&normalized).is_some()
}
