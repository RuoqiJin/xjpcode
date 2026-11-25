use ratatui::layout::Rect;

/// 计算输入框上方悬浮弹出菜单/选择器的几何区域 (唯一出处，M10)。
pub fn compute_popup_area(input_area: Rect, item_count: usize) -> Rect {
    let popup_height = (item_count as u16 + 2).min(8);
    let popup_y = input_area.y.saturating_sub(popup_height);
    Rect {
        x: input_area.x + 1,
        y: popup_y,
        width: input_area.width.saturating_sub(2),
        height: popup_height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_popup_area() {
        let input = Rect {
            x: 0,
            y: 20,
            width: 80,
            height: 3,
        };
        let area = compute_popup_area(input, 3);
        assert_eq!(area.height, 5);
        assert_eq!(area.y, 15);
        assert_eq!(area.width, 78);
    }
}
