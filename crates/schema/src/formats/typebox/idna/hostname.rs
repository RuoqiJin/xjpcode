//! 对应上游 `packages/schema/src/formats/typebox/idna/hostname.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use super::label::ascii::is_ascii_label;
use super::label::puny::is_puny_label;

fn is_valid_label_length(value: &str) -> bool {
    let length = value.encode_utf16().count();
    length > 0 && length <= 63
}

fn is_label(value: &str) -> bool {
    is_valid_label_length(value) && (is_puny_label(value) || is_ascii_label(value))
}

/// 值是不是一个合法主机名（ASCII 标签与 punycode 标签）。
pub fn is_hostname(value: &str) -> bool {
    let length = value.encode_utf16().count();
    if length == 0 || length > 253 {
        return false;
    }
    if value.ends_with('.') {
        return false;
    }
    value.split('.').all(is_label)
}
