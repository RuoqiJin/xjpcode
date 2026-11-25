use crate::tui::render::theme::Theme;
use crate::ui::ContentBlock;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

pub fn draw(block: &ContentBlock, content_lines: &mut Vec<Line<'static>>) {
    content_lines.push(Line::from(vec![Span::styled(
        format!(" {} ", block.title),
        Style::default()
            .bg(Theme::COLOR_PAYLOAD)
            .fg(Theme::COLOR_BADGE_TEXT)
            .add_modifier(Modifier::BOLD),
    )]));

    for line in block.content.lines() {
        content_lines.push(Line::from(Span::styled(
            line.to_string(),
            Style::default().fg(Theme::COLOR_TEXT_DIM),
        )));
    }
}
