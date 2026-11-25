use crate::tui::render::theme::Theme;
use crate::ui::{App, BlockKind};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Text;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::blocks;

/// 内容区 Borders::ALL 占用的行数（上 1 + 下 1）；events/engine.rs 计算可视高度时引用。
pub const BORDER_ROWS: u16 = 2;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) -> u16 {
    app.content_area = Some(area);

    let mut content_lines = Vec::new();
    let mut current_y = area.y + 1;
    let inner_width = area.width.saturating_sub(2) as usize;

    for block in app.blocks.iter_mut() {
        let block_y_start = current_y;
        let toggle_icon = if block.collapsed { "▸ " } else { "▾ " };
        let lines_start = content_lines.len();

        match block.kind {
            BlockKind::UserPrompt => blocks::user::draw(block, &mut content_lines),
            BlockKind::ToolCall => blocks::tool_call::draw(block, &mut content_lines),
            BlockKind::AssistantAnswer => blocks::assistant::draw(block, &mut content_lines),
            BlockKind::SystemNotice => blocks::system_notice::draw(block, &mut content_lines),
            BlockKind::Divider => {
                blocks::divider::draw(block.round.unwrap_or(1), inner_width, &mut content_lines);
            }
        }
        let _ = toggle_icon;

        super::spacing::append_block_spacing(&block.kind, &mut content_lines);

        let mut visual_rows_added: u16 = 0;
        for line in &content_lines[lines_start..] {
            let rows = crate::tui::render::selection::wrapper::compute_visual_line_count(
                line,
                inner_width,
            ) as u16;
            visual_rows_added += rows;
        }

        let block_y_end = block_y_start + visual_rows_added.saturating_sub(1);
        current_y = block_y_start + visual_rows_added;
        block.render_y_range = Some((block_y_start, block_y_end));
    }

    let visual_lines =
        crate::tui::render::selection::wrapper::wrap_lines(&content_lines, inner_width);

    if app.selection.anchor.is_some() {
        app.rendered_text_lines = visual_lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
    }

    let mut render_lines = visual_lines;

    if let Some(range) = app.selection.get_normalized_range() {
        let mut highlighted = Vec::with_capacity(render_lines.len());
        let mut line_y = area.y + 1;
        for line in render_lines {
            let render_y = line_y.saturating_sub(app.scroll.offset);
            let highlighted_line = crate::tui::render::selection::highlight::highlight_line(
                &line, render_y, area.x, range,
            );
            highlighted.push(highlighted_line);
            line_y += 1;
        }
        render_lines = highlighted;
    }

    let total_visual_lines = current_y.saturating_sub(area.y + 1);

    let widget = Paragraph::new(Text::from(render_lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(Theme::BORDER_TYPE)
                .border_style(Style::default().fg(Theme::COLOR_BORDER)),
        )
        .scroll((app.scroll.offset, 0));

    f.render_widget(widget, area);

    total_visual_lines
}
