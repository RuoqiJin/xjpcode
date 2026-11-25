use crate::tui::render::theme::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

pub fn parse_inline(line: &str) -> Line<'static> {
    let mut spans = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();
    let mut i = 0;

    let base_style = Style::default().fg(Theme::COLOR_TEXT_MAIN);

    // 行内代码与路径高亮：纯无底色高亮（琥珀金加粗）
    let code_style = Style::default()
        .fg(Theme::COLOR_HELP_KEY)
        .add_modifier(Modifier::BOLD);

    let bold_style = Style::default()
        .fg(Theme::COLOR_TEXT_MAIN)
        .add_modifier(Modifier::BOLD);

    let bold_code_style = Style::default()
        .fg(Theme::COLOR_HELP_KEY)
        .add_modifier(Modifier::BOLD);

    let italic_style = Style::default()
        .fg(Theme::COLOR_TEXT_MAIN)
        .add_modifier(Modifier::ITALIC);

    while i < len {
        // 匹配 **`code`** 或者 `**code**`
        if i + 3 < len && chars[i] == '*' && chars[i + 1] == '*' && chars[i + 2] == '`' {
            if !current.is_empty() {
                spans.push(Span::styled(current.clone(), base_style));
                current.clear();
            }
            i += 3;
            let start = i;
            while i + 2 < len && !(chars[i] == '`' && chars[i + 1] == '*' && chars[i + 2] == '*') {
                i += 1;
            }
            let code_content: String = chars[start..i].iter().collect();
            spans.push(Span::styled(code_content, bold_code_style));
            if i + 2 < len {
                i += 3;
            }
            continue;
        }

        // 匹配 **bold**
        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            if !current.is_empty() {
                spans.push(Span::styled(current.clone(), base_style));
                current.clear();
            }
            i += 2;
            let start = i;
            while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '*') {
                i += 1;
            }
            let bold_content: String = chars[start..i].iter().collect();
            spans.push(Span::styled(bold_content, bold_style));
            if i + 1 < len {
                i += 2;
            }
            continue;
        }

        // 匹配 *italic*（单个星号；`**bold**` 已在上面走掉）
        if chars[i] == '*' {
            if let Some(end) = (i + 1..len).find(|&j| chars[j] == '*') {
                if end > i + 1 {
                    if !current.is_empty() {
                        spans.push(Span::styled(current.clone(), base_style));
                        current.clear();
                    }
                    let italic: String = chars[i + 1..end].iter().collect();
                    spans.push(Span::styled(italic, italic_style));
                    i = end + 1;
                    continue;
                }
            }
        }

        // 匹配 `code`
        if chars[i] == '`' {
            if !current.is_empty() {
                spans.push(Span::styled(current.clone(), base_style));
                current.clear();
            }
            i += 1;
            let start = i;
            while i < len && chars[i] != '`' {
                i += 1;
            }
            let code_content: String = chars[start..i].iter().collect();
            spans.push(Span::styled(code_content, code_style));
            if i < len {
                i += 1;
            }
            continue;
        }

        current.push(chars[i]);
        i += 1;
    }

    if !current.is_empty() {
        spans.push(Span::styled(current, base_style));
    }

    Line::from(spans)
}
