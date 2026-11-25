use crate::tui::render::theme::Theme;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

pub fn highlight_line(
    line: &Line<'static>,
    line_y: u16,
    area_x: u16,
    range: ((u16, u16), (u16, u16)),
) -> Line<'static> {
    let ((_, y1), (_, y2)) = range;

    if line_y < y1 || line_y > y2 {
        return line.clone();
    }

    let sel_style = Style::default()
        .bg(Theme::COLOR_SELECTION_BG)
        .fg(Theme::COLOR_SELECTION_FG);

    let mut new_spans = Vec::new();
    let mut current_col = area_x + 1;

    for span in &line.spans {
        let content = &span.content;
        let mut sub_content = String::new();
        let mut is_sub_selected = false;

        for ch in content.chars() {
            let ch_str = ch.to_string();
            let ch_w = ch_str.as_str().width() as u16;
            let ch_end_col = current_col + ch_w;

            let in_selection = super::predicate::is_point_in_selection(line_y, current_col, range);

            if sub_content.is_empty() {
                is_sub_selected = in_selection;
                sub_content.push(ch);
            } else if is_sub_selected == in_selection {
                sub_content.push(ch);
            } else {
                let style = if is_sub_selected {
                    span.style.patch(sel_style)
                } else {
                    span.style
                };
                new_spans.push(Span::styled(sub_content.clone(), style));
                sub_content.clear();
                is_sub_selected = in_selection;
                sub_content.push(ch);
            }

            current_col = ch_end_col;
        }

        if !sub_content.is_empty() {
            let style = if is_sub_selected {
                span.style.patch(sel_style)
            } else {
                span.style
            };
            new_spans.push(Span::styled(sub_content, style));
        }
    }

    Line::from(new_spans)
}
