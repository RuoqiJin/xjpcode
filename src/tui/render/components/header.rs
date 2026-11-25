use crate::build_info;
use crate::tui::render::theme::Theme;
use crate::ui::App;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

fn short_sha() -> &'static str {
    &build_info::GIT_SHA[..build_info::GIT_SHA.len().min(7)]
}

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let version = build_info::VERSION;
    let short = short_sha();
    let header_text = vec![Line::from(vec![Span::styled(
        format!(
            " {} CLI {} ({}) ",
            if app.model_name.is_empty() {
                "xjpcode".to_string()
            } else {
                app.model_name.clone()
            },
            version,
            short
        ),
        Theme::style_header_title(),
    )])];
    let header_widget = Paragraph::new(header_text).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(Theme::BORDER_TYPE)
            .title(Span::styled(
                format!(" 控制台 {version} · {short} "),
                Style::default()
                    .fg(Theme::COLOR_AI)
                    .add_modifier(Modifier::BOLD),
            ))
            .border_style(Style::default().fg(Theme::COLOR_BORDER)),
    );
    f.render_widget(header_widget, area);
}
