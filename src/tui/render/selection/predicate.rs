/// 选区位置判定谓词（Single Source of Truth）：
/// 判定在坐标 range ((x1, y1), (x2, y2)) 下，指定的 (line_y, current_col) 是否在选区范围内。
/// highlight.rs (渲染高亮) 与 extractor.rs (提取选中文本) 100% 共享此判定公式。
pub fn is_point_in_selection(line_y: u16, col: u16, range: ((u16, u16), (u16, u16))) -> bool {
    let ((x1, y1), (x2, y2)) = range;
    if y1 == y2 {
        line_y == y1 && col >= x1 && col <= x2
    } else if line_y == y1 {
        col >= x1
    } else if line_y == y2 {
        col <= x2
    } else {
        true
    }
}
