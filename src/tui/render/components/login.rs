use crate::tui::render::theme::Theme;
use crate::ui::App;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use unicode_width::UnicodeWidthStr;

fn line_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|span| span.content.width()).sum()
}

fn wrapped_rows(line: &Line<'_>, inner_width: usize) -> u16 {
    if inner_width == 0 {
        return 1;
    }
    line_width(line).div_ceil(inner_width).max(1) as u16
}

/// 登录提示：xj 风格的边框弹出块，盖住内容区。
/// 输入行放在地址前面：地址很长时也先看得到自己贴的东西。
pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let Some(prompt) = &app.login_prompt else {
        return;
    };

    let shown = if prompt.secret {
        "•".repeat(prompt.input.chars().count())
    } else {
        prompt.input.clone()
    };

    let mut lines = vec![
        Line::from(Span::styled(
            prompt.message.clone(),
            Style::default()
                .fg(Theme::COLOR_TEXT_MAIN)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("> ", Style::default().fg(Theme::COLOR_TEXT_DIM)),
            Span::styled(shown.clone(), Style::default().fg(Theme::COLOR_TEXT_MAIN)),
        ]),
    ];
    if let Some(notice) = &prompt.notice {
        lines.push(Line::from(Span::styled(
            notice.clone(),
            Style::default().fg(Theme::COLOR_HELP_KEY),
        )));
    }
    if let Some(url) = &app.login_url {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "浏览器没打开的话，把下面这个地址打开（可以整段复制）：",
            Style::default().fg(Theme::COLOR_TEXT_DIM),
        )));
        lines.push(Line::from(Span::styled(
            url.clone(),
            Style::default().fg(Theme::COLOR_AI),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Enter 提交 · Esc 取消 · 可以直接粘贴",
        Style::default().fg(Theme::COLOR_TEXT_DIM),
    )));

    let width = area.width.saturating_sub(4).min(96).max(20);
    let inner_width = width.saturating_sub(2) as usize;
    let content_rows: u16 = lines
        .iter()
        .map(|line| wrapped_rows(line, inner_width))
        .sum();
    let height = (content_rows + 2).min(area.height.max(3));
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let input_row = wrapped_rows(&lines[0], inner_width) + 1;
    f.render_widget(Clear, popup);
    let widget = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(Theme::BORDER_TYPE)
            .title(Span::styled(
                " 登录 ",
                Style::default()
                    .fg(Theme::COLOR_USER)
                    .add_modifier(Modifier::BOLD),
            ))
            .border_style(Style::default().fg(Theme::COLOR_BORDER_ACTIVE)),
    );
    f.render_widget(widget, popup);

    // 光标放在输入行末尾（登录提示独占键盘）。
    let cursor_x = popup.x + 1 + 2 + shown.width() as u16;
    let cursor_y = popup.y + 1 + input_row;
    if cursor_y < popup.y + popup.height.saturating_sub(1) {
        f.set_cursor_position((
            cursor_x.min(popup.x + popup.width.saturating_sub(1)),
            cursor_y,
        ));
    }
}
