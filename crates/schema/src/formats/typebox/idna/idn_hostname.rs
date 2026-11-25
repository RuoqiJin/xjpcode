//! 对应上游 `packages/schema/src/formats/typebox/idna/idn-hostname.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use unicode_normalization::UnicodeNormalization;

use super::format::bidi::{has_bidi_chars, satisfies_bidi_rule};
use super::label::puny::is_puny_label;
use super::label::unicode::is_unicode_label;

fn is_valid_label_length(value: &str) -> bool {
    let length = value.encode_utf16().count();
    length > 0 && length <= 63
}

fn is_label(value: &str) -> bool {
    is_valid_label_length(value) && (is_puny_label(value) || is_unicode_label(value))
}

// 把主机名归一后交给标签校验：先把全角码点映射成 ASCII（例如 'ａ' U+FF41 映射到 'a'），再按 NFC
// 归一，让等价的码点序列可以统一比较；然后去掉 IDNA 映射忽略的码点，并把各种句号变体换成 ASCII
// '.'，这样主机名能按标签切分。
fn normalize_hostname(value: &str) -> String {
    let fullwidth_mapped: String = value
        .chars()
        .map(|ch| {
            // RE_FULLWIDTH_MAPPING
            if ('\u{ff01}'..='\u{ff5e}').contains(&ch) {
                char::from_u32(ch as u32 - 0xfee0).unwrap_or(ch)
            } else {
                ch
            }
        })
        .collect();
    let normalized: String = fullwidth_mapped.nfc().collect();
    // RE_IGNORED_MAPPING：一组单个码点，按设计去掉。
    let ignored: String = normalized
        .chars()
        .filter(|ch| !is_ignored_mapping(*ch))
        .collect();
    // RE_FULL_STOP_MAPPING
    ignored
        .chars()
        .map(|ch| {
            if matches!(ch, '\u{002e}' | '\u{3002}' | '\u{ff0e}' | '\u{ff61}') {
                '.'
            } else {
                ch
            }
        })
        .collect()
}

fn is_ignored_mapping(ch: char) -> bool {
    matches!(
        ch,
        '\u{00ad}'
            | '\u{034f}'
            | '\u{180b}'..='\u{180d}'
            | '\u{200b}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{e0100}'..='\u{e01ef}'
    )
}

/// 值是不是一个合法的国际化（IDN）主机名。
pub fn is_idn_hostname(value: &str) -> bool {
    if value.is_empty() || value.contains(' ') {
        return false;
    }
    let normalized = normalize_hostname(value);
    if normalized.encode_utf16().count() > 253 {
        return false;
    }
    let labels: Vec<&str> = normalized.split('.').collect();
    let has_bidi = labels.iter().any(|label| has_bidi_chars(label));
    labels
        .iter()
        .all(|label| is_label(label) && (!has_bidi || satisfies_bidi_rule(label)))
}
