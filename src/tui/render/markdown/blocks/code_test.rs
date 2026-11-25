use super::code::{render_code_line, render_code_toggle};
use ratatui::text::Line;

fn text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// 开栏时带语言:渲染竖条 + 小写语言名
#[test]
fn opening_fence_lowercases_the_language() {
    let line_rust = render_code_toggle("```rust", true).expect("应有开栏行");
    assert_eq!(text(&line_rust), " │ rust");

    let line_bash = render_code_toggle("```bash", true).expect("应有开栏行");
    assert_eq!(text(&line_bash), " │ bash");
}

/// 开栏时不带语言:退回通用 code 标签
#[test]
fn opening_fence_without_language_uses_generic_label() {
    let line = render_code_toggle("```", true).expect("应有开栏行");
    assert_eq!(text(&line), " │ code");
}

/// 收栏不产生多余效果行
#[test]
fn closing_fence_returns_none() {
    assert!(render_code_toggle("```", false).is_none());
}

/// 代码行带竖线前缀,原文缩进一字不改地保留
#[test]
fn code_line_keeps_prefix_and_indentation() {
    let line = render_code_line("    let x = 1;");

    assert_eq!(line.spans.len(), 2);
    assert_eq!(line.spans[0].content, " │ ");
    assert_eq!(line.spans[1].content, "    let x = 1;");
}

/// 代码行内的 markdown 标记不被解析(代码就是代码)
#[test]
fn code_line_does_not_parse_markdown() {
    let line = render_code_line("let s = \"**not bold**\";");

    assert_eq!(line.spans[1].content, "let s = \"**not bold**\";");
}

#[test]
fn empty_code_line_still_gets_prefix() {
    let line = render_code_line("");

    assert_eq!(line.spans[0].content, " │ ");
    assert_eq!(line.spans[1].content, "");
}
