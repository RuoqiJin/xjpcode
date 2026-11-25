use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

/// 一行渲染后的终端显示宽度(CJK 按双宽计)
pub fn line_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|s| s.content.width()).sum()
}

pub fn plain_text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// 表格"不塌"的判据:所有行(含边框)显示宽度完全一致,竖线才能对齐成一列。
pub fn assert_all_lines_same_width(lines: &[Line<'_>]) {
    let widths: Vec<usize> = lines.iter().map(line_width).collect();
    let expected = widths[0];
    for (i, w) in widths.iter().enumerate() {
        assert_eq!(
            *w,
            expected,
            "第 {i} 行宽度 {w} 与首行 {expected} 不一致,表格已塌:\n{}",
            lines
                .iter()
                .map(|l| plain_text(l))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
