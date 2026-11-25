use super::inline::parse_inline;
use crate::tui::render::theme::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

pub fn render_table_block(raw_table_lines: &[&str]) -> Vec<Line<'static>> {
    let mut rows: Vec<Vec<&str>> = Vec::new();
    let mut is_sep_flags: Vec<bool> = Vec::new();

    for line in raw_table_lines {
        let trimmed = line.trim();
        let is_sep = trimmed
            .chars()
            .all(|c| c == '|' || c == '-' || c == ':' || c == ' ');
        is_sep_flags.push(is_sep);

        let cells: Vec<&str> = trimmed.split('|').map(|s| s.trim()).collect();

        let start_idx = if cells.first() == Some(&"") { 1 } else { 0 };
        let end_idx = if cells.last() == Some(&"") && cells.len() > start_idx {
            cells.len() - 1
        } else {
            cells.len()
        };

        let row_cells = if start_idx < end_idx {
            cells[start_idx..end_idx].to_vec()
        } else {
            Vec::new()
        };

        rows.push(row_cells);
    }

    if rows.is_empty() {
        return Vec::new();
    }

    let num_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if num_cols == 0 {
        return Vec::new();
    }

    let mut col_widths = vec![3usize; num_cols];
    for (i, row) in rows.iter().enumerate() {
        if is_sep_flags[i] {
            continue;
        }
        for (c, cell) in row.iter().enumerate() {
            if c < num_cols {
                let w = get_cell_rendered_width(cell);
                col_widths[c] = col_widths[c].max(w);
            }
        }
    }

    let mut result_lines = Vec::new();

    // 1. 顶边框: ┌──────────┬──────────┐
    result_lines.push(render_border_line(&col_widths, "┌", "┬", "┐"));

    let mut is_header_done = false;

    // 2. 渲染行
    for (i, row) in rows.iter().enumerate() {
        if is_sep_flags[i] {
            // 中缝分割线: ├──────────┼──────────┤
            result_lines.push(render_border_line(&col_widths, "├", "┼", "┤"));
            is_header_done = true;
            continue;
        }

        let is_header = !is_header_done;
        if is_header {
            is_header_done = true;
        }

        let mut row_spans = vec![Span::styled(
            " │ ",
            Style::default().fg(Theme::COLOR_BORDER),
        )];

        for (c, col_width) in col_widths.iter().enumerate().take(num_cols) {
            if c > 0 {
                row_spans.push(Span::styled(
                    " │ ",
                    Style::default().fg(Theme::COLOR_BORDER),
                ));
            }

            let cell_str = row.get(c).copied().unwrap_or("");
            let cell_w = get_cell_rendered_width(cell_str);
            let pad_len = col_width.saturating_sub(cell_w);

            if is_header {
                row_spans.push(Span::styled(
                    cell_str.to_string(),
                    Style::default()
                        .fg(Theme::COLOR_AI)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                let parsed = parse_inline(cell_str);
                row_spans.extend(parsed.spans);
            }

            if pad_len > 0 {
                row_spans.push(Span::styled(
                    " ".repeat(pad_len),
                    Style::default().fg(Theme::COLOR_TEXT_MAIN),
                ));
            }
        }

        row_spans.push(Span::styled(" │", Style::default().fg(Theme::COLOR_BORDER)));
        result_lines.push(Line::from(row_spans));
    }

    // 3. 底边框: └──────────┴──────────┘
    result_lines.push(render_border_line(&col_widths, "└", "┴", "┘"));

    result_lines
}

fn render_border_line(col_widths: &[usize], left: &str, mid: &str, right: &str) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!(" {left}"),
        Style::default().fg(Theme::COLOR_BORDER),
    )];
    for (c, &w) in col_widths.iter().enumerate() {
        if c > 0 {
            spans.push(Span::styled(
                mid.to_string(),
                Style::default().fg(Theme::COLOR_BORDER),
            ));
        }
        spans.push(Span::styled(
            "─".repeat(w + 2),
            Style::default().fg(Theme::COLOR_BORDER),
        ));
    }
    spans.push(Span::styled(
        right.to_string(),
        Style::default().fg(Theme::COLOR_BORDER),
    ));
    Line::from(spans)
}

fn get_cell_rendered_width(cell_str: &str) -> usize {
    let parsed = parse_inline(cell_str);
    parsed.spans.iter().map(|s| s.content.width()).sum()
}
