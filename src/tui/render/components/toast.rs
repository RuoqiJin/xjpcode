use crate::tui::render::theme::Theme;
use crate::ui::App;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use unicode_width::UnicodeWidthStr;

pub fn draw(f: &mut Frame, app: &mut App, container_area: Rect) {
    if let Some(toast) = &app.toast {
        if toast.is_expired() {
            app.toast = None;
            return;
        }

        let msg = format!(" {} ", toast.message);
        let msg_width = msg.as_str().width() as u16 + 2; // Include border width
        let toast_height = 3;

        // Position in bottom-right corner of container_area
        let toast_x = container_area
            .x
            .saturating_add(container_area.width.saturating_sub(msg_width + 1));
        let toast_y = container_area
            .y
            .saturating_add(container_area.height.saturating_sub(toast_height + 1));

        let toast_area = Rect {
            x: toast_x,
            y: toast_y,
            width: msg_width,
            height: toast_height,
        };

        // Render Clear widget first so toast floats above content without text bleed-through
        f.render_widget(Clear, toast_area);

        let widget = Paragraph::new(msg).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(Theme::BORDER_TYPE)
                .title(Span::styled(
                    " 提示 ",
                    Style::default()
                        .fg(Theme::COLOR_AI)
                        .add_modifier(Modifier::BOLD),
                ))
                .border_style(Style::default().fg(Theme::COLOR_BORDER_ACTIVE)),
        );

        f.render_widget(widget, toast_area);
    }
}
