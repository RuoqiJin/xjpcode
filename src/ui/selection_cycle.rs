/// 列表环形导航通用索引计算（唯一出处，用于 Picker 与 Popup 列表的 ↑/↓ 移动）
pub fn cycle_prev(current: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else if current == 0 {
        len - 1
    } else {
        current - 1
    }
}

pub fn cycle_next(current: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        (current + 1) % len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cycle_prev_and_next() {
        assert_eq!(cycle_prev(0, 3), 2);
        assert_eq!(cycle_prev(1, 3), 0);
        assert_eq!(cycle_next(2, 3), 0);
        assert_eq!(cycle_next(0, 3), 1);
        assert_eq!(cycle_next(0, 0), 0);
        assert_eq!(cycle_prev(0, 0), 0);
    }
}
