use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

/// 从「最后一帧渲染快照」中提取当前划选选区的纯文本。
///
/// `lines` 是 content/router.rs 每帧缓存的纯文本行(每行 spans 拼接);`area` 是内容区 Rect;
/// `range` 是屏幕坐标下归一化后的选区 ((x1,y1),(x2,y2))。
///
/// 关键:列的判定按**显示列宽**(unicode-width,CJK=2)逐 char 累计,与 selection/highlight.rs
/// 完全一致——保证「看到高亮什么就复制什么」,不会在 CJK 文本上因 char 索引 vs 列宽错位。
///
/// 快照与实际渲染帧之间可能相差一帧(见 content/router.rs 的刷新守卫):拖拽期间每帧都会刷新
/// 快照,这点固有偏差对复制无实质影响,此处不做补偿。
pub fn extract_selected_text(
    lines: &[String],
    area: Rect,
    scroll_offset: u16,
    range: ((u16, u16), (u16, u16)),
) -> String {
    let ((_, y1), (_, y2)) = range;
    let mut out: Vec<String> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        // 快照第 i 行的屏幕行号:与 content/router.rs 应用高亮时的 render_y 同一算法
        // ((area.y + 1) + i - scroll_offset),确保选中判定与高亮所见一致。
        let line_y = (area.y + 1)
            .saturating_add(i as u16)
            .saturating_sub(scroll_offset);
        if line_y < y1 || line_y > y2 {
            continue;
        }

        // 逐 char 累计显示列宽判定落选,列基准与高亮同为 area.x + 1;判定谓词与 highlight_line 对齐。
        let mut selected = String::new();
        let mut current_col = area.x + 1;
        for ch in line.chars() {
            let in_selection = crate::tui::render::selection::predicate::is_point_in_selection(
                line_y,
                current_col,
                range,
            );
            if in_selection {
                selected.push(ch);
            }
            current_col += ch.to_string().as_str().width() as u16;
        }
        out.push(selected.trim_end().to_string());
    }

    out.join("\n")
}
