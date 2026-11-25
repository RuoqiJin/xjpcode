use ratatui::style::Modifier;
use ratatui::text::Line;

/// 渲染后的可见文本(标记符号已被吃掉)
pub fn text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

pub fn has(line: &Line<'_>, modifier: Modifier) -> bool {
    line.spans
        .iter()
        .any(|s| s.style.add_modifier.contains(modifier))
}
