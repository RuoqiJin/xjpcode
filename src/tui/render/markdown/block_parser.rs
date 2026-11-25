use super::blocks::{code, header, list};
use super::inline::parse_inline;
use super::table::render_table_block;
use ratatui::text::Line;

pub fn parse_markdown(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code_block = false;
    let raw_lines: Vec<&str> = text.lines().collect();
    let mut i = 0;

    while i < raw_lines.len() {
        let raw_line = raw_lines[i];
        let trimmed = raw_line.trim();

        // 0. 水平分割线
        if let Some(line) = header::try_render_hr(trimmed, in_code_block) {
            lines.push(line);
            i += 1;
            continue;
        }

        // 1. 代码块开关
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            if let Some(line) = code::render_code_toggle(trimmed, in_code_block) {
                lines.push(line);
            }
            i += 1;
            continue;
        }

        // 2. 代码块内容行
        if in_code_block {
            lines.push(code::render_code_line(raw_line));
            i += 1;
            continue;
        }

        // 3. 引用块
        if let Some(line) = list::try_render_quote(trimmed) {
            lines.push(line);
            i += 1;
            continue;
        }

        // 4. 标题级数
        if let Some(line) = header::try_render_header(trimmed) {
            lines.push(line);
            i += 1;
            continue;
        }

        // 5. 无序列表
        if let Some(line) = list::try_render_unordered_list(trimmed) {
            lines.push(line);
            i += 1;
            continue;
        }

        // 6. 有序列表
        if let Some(line) = list::try_render_ordered_list(trimmed) {
            lines.push(line);
            i += 1;
            continue;
        }

        // 7. 表格块检测与自适应渲染 (| col1 | col2 |)
        if trimmed.starts_with('|') && trimmed.ends_with('|') {
            let mut table_block = Vec::new();
            while i < raw_lines.len() {
                let cur = raw_lines[i].trim();
                if cur.starts_with('|') && cur.ends_with('|') {
                    table_block.push(cur);
                    i += 1;
                } else {
                    break;
                }
            }

            lines.extend(render_table_block(&table_block));
            continue;
        }

        // 8. 普通段落文本（解析行内 Markdown）
        lines.push(parse_inline(raw_line));
        i += 1;
    }

    lines
}
