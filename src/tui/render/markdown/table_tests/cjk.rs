use super::fixtures::*;
use crate::tui::render::markdown::table::render_table_block;

/// CJK 列按显示宽度对齐:中文单元格不得把列挤塌。
#[test]
fn cjk_columns_stay_aligned() {
    let lines = render_table_block(&[
        "| 名称 | 说明 |",
        "|------|------|",
        "| 中文很长的一列 | ok |",
        "| a | 短 |",
    ]);

    assert_all_lines_same_width(&lines);
}

/// 中英混排同样对齐
#[test]
fn mixed_width_columns_stay_aligned() {
    let lines = render_table_block(&[
        "| Key | 描述 |",
        "|-----|------|",
        "| timeout | 超时时间(秒) |",
        "| 重试 | retry count |",
    ]);

    assert_all_lines_same_width(&lines);
}

/// 行内标记在数据单元格里被解析,宽度按解析后的显示宽度计。
#[test]
fn inline_markup_in_body_cells_stays_aligned() {
    let lines = render_table_block(&[
        "| Key | Note |",
        "|-----|------|",
        "| `code` | **bold** |",
        "| plain | 普通 |",
    ]);

    assert_all_lines_same_width(&lines);
    // 数据单元格里的 ** 被吃掉,不该原样出现在渲染结果里
    assert!(!plain_text(&lines[3]).contains("**"));
}
