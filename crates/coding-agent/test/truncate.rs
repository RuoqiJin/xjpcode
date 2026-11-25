//! 对应上游 `test/truncate-surrogates.test.ts`：切字符串时不把代理对切开。

use xjpcode_coding_agent::core::tools::truncate::{slice_head, slice_tail, truncate_line};

const EMOJI: &str = "\u{1F600}"; // 两个 UTF-16 码元

fn has_lone_surrogate(text: &str) -> bool {
    let units: Vec<u16> = text.encode_utf16().collect();
    for (index, unit) in units.iter().enumerate() {
        let is_high = (0xd800..=0xdbff).contains(unit);
        let is_low = (0xdc00..=0xdfff).contains(unit);
        if is_high && units.get(index + 1).is_none_or(|next| !(0xdc00..=0xdfff).contains(next)) {
            return true;
        }
        if is_low && (index == 0 || !(0xd800..=0xdbff).contains(&units[index - 1])) {
            return true;
        }
    }
    false
}

#[test]
fn truncate_line_does_not_cut_an_emoji_in_half_at_the_limit() {
    let line = format!("{}{EMOJI}tail", "a".repeat(499));
    let (text, was_truncated) = truncate_line(&line, 500);
    assert!(was_truncated);
    assert_eq!(text, format!("{}... [truncated]", "a".repeat(499)));
    assert!(!has_lone_surrogate(&text));
}

#[test]
fn truncate_line_keeps_an_emoji_that_ends_exactly_at_the_limit() {
    let line = format!("{}{EMOJI}tail", "a".repeat(498));
    assert_eq!(
        truncate_line(&line, 500).0,
        format!("{}{EMOJI}... [truncated]", "a".repeat(498))
    );
}

#[test]
fn slice_head_and_tail_never_split_a_pair() {
    let text = format!("{EMOJI}x{EMOJI}");
    let length = text.encode_utf16().count();
    for units in 0..=length + 1 {
        let head = slice_head(&text, units);
        let tail = slice_tail(&text, units);
        assert!(!has_lone_surrogate(&head));
        assert!(!has_lone_surrogate(&tail));
        assert!(text.starts_with(&head));
        assert!(text.ends_with(&tail));
        assert!(head.encode_utf16().count() + 1 >= units.min(length));
        assert!(tail.encode_utf16().count() + 1 >= units.min(length));
    }
}

#[test]
fn slice_head_and_tail_match_slice_on_text_without_surrogates() {
    assert_eq!(slice_head("abcdef", 3), "abc");
    assert_eq!(slice_tail("abcdef", 3), "def");
    assert_eq!(slice_head("abc", 10), "abc");
    assert_eq!(slice_tail("abc", 10), "abc");
    assert_eq!(slice_head("abc", 0), "");
    assert_eq!(slice_tail("abc", 0), "");
}
