use crate::tui::render::theme::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

pub fn render_code_toggle(trimmed: &str, in_code_block: bool) -> Option<Line<'static>> {
    if in_code_block {
        let lang = trimmed.trim_start_matches('`').trim();
        let label = if lang.is_empty() {
            "code".to_string()
        } else {
            lang.to_lowercase()
        };

        let line_style = Style::default().fg(Theme::COLOR_BORDER);
        let label_style = Style::default()
            .fg(Theme::COLOR_AI)
            .add_modifier(Modifier::BOLD);

        Some(Line::from(vec![
            Span::styled(" │ ", line_style),
            Span::styled(label, label_style),
        ]))
    } else {
        None
    }
}

pub fn render_code_line(raw_line: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(" │ ", Style::default().fg(Theme::COLOR_BORDER)),
        Span::styled(
            raw_line.to_string(),
            Style::default().fg(Theme::COLOR_HELP_KEY),
        ),
    ])
}
