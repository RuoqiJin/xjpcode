use super::list::{try_render_ordered_list, try_render_quote, try_render_unordered_list};
use ratatui::text::Line;

fn text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

// —— 引用块 ——

#[test]
fn quote_gets_vertical_bar_prefix() {
    let line = try_render_quote("> 引用内容").expect("应识别为引用");

    assert_eq!(line.spans[0].content, " │ ");
    assert_eq!(text(&line), " │ 引用内容");
}

/// 尖括号后必须有空格,否则不是引用(HTML 片段之类不误判)
#[test]
fn quote_requires_space_after_marker() {
    assert!(try_render_quote(">没有空格").is_none());
    assert!(try_render_quote("普通文本").is_none());
}

// —— 无序列表 ——

#[test]
fn unordered_list_accepts_all_three_markers() {
    for raw in ["- 项目", "* 项目", "+ 项目"] {
        let line = try_render_unordered_list(raw).unwrap_or_else(|| panic!("{raw} 应识别为列表"));
        assert_eq!(line.spans[0].content, " • ", "输入 {raw}");
        assert_eq!(text(&line), " • 项目");
    }
}

/// 标记后必须有空格,否则不是列表(-1、*ptr 之类不误判)
#[test]
fn unordered_list_requires_space_after_marker() {
    assert!(try_render_unordered_list("-1 摄氏度").is_none());
    assert!(try_render_unordered_list("*ptr").is_none());
    assert!(try_render_unordered_list("普通文本").is_none());
}

#[test]
fn unordered_list_body_parses_inline_markup() {
    let line = try_render_unordered_list("- 带 **重点** 的项").expect("应识别为列表");

    assert!(!text(&line).contains("**"));
}

// —— 有序列表 ——

#[test]
fn ordered_list_keeps_the_number() {
    let line = try_render_ordered_list("1. 第一项").expect("应识别为有序列表");

    assert_eq!(line.spans[0].content, " 1. ");
    assert_eq!(text(&line), " 1. 第一项");
}

#[test]
fn ordered_list_accepts_multi_digit_numbers() {
    let line = try_render_ordered_list("12. 第十二项").expect("应识别为有序列表");

    assert_eq!(line.spans[0].content, " 12. ");
}

/// 序号前缀必须全是数字:普通句子里的句点不得被当成列表
#[test]
fn ordered_list_rejects_non_digit_prefix() {
    assert!(try_render_ordered_list("Hello. World").is_none());
    assert!(try_render_ordered_list("v1. 版本").is_none());
}

/// 点号后必须有空格
#[test]
fn ordered_list_requires_space_after_dot() {
    assert!(try_render_ordered_list("1.紧挨着").is_none());
}

/// 空序号前缀(以 ". " 开头)不是列表
#[test]
fn ordered_list_rejects_empty_prefix() {
    assert!(try_render_ordered_list(". 无序号").is_none());
}

#[test]
fn ordered_list_ignores_plain_text() {
    assert!(try_render_ordered_list("没有点号的普通文本").is_none());
    assert!(try_render_ordered_list("").is_none());
}
