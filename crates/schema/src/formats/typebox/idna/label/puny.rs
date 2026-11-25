//! 对应上游 `packages/schema/src/formats/typebox/idna/label/puny.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use super::super::format::puny::{decode, has_non_ascii, is_ace_prefixed};
use super::unicode::is_unicode_label;

/// 值是不是一个合法的 punycode 标签。
pub fn is_puny_label(value: &str) -> bool {
    if !is_ace_prefixed(value) {
        return false;
    }
    let body = value[4..].to_lowercase();
    // 载荷只有一个连字符、或以分隔符开头（像 "-9uc"），意味着分隔符前有 0 个基本 ASCII 字符，
    // 那是坏掉的形状。
    if body.rfind('-') == Some(0) {
        return false;
    }
    let Ok(decoded) = decode(&body) else {
        return false;
    };
    // RFC 5890 §2.3.2.1：U-label 至少要有一个非 ASCII 字符。
    if !has_non_ascii(&decoded) {
        return false;
    }
    is_unicode_label(&decoded)
}
