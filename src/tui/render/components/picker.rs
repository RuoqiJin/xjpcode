use crate::tui::render::theme::Theme;
use crate::ui::App;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

/// 模态选择菜单的统一绘制（模型/思考档位等共用；标题与列表由 PickerState 携带）
pub fn draw(f: &mut Frame, app: &App, input_area: Rect) {
    let Some(picker) = app.picker.as_ref() else {
        return;
    };
    if picker.items.is_empty() {
        return;
    }

    let popup_area = super::popup_geometry::compute_popup_area(input_area, picker.items.len());

    // 遮挡背景
    f.render_widget(Clear, popup_area);

    let selected = picker.index.min(picker.items.len() - 1);
    // 列表比框高时跟着选中项滚动，别让选中项落在框外。
    let visible_rows = popup_area.height.saturating_sub(2) as usize;
    let start = if visible_rows == 0 || selected < visible_rows {
        0
    } else {
        selected + 1 - visible_rows
    };
    let mut lines = Vec::new();
    for (idx, item) in picker
        .items
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

        let mut spans = vec![
            Span::styled(prefix, style),
            Span::styled(format!("{:<12} ", item.label), style),
            Span::styled(item.detail.clone(), desc_style),
        ];
        if item.active {
            spans.push(Span::styled(
                "  ✓ 当前",
                Style::default().fg(Theme::COLOR_AI),
            ));
        }
        lines.push(Line::from(spans));
    }

    let title_span = Span::styled(
        format!(" {} ", picker.title),
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
