use super::fixtures::*;
use crate::tui::render::markdown::table::render_table_block;

/// 标准三段式表格:上边框 + 表头 + 中缝 + 数据行 + 下边框
#[test]
fn standard_table_renders_three_borders() {
    let lines = render_table_block(&[
        "| Name | Value |",
        "|------|-------|",
        "| a | 1 |",
        "| b | 2 |",
    ]);

    assert_eq!(
        lines.len(),
        6,
        "2 数据行应渲染成 6 行(3 边框 + 表头 + 2 行)"
    );
    assert!(plain_text(&lines[0]).starts_with(" ┌"));
    assert!(plain_text(&lines[2]).starts_with(" ├"), "分隔行应变中缝");
    assert!(plain_text(&lines[5]).starts_with(" └"));
    assert_all_lines_same_width(&lines);
}

/// 没有分隔行的表格照样渲染(首行仍当表头),只是不出中缝。
#[test]
fn table_without_separator_row_still_renders() {
    let lines = render_table_block(&["| a | b |", "| 1 | 2 |"]);

    assert_eq!(lines.len(), 4, "无分隔行:上边框 + 2 行 + 下边框");
    assert!(!plain_text(&lines[1]).starts_with(" ├"));
    assert_all_lines_same_width(&lines);
}

/// 窄内容也有最小列宽(3),避免出现贴脸的 1 字符列
#[test]
fn narrow_column_gets_minimum_width() {
    let lines = render_table_block(&["| a |", "|---|", "| 1 |"]);

    assert_all_lines_same_width(&lines);
    // " ┌" + "─"*(3+2) + "┐" = 8
    assert_eq!(line_width(&lines[0]), 8);
}
