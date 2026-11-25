use crate::tui::render::theme::Theme;
use crate::ui::ContentBlock;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// 可折叠块（标题行 + 可选内容行 + 空行）的统一绘制模板，payload 专用
pub fn draw(
    block: &ContentBlock,
    toggle_icon: &str,
    color: Color,
    content_lines: &mut Vec<Line<'static>>,
) {
    draw_title_row(block, toggle_icon, color, content_lines);

    if !block.collapsed {
        for line in block.content.lines() {
            content_lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(color),
            )));
        }
    }
}

/// 给每一行前面加统一的引用条 " │ "(唯一出处)，reasoning 与工具调用展开内容共用，
/// 借此在视觉上表达"这是块内的引述内容"而不必再画 ASCII 方框。
pub fn prefix_quote_bar(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| {
            let mut spans = vec![Span::styled(
                " │ ",
                Style::default().fg(Theme::COLOR_BORDER),
            )];
            spans.extend(line.spans);
            Line::from(spans)
        })
        .collect()
}

/// 折叠块标题行（toggle 图标 + 主标题 + 降级为暗灰色的统计后缀）的唯一画法，
/// 将 "(15字符 · 350μs)"、"(包含2条对话上下文)" 等括号统计后缀置为 COLOR_TEXT_DIM，突出主体。
pub fn draw_title_row(
    block: &ContentBlock,
    toggle_icon: &str,
    color: Color,
    content_lines: &mut Vec<Line<'static>>,
) {
    let main_style = Style::default().fg(color);
    let dim_style = Style::default().fg(Theme::COLOR_TEXT_DIM);

    let mut spans = vec![Span::styled(toggle_icon.to_string(), main_style)];

    let title = &block.title;
    let (primary, suffix) = crate::format::titles::split_title_and_suffix(title);
    if !suffix.is_empty() {
        spans.push(Span::styled(primary.to_string(), main_style));
        spans.push(Span::styled(suffix.to_string(), dim_style));
    } else {
        spans.push(Span::styled(title.clone(), main_style));
    }

    content_lines.push(Line::from(spans));
}
