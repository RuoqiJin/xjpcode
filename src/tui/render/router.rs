use crate::tui::render::spinner::SPINNER_FRAMES;
use crate::tui::render::theme::Theme;
use crate::ui::App;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::components;

/// 布局中固定占用的行数：Header 3 + 底部状态栏 1 + 动态排队行 + 动态 Input 框 3~8 行。
pub fn chrome_height(app: &App) -> u16 {
    let line_count = app.input.split('\n').count() as u16;
    let input_h = (line_count + 2).clamp(3, 8);
    3 + 1 + app.queue_rows() + input_h
}

pub fn render_ui(f: &mut Frame, app: &mut App) -> u16 {
    let status_height = app.spinner.status_bar_height();
    let line_count = app.input.split('\n').count() as u16;
    let input_height = (line_count + 2).clamp(3, 8);
    let queue_height = app.queue_rows();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),             // 0: 顶部 Header 栏
            Constraint::Min(5),                // 1: 主对话内容区
            Constraint::Length(status_height), // 2: Spinner 状态栏（位于对话与输入之间）
            Constraint::Length(queue_height),  // 3: 排队中的话
            Constraint::Length(input_height),  // 4: 底部 Input 框
            Constraint::Length(1),             // 5: 底部状态栏
        ])
        .split(f.area());

    components::header::draw(f, app, chunks[0]);
    let total_lines = components::content::router::draw(f, app, chunks[1]);

    if app.spinner.is_processing {
        let frame_char = SPINNER_FRAMES[app.spinner.frame % SPINNER_FRAMES.len()];
        let duration = app.spinner.format_elapsed();
        let tokens = app.spinner.format_tokens();
        let spinner_line = Line::from(vec![
            Span::styled(
                format!("  {frame_char} "),
                Style::default()
                    .fg(Theme::COLOR_AI)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} ", app.spinner.verb),
                Style::default()
                    .fg(Theme::COLOR_HELP_KEY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("({duration} · {tokens})"),
                Style::default().fg(Theme::COLOR_TEXT_DIM),
            ),
        ]);
        f.render_widget(Paragraph::new(spinner_line), chunks[2]);
    }

    components::queue::draw(f, app, chunks[3]);
    components::input::draw(f, app, chunks[4]);
    components::popup::draw(f, app, chunks[4]);
    components::picker::draw(f, app, chunks[4]);
    components::login::draw(f, app, chunks[1]);
    components::toast::draw(f, app, chunks[1]);
    components::status_line::draw(f, app, chunks[5]);
    app.status_bar_area = Some(chunks[5]);

    total_lines
}
