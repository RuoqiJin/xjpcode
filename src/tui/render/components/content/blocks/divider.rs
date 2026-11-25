use crate::tui::render::theme::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// 轮次极简分隔线（方案一：嵌入式淡色细线）
pub fn draw(round: usize, width: usize, content_lines: &mut Vec<Line<'static>>) {
    if width == 0 {
        return;
    }
    let label = format!(" Round {} ", round);
    let label_len = label.as_str().width();

    if width <= label_len {
        content_lines.push(Line::from(Span::styled(
            label,
            Style::default()
                .fg(Theme::COLOR_TEXT_DIM)
                .add_modifier(Modifier::BOLD),
        )));
        return;
    }

    let remaining = width - label_len;
    let left_dashes = remaining / 2;
    let right_dashes = remaining - left_dashes;

    let line_style = Style::default().fg(Theme::COLOR_BORDER);
    let label_style = Style::default()
        .fg(Theme::COLOR_TEXT_DIM)
        .add_modifier(Modifier::BOLD);

    content_lines.push(Line::from(vec![
        Span::styled("─".repeat(left_dashes), line_style),
        Span::styled(label, label_style),
        Span::styled("─".repeat(right_dashes), line_style),
    ]));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divider_line_matches_width() {
        let mut lines = Vec::new();
        draw(2, 40, &mut lines);
        assert_eq!(lines.len(), 1);
        let line_w: usize = lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref().width())
            .sum();
        assert_eq!(line_w, 40);
    }
}
