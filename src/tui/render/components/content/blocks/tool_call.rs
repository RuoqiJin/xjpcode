use crate::format::titles::{split_title_and_suffix, TOOL_BADGE};
use crate::tui::render::theme::Theme;
use crate::ui::ContentBlock;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::collapsible;

pub fn draw(block: &ContentBlock, content_lines: &mut Vec<Line<'static>>) {
    draw_tool_call_title_row(block, content_lines);

    if !block.collapsed {
        let (_, suffix) = split_title_and_suffix(&block.title);
        let mut body_lines = Vec::new();

        if !suffix.is_empty() {
            body_lines.push(Line::from(Span::styled(
                suffix.trim().to_string(),
                Style::default().fg(Theme::COLOR_TEXT_DIM),
            )));
        }

        for line in block.content.lines() {
            body_lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(Theme::COLOR_TEXT_DIM),
            )));
        }

        content_lines.extend(collapsible::prefix_quote_bar(body_lines));
    }
}

/// 工具调用标题行：圆点（失败变红）+ `[工具]` + 工具名 + (失败)。
pub fn draw_tool_call_title_row(block: &ContentBlock, content_lines: &mut Vec<Line<'static>>) {
    let bullet_style = if block.is_error {
        Style::default().fg(Theme::COLOR_ERROR)
    } else {
        Style::default().fg(Theme::COLOR_USER)
    };
    let label_style = Style::default().fg(Theme::COLOR_TEXT_MAIN);
    let blue_tool_style = Style::default()
        .fg(Theme::COLOR_AI)
        .add_modifier(Modifier::BOLD);

    let (prefix_and_name, _) = split_title_and_suffix(&block.title);
    let tool_name = block.tool_name.as_deref().unwrap_or(prefix_and_name);

    let mut spans = vec![
        Span::styled("• ", bullet_style),
        Span::styled(format!("[{TOOL_BADGE}] "), label_style),
        Span::styled(tool_name.to_string(), blue_tool_style),
    ];

    if block.is_error {
        spans.push(Span::styled(
            " (失败)".to_string(),
            Style::default()
                .fg(Theme::COLOR_ERROR)
                .add_modifier(Modifier::BOLD),
        ));
    }

    content_lines.push(Line::from(spans));
}
