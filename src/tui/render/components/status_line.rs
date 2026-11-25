use crate::format::cwd;
use crate::tui::render::theme::Theme;
use crate::ui::App;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// 左下角状态栏：当前模型/思考档位 + 工作目录（唯一出处）。
fn truncate_to_width(text: &str, max: usize) -> String {
    let mut width = 0;
    for (index, character) in text.char_indices() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width + character_width > max {
            return text[..index].to_string();
        }
        width += character_width;
    }
    text.to_string()
}

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let model = if app.model_provider.is_empty() {
        app.model_id.clone()
    } else {
        format!("{}/{}", app.model_provider, app.model_id)
    };
    let left = format!(
        "{} (effort: {})  {}",
        model,
        app.thinking,
        cwd::display_cwd()
    );
    let right = super::context_meter::text(app);

    // 左右两段拼在一行里，先按剩余宽度截左段，避免两段互相盖字。
    let available = area.width as usize;
    let right_width = right.as_str().width();
    let left = if right.is_empty() {
        truncate_to_width(&left, available)
    } else if right_width + 2 >= available {
        String::new()
    } else {
        truncate_to_width(&left, available - right_width - 2)
    };
    let gap = available.saturating_sub(left.as_str().width() + right.as_str().width());
    let line = Line::from(vec![
        Span::styled(
            left,
            Style::default()
                .fg(Theme::COLOR_AI)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(gap)),
        Span::styled(right, Style::default().fg(Theme::COLOR_TEXT_DIM)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}
