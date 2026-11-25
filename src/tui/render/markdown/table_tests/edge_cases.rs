use super::fixtures::*;
use crate::tui::render::markdown::table::render_table_block;

/// 单元格数不齐:短行补空单元格,长行决定列数,都不得 panic 或塌列。
#[test]
fn ragged_rows_are_padded_to_the_widest_row() {
    let lines = render_table_block(&[
        "| a | b | c |",
        "|---|---|---|",
        "| 1 |",
        "| 1 | 2 | 3 | 4 |",
    ]);

    assert_all_lines_same_width(&lines);
}

/// 空输入不渲染任何东西(不出孤零零的边框)
#[test]
fn empty_input_renders_nothing() {
    assert!(render_table_block(&[]).is_empty());
}

/// 只有分隔行时无有效列宽来源,但仍应产出结构完整、宽度一致的框
#[test]
fn separator_only_input_does_not_panic() {
    let lines = render_table_block(&["|---|---|"]);

    assert_all_lines_same_width(&lines);
}
