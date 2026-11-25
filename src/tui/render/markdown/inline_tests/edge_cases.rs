use super::fixtures::*;
use crate::tui::render::markdown::inline::parse_inline;
use ratatui::style::Modifier;

/// 未闭合的粗体标记不 panic,且一个字符都不吞
#[test]
fn unclosed_bold_keeps_every_character() {
    let line = parse_inline("**没有收尾");

    assert_eq!(text(&line), "没有收尾");
}

/// 未闭合的反引号同样不 panic
#[test]
fn unclosed_code_does_not_panic() {
    let line = parse_inline("`没有收尾");

    assert_eq!(text(&line), "没有收尾");
}

/// 单个星号是普通字符(乘号、脚注),不得触发粗体分支
#[test]
fn lone_asterisk_is_literal() {
    let line = parse_inline("2 * 3 = 6");

    assert_eq!(text(&line), "2 * 3 = 6");
    assert!(!has(&line, Modifier::BOLD));
}

/// 空粗体标记 **** 不 panic
#[test]
fn empty_bold_marker_does_not_panic() {
    assert_eq!(text(&parse_inline("****")), "");
}

/// 只有反引号也不 panic
#[test]
fn lone_backtick_does_not_panic() {
    assert_eq!(text(&parse_inline("`")), "");
}

/// CJK 正文在标记解析后原样保留
#[test]
fn cjk_around_markers_survives() {
    let line = parse_inline("中文**粗体**中文`代码`结尾");

    assert_eq!(text(&line), "中文粗体中文代码结尾");
}
