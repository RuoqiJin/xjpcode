use crate::tui::render::markdown::inline::parse_inline;
use crate::tui::render::theme::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

pub fn try_render_quote(trimmed: &str) -> Option<Line<'static>> {
    if let Some(quote_body) = trimmed.strip_prefix("> ") {
        let parsed = parse_inline(quote_body);
        let mut spans = vec![Span::styled(
            " │ ",
            Style::default().fg(Theme::COLOR_BORDER_ACTIVE),
        )];
        spans.extend(parsed.spans);
        Some(Line::from(spans))
    } else {
        None
    }
}

pub fn try_render_unordered_list(trimmed: &str) -> Option<Line<'static>> {
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
        let list_body = &trimmed[2..];
        let parsed_line = parse_inline(list_body);
        let mut spans = vec![Span::styled(
            " • ",
            Style::default()
                .fg(Theme::COLOR_AI)
                .add_modifier(Modifier::BOLD),
        )];
        spans.extend(parsed_line.spans);
        Some(Line::from(spans))
    } else {
        None
    }
}

pub fn try_render_ordered_list(trimmed: &str) -> Option<Line<'static>> {
    if let Some(dot_pos) = trimmed.find(". ") {
        let prefix = &trimmed[..dot_pos];
        if !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_digit()) {
            let num_str = format!(" {}. ", prefix);
            let list_body = &trimmed[dot_pos + 2..];
            let parsed_line = parse_inline(list_body);
            let mut spans = vec![Span::styled(
                num_str,
                Style::default()
                    .fg(Theme::COLOR_AI)
                    .add_modifier(Modifier::BOLD),
            )];
            spans.extend(parsed_line.spans);
            return Some(Line::from(spans));
        }
    }
    None
}
