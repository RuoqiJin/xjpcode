pub const PREVIEW_MAX_LINES: usize = 25;

/// 长文本折叠预览：超过 PREVIEW_MAX_LINES 行时截断并标注，全项目唯一出处
pub fn collapse_lines(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > PREVIEW_MAX_LINES {
        format!(
            "{}\n... [余下 {} 行数据已折叠，共 {} 字符]",
            lines[..PREVIEW_MAX_LINES].join("\n"),
            lines.len() - PREVIEW_MAX_LINES,
            text.len()
        )
    } else {
        text.to_string()
    }
}
