//! 对应上游 `packages/schema/src/formats/typebox/idna/format/bidi.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。
//!
//! 上游用正则判断“这个 Bidi 类在不在允许集里”（`/^(?:R|AL|AN|EN|ES|CS|ET|ON|BN|NSM)$/` 之类）。
//! 那几个类名都是本文件自己产出的固定字符串，Rust 侧直接按名字匹配，判定相同。

use super::super::pattern::pattern::{
    RE_ARABIC_INDIC_DIGIT, RE_EUROPEAN_NUMBER, RE_LETTER, RE_MARK_NONSPACING,
    RE_SCRIPT_ARABIC_LETTER, RE_SCRIPT_HEBREW,
};
use super::puny::{decode, is_ace_prefixed};

/// 一个字符的 Bidi 类名（RFC 5893 用到的那些）。
fn bidi_class(code_point: u32) -> &'static str {
    let Ok(ch) = char::try_from(code_point) else {
        return "ON";
    };
    let mut buffer = [0u8; 4];
    let text = ch.encode_utf8(&mut buffer);
    if RE_EUROPEAN_NUMBER.find(text).is_some() {
        "EN"
    } else if RE_ARABIC_INDIC_DIGIT.find(text).is_some() {
        "AN"
    } else if RE_MARK_NONSPACING.find(text).is_some() {
        "NSM"
    } else if RE_SCRIPT_HEBREW.find(text).is_some() {
        "R"
    } else if RE_SCRIPT_ARABIC_LETTER.find(text).is_some() {
        "AL"
    } else if RE_LETTER.find(text).is_some() {
        "L"
    } else {
        "ON"
    }
}

/// 这个方向允许的类（RFC 5893）。
fn is_allowed(is_rtl: bool, class: &str) -> bool {
    if is_rtl {
        matches!(
            class,
            "R" | "AL" | "AN" | "EN" | "ES" | "CS" | "ET" | "ON" | "BN" | "NSM"
        )
    } else {
        matches!(class, "L" | "EN" | "ES" | "CS" | "ET" | "ON" | "BN" | "NSM")
    }
}

fn is_rtl_class(class: &str) -> bool {
    matches!(class, "R" | "AL" | "AN")
}

/// 值是不是一个 RTL 标签（RFC 5893 §1.4）：是 punycode 标签就先解回 Unicode。
pub fn has_bidi_chars(value: &str) -> bool {
    if is_ace_prefixed(value) {
        let payload = value[4..].to_lowercase();
        match decode(&payload) {
            Ok(decoded) => has_right_to_left_characters(&decoded),
            Err(_) => false,
        }
    } else {
        has_right_to_left_characters(value)
    }
}

/// RFC 5893 §1.4：RTL 标签是至少含一个 R、AL 或 AN 类字符的标签。用来判断一个域名是不是
/// “Bidi 域名”，是的话 Bidi 规则适用于其中每个标签。
pub fn has_right_to_left_characters(value: &str) -> bool {
    value.chars().any(|ch| is_rtl_class(bidi_class(ch as u32)))
}

/// RFC 5893 的 Bidi 规则。
pub fn satisfies_bidi_rule(value: &str) -> bool {
    let mut is_rtl = false;
    let mut saw_en = false;
    let mut saw_an = false;
    let mut is_first = true;
    for ch in value.chars() {
        let class = bidi_class(ch as u32);
        if is_first {
            // 1. 首字符必须是 Bidi 属性为 L、R 或 AL 的字符。
            if class != "L" && class != "R" && class != "AL" {
                return false;
            }
            is_rtl = class == "R" || class == "AL";
            is_first = false;
        }
        // 2 / 5. 只允许这个方向允许集里的字符。
        if !is_allowed(is_rtl, class) {
            return false;
        }
        if class == "EN" {
            saw_en = true;
        } else if class == "AN" {
            saw_an = true;
        }
    }
    if is_rtl && saw_en && saw_an {
        return false;
    }
    true
}
