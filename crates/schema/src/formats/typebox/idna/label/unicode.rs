//! 对应上游 `packages/schema/src/formats/typebox/idna/label/unicode.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use regress::Regex;

use super::super::format::bidi::{has_right_to_left_characters, satisfies_bidi_rule};
use super::super::format::puny::encode;
use super::super::pattern::pattern::{
    RE_COMBINING_MARK, RE_NON_ASCII, RE_PERMITTED_CATEGORY, RE_RFC5892_DISALLOWED, RE_SCRIPT_GREEK,
    RE_SCRIPT_HEBREW, RE_SCRIPT_JAPANESE, RE_VIRAMA,
};

fn matches(pattern: &Regex, ch: char) -> bool {
    let mut buffer = [0u8; 4];
    pattern.find(ch.encode_utf8(&mut buffer)).is_some()
}

// RFC 5890 §2.3.2.1：A-label（标签的 ACE 编码形状，"xn--" + punycode）不得超过 63 字节。只有含
// 非 ASCII 字符的标签需要查——纯 ASCII 标签没有 "xn--" 前缀、原样使用，编码后的长度无关。先做
// ASCII 判断是便宜的短路，省掉不需要编码的标签。
fn exceeds_max_a_label_length(value: &str) -> bool {
    RE_NON_ASCII.find(value).is_some() && encode(value).chars().count() + 4 > 63
}

// RFC 5891 §4.2.3.2：标签不得以连字符开头或结尾，第 3、4 个码点位置也不能是连字符（ACE 前缀
// 保留，例如 "xn--"）。按码点数组而不是原始串下标判，前面有增补平面字符时码点位置也不会错。
fn has_invalid_hyphens(chars: &[char]) -> bool {
    if chars.first() == Some(&'-') || chars.last() == Some(&'-') {
        return true;
    }
    chars.iter().skip(2).collect::<String>().starts_with("--")
}

/// 值是不是一个合法的 Unicode 标签。
pub fn is_unicode_label(value: &str) -> bool {
    // RFC 5890 §2.3.2.1：先保证最大长度。
    if exceeds_max_a_label_length(value) {
        return false;
    }
    // RFC 5893：这个标签自己是 RTL 时适用 Bidi 规则。
    if has_right_to_left_characters(value) && !satisfies_bidi_rule(value) {
        return false;
    }
    let chars: Vec<char> = value.chars().collect();
    let code_points: Vec<u32> = chars.iter().map(|ch| *ch as u32).collect();
    // RFC 5891 §4.2.3.2：连字符规则。
    if has_invalid_hyphens(&chars) {
        return false;
    }
    // RFC 5891 §4.2.3.2：不得以组合标记开头。
    if chars
        .first()
        .is_some_and(|ch| matches(&RE_COMBINING_MARK, *ch))
    {
        return false;
    }
    let mut has_japanese = false;
    for (i, &code_point) in code_points.iter().enumerate() {
        let ch = chars[i];
        // 1. DISALLOWED 例外。
        if matches(&RE_RFC5892_DISALLOWED, ch) {
            return false;
        }
        // 1b. 通用类别（PVALID）约束。
        if !matches(&RE_PERMITTED_CATEGORY, ch) {
            return false;
        }
        // 2. 收集标记。
        if matches(&RE_SCRIPT_JAPANESE, ch) {
            has_japanese = true;
        }
        // 3. CONTEXTO / CONTEXTJ 的邻居规则。
        let prev = i.checked_sub(1).map(|index| code_points[index]);
        let next = code_points.get(i + 1).copied();
        match code_point {
            // MIDDLE DOT（加泰罗尼亚语）
            0x00b7 => {
                if prev != Some(0x006c) || next != Some(0x006c) {
                    return false;
                }
            }
            // Greek KERAIA
            0x0375 => match next {
                None => return false,
                Some(_) => {
                    if !matches(&RE_SCRIPT_GREEK, chars[i + 1]) {
                        return false;
                    }
                }
            },
            // Hebrew GERESH
            0x05f3 | 0x05f4 => match prev {
                None => return false,
                Some(_) => {
                    if !matches(&RE_SCRIPT_HEBREW, chars[i - 1]) {
                        return false;
                    }
                }
            },
            // ZWNJ——RFC 5892 附录 A.1。
            // 注意：完整校验需要 Unicode 连接类型表。这里只在前面是 ASCII（U+0000-U+007F）时
            // 拒绝，那种情况永远满足不了 Virama 或连写规则。
            0x200c => match prev {
                None => return false,
                Some(prev) => {
                    if prev < 0x0080 && !matches(&RE_VIRAMA, chars[i - 1]) {
                        return false;
                    }
                }
            },
            // ZWJ：RFC 5892 附录 A.2。
            0x200d => match prev {
                None => return false,
                Some(_) => {
                    if !matches(&RE_VIRAMA, chars[i - 1]) {
                        return false;
                    }
                }
            },
            // KATAKANA MIDDLE DOT：循环后由 has_japanese 统一判。
            0x30fb => {}
            _ => {}
        }
    }
    // 4. 全局上下文校验（循环之后）。
    // RFC 5892 附录 A.7：片假名中点要求标签里有日文字符。
    if value.contains('\u{30fb}') && !has_japanese {
        return false;
    }
    true
}
