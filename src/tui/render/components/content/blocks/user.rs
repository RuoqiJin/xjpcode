use crate::tui::render::theme::Theme;
use crate::ui::ContentBlock;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// 用户输入块: 前置 ` YOU ` 徽章，卡片式底框 + 白色粗体文本
pub fn draw(block: &ContentBlock, content_lines: &mut Vec<Line<'static>>) {
    const BADGE: &str = " YOU ";
    let card_style = Style::default()
        .bg(Theme::COLOR_SELECTION_BG)
        .fg(Theme::COLOR_TEXT_MAIN)
        .add_modifier(Modifier::BOLD);

    let you_badge = Span::styled(BADGE, Theme::style_user_badge());
    // 续行缩进 = 徽章宽度 + 徽后 1 空格，保证多行输入时正文列对齐
    let continuation_indent = " ".repeat(BADGE.len() + 1);

    for (i, line_text) in block.content.lines().enumerate() {
        if i == 0 {
            content_lines.push(Line::from(vec![
                you_badge.clone(),
                Span::raw(" "),
                Span::styled(format!(" {} ", line_text), card_style),
            ]));
        } else {
            content_lines.push(Line::from(vec![
                Span::raw(continuation_indent.clone()),
                Span::styled(format!(" {} ", line_text), card_style),
            ]));
        }
    }
}
