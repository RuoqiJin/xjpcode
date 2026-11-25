use crate::tui::render::theme::Theme;
use crate::ui::App;
use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// 输入框上方：跑着时排队的话（截到最近几行）。
pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 {
        return;
    }
    let mut lines: Vec<Line<'static>> = Vec::new();
    for text in &app.pending_steering {
        lines.push(Line::from(vec![
            Span::styled(" ↳ 插话: ", Style::default().fg(Theme::COLOR_HELP_KEY)),
            Span::styled(
                text.replace('\n', " "),
                Style::default().fg(Theme::COLOR_TEXT_DIM),
            ),
        ]));
    }
    for text in &app.pending_follow_up {
        lines.push(Line::from(vec![
            Span::styled(" ↳ 排队: ", Style::default().fg(Theme::COLOR_AI)),
            Span::styled(
                text.replace('\n', " "),
                Style::default().fg(Theme::COLOR_TEXT_DIM),
            ),
        ]));
    }
    let omitted = lines.len().saturating_sub(area.height as usize);
    if omitted > 0 {
        lines = lines[omitted..].to_vec();
    }
    f.render_widget(Paragraph::new(lines), area);
}
