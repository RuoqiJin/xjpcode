use super::fixtures::*;
use crate::tui::render::markdown::inline::parse_inline;
use ratatui::style::Modifier;

#[test]
fn plain_text_becomes_one_span() {
    let line = parse_inline("普通一行 text");

    assert_eq!(line.spans.len(), 1);
    assert_eq!(text(&line), "普通一行 text");
    assert!(!has(&line, Modifier::BOLD));
}

#[test]
fn empty_line_has_no_spans() {
    assert!(parse_inline("").spans.is_empty());
}

// —— 粗体 ——

#[test]
fn bold_markers_are_stripped_and_styled() {
    let line = parse_inline("前 **重点** 后");

    assert_eq!(text(&line), "前 重点 后");
    assert!(has(&line, Modifier::BOLD));
    let bold = line
        .spans
        .iter()
        .find(|s| s.style.add_modifier.contains(Modifier::BOLD))
        .expect("应有粗体 span");
    assert_eq!(bold.content, "重点");
}

#[test]
fn multiple_bold_segments_in_one_line() {
    let line = parse_inline("**a** 与 **b**");

    assert_eq!(text(&line), "a 与 b");
    let bold_count = line
        .spans
        .iter()
        .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
        .count();
    assert_eq!(bold_count, 2);
}

// —— 行内代码 ——

/// 行内代码采用 Tag 胶囊背景高亮风格
#[test]
fn inline_code_is_padded_and_styled() {
    let line = parse_inline("看 `cargo test` 的输出");

    assert_eq!(text(&line), "看 cargo test 的输出");
    assert!(line.spans.iter().any(|s| s.content == "cargo test"));
}

/// **`code`** 组合走独立分支,无下划线，采用 Tag 胶囊背景
#[test]
fn bold_code_combo_uses_tag_style() {
    let line = parse_inline("**`xj`** 是二进制名");

    assert_eq!(text(&line), "xj 是二进制名");
    assert!(!has(&line, Modifier::UNDERLINED), "**`code`** 不应带下划线");
    assert!(has(&line, Modifier::BOLD));
}

// —— 斜体 ——

#[test]
fn italic_markers_are_stripped_and_styled() {
    let line = parse_inline("中文 *斜体* 结尾");

    assert_eq!(text(&line), "中文 斜体 结尾");
    assert!(has(&line, Modifier::ITALIC));
}
