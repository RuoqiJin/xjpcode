use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// 将单行 Line 按 max_width 显示宽度拆分成多行 Line（视觉折行预处理）
pub fn wrap_line(line: &Line<'static>, max_width: usize) -> Vec<Line<'static>> {
    if max_width == 0 {
        return vec![line.clone()];
    }

    let mut result = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut current_width: usize = 0;

    for span in &line.spans {
        let content = &span.content;
        let style = span.style;
        let mut cur_buf = String::new();

        for ch in content.chars() {
            let ch_str = ch.to_string();
            let ch_w = ch_str.as_str().width();

            if current_width + ch_w > max_width && current_width > 0 {
                if !cur_buf.is_empty() {
                    current_spans.push(Span::styled(cur_buf.clone(), style));
                    cur_buf.clear();
                }
                result.push(Line::from(current_spans));
                current_spans = Vec::new();
                current_width = 0;
            }

            cur_buf.push(ch);
            current_width += ch_w;
        }

        if !cur_buf.is_empty() {
            current_spans.push(Span::styled(cur_buf, style));
        }
    }

    if !current_spans.is_empty() || result.is_empty() {
        result.push(Line::from(current_spans));
    }

    result
}

/// 计算单行 Line 按 max_width 显示宽度折行后的实际视觉行数（与 wrap_line 保持 100% 相同物理分行规则）
pub fn compute_visual_line_count(line: &Line<'_>, max_width: usize) -> usize {
    if max_width == 0 {
        return 1;
    }

    let mut lines_count = 0;
    let mut current_width = 0;

    for span in &line.spans {
        for ch in span.content.chars() {
            let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);

            if current_width + ch_w > max_width && current_width > 0 {
                lines_count += 1;
                current_width = 0;
            }
            current_width += ch_w;
        }
    }

    if current_width > 0 || lines_count == 0 {
        lines_count += 1;
    }

    lines_count
}

/// 批量折行：将 Lines 列表全部预切分为宽度不超过 max_width 的视觉行
pub fn wrap_lines(lines: &[Line<'static>], max_width: usize) -> Vec<Line<'static>> {
    let mut wrapped = Vec::new();
    for line in lines {
        wrapped.extend(wrap_line(line, max_width));
    }
    wrapped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_visual_line_count_matches_wrap_line() {
        let line = Line::from("ab你好");
        let inner_width = 3;
        // "ab" (2) + "你" (2) > 3 -> "ab" (row 1)
        // "你" (2) + "好" (2) > 3 -> "你" (row 2)
        // "好" (2) (row 3)
        // wrap_line produces 3 lines, whereas ceil(6 / 3) would produce 2 lines.
        let wrapped = wrap_line(&line, inner_width);
        let count = compute_visual_line_count(&line, inner_width);
        assert_eq!(wrapped.len(), 3);
        assert_eq!(count, 3);
    }
}
