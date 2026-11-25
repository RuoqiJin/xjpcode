use crate::tui::render::markdown::inline::parse_inline;
use crate::tui::render::theme::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

pub fn try_render_hr(trimmed: &str, in_code_block: bool) -> Option<Line<'static>> {
    if !in_code_block
        && (trimmed == "---"
            || trimmed == "***"
            || trimmed == "___"
            || (trimmed.len() >= 3 && trimmed.chars().all(|c| c == '-')))
    {
        Some(Line::from(vec![Span::styled(
            "────────────────────────────────────────────────────",
            Style::default().fg(Theme::COLOR_BORDER),
        )]))
    } else {
        None
    }
}

pub fn try_render_header(trimmed: &str) -> Option<Line<'static>> {
    let header_prefix = if trimmed.starts_with("# ") {
        Some((Theme::COLOR_AI, 2))
    } else if trimmed.starts_with("## ") {
        Some((Theme::COLOR_USER, 3))
    } else if trimmed.starts_with("### ") {
        Some((Theme::COLOR_TEXT_MAIN, 4))
    } else {
        None
    };

    if let Some((color, skip_len)) = header_prefix {
        let title_content = trimmed[skip_len..].trim();
        let parsed = parse_inline(title_content);
        let mut spans = Vec::with_capacity(parsed.spans.len());
        for mut span in parsed.spans {
            span.style = span.style.fg(color).add_modifier(Modifier::BOLD);
            spans.push(span);
        }
        Some(Line::from(spans))
    } else {
        None
    }
}
