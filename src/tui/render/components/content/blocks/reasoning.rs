use super::collapsible;
use crate::format::titles::REASONING_TITLE;
use crate::tui::render::markdown::parse_markdown;
use crate::tui::render::theme::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

pub use crate::format::reasoning::format_reasoning_content;

/// 构造 [思考] 标题行（唯一的标题行生成逻辑）。
pub fn title_line(collapsed: bool) -> Line<'static> {
    let icon = if collapsed { "▸ " } else { "▾ " };
    Line::from(vec![
        Span::styled(icon, Style::default().fg(Theme::COLOR_TEXT_DIM)),
        Span::styled(
            REASONING_TITLE,
            Style::default()
                .fg(Theme::COLOR_TEXT_DIM)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

pub fn draw_collapsed_title(content_lines: &mut Vec<Line<'static>>) {
    content_lines.push(title_line(true));
}

/// 展开态完整思考小节：标题 ▾ [思考] + 断句 + markdown + 调暗 + 引用条。
pub fn draw_section(reasoning_text: &str, content_lines: &mut Vec<Line<'static>>) {
    content_lines.push(title_line(false));
    let formatted = format_reasoning_content(reasoning_text);
    let markdown_lines = dim_reasoning(parse_markdown(&formatted));
    content_lines.extend(collapsible::prefix_quote_bar(markdown_lines));
}

/// 思考过程正文整体调暗+斜体，保留 markdown 的加粗/代码等样式。
pub fn dim_reasoning(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| {
            Line::from(
                line.spans
                    .into_iter()
                    .map(|span| {
                        Span::styled(
                            span.content,
                            span.style
                                .fg(Theme::COLOR_TEXT_DIM)
                                .add_modifier(Modifier::ITALIC),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}
