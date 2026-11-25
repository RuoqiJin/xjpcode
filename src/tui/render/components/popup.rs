use crate::tui::render::theme::Theme;
use crate::ui::App;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

pub fn draw(f: &mut Frame, app: &App, input_area: Rect) {
    let matching_cmds = app.matching_commands();
    if matching_cmds.is_empty() {
        return;
    }

    let popup_area = super::popup_geometry::compute_popup_area(input_area, matching_cmds.len());

    // 遮挡背景
    f.render_widget(Clear, popup_area);

    let selected = app.popup_index.min(matching_cmds.len() - 1);
    let visible_rows = popup_area.height.saturating_sub(2) as usize;
    let start = if visible_rows == 0 || selected < visible_rows {
        0
    } else {
        selected + 1 - visible_rows
    };
    let mut lines = Vec::new();
    for (idx, cmd) in matching_cmds
        .iter()
        .enumerate()
        .skip(start)
        .take(visible_rows.max(1))
    {
        let is_selected = idx == selected;
        let prefix = if is_selected { "▸ " } else { "  " };

        let style = if is_selected {
            Style::default()
                .fg(Theme::COLOR_HELP_KEY)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Theme::COLOR_TEXT_MAIN)
        };

        let desc_style = Style::default().fg(Theme::COLOR_TEXT_DIM);

        lines.push(Line::from(vec![
            Span::styled(prefix, style),
            Span::styled(format!("{:<10} ", cmd.name), style),
            Span::styled(format!("- {}", cmd.description), desc_style),
        ]));
    }

    let title_span = Span::styled(
        " 命令补全 ",
        Style::default()
            .fg(Theme::COLOR_USER)
            .add_modifier(Modifier::BOLD),
    );

    let popup_widget = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(Theme::BORDER_TYPE)
            .title(title_span)
            .border_style(Style::default().fg(Theme::COLOR_BORDER_ACTIVE)),
    );

    f.render_widget(popup_widget, popup_area);
}
