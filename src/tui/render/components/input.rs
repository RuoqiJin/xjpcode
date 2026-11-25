use crate::tui::render::theme::Theme;
use crate::ui::App;
use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use unicode_width::UnicodeWidthStr;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let border_color = if app.spinner.is_processing {
        Theme::COLOR_BORDER
    } else {
        Theme::COLOR_BORDER_ACTIVE
    };

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(Theme::BORDER_TYPE)
        .border_style(Style::default().fg(border_color));

    if let Some(index) = app.history_idx {
        let title = format!(" History {}/{} ", index + 1, app.history.len());
        block = block.title(Span::styled(
            title,
            Style::default().fg(Theme::COLOR_TEXT_DIM),
        ));
    }

    let lines: Vec<&str> = app.input.split('\n').collect();
    let total_lines = lines.len();
    let inner_height = area.height.saturating_sub(2) as usize;
    let cursor_row = app.input[..app.cursor_pos].matches('\n').count();
    let scroll = if inner_height == 0 {
        0
    } else if cursor_row >= inner_height {
        cursor_row + 1 - inner_height
    } else if total_lines > inner_height {
        total_lines - inner_height
    } else {
        0
    };

    let mut formatted_lines: Vec<Line<'static>> = Vec::new();
    for (index, line) in lines
        .iter()
        .enumerate()
        .skip(scroll)
        .take(inner_height.max(1))
    {
        let prefix = if index == 0 {
            Span::styled("> ", Style::default().fg(Theme::COLOR_TEXT_DIM))
        } else {
            Span::raw("  ")
        };
        formatted_lines.push(Line::from(vec![
            prefix,
            Span::styled(
                (*line).to_string(),
                Style::default().fg(Theme::COLOR_TEXT_MAIN),
            ),
        ]));
    }

    let widget = Paragraph::new(formatted_lines).scroll((0, 0)).block(block);
    f.render_widget(widget, area);

    if app.login_prompt.is_some() {
        return;
    }
    let line_start = app.input[..app.cursor_pos]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let col_text = &app.input[line_start..app.cursor_pos];
    let cursor_x = area.x + 1 + 2 + col_text.width() as u16;
    let cursor_y = area.y + 1 + (cursor_row.saturating_sub(scroll)) as u16;
    let max_x = area.x + area.width.saturating_sub(1);
    f.set_cursor_position((cursor_x.min(max_x), cursor_y));
}
