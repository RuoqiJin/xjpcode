use super::picker::{PickerItem, PickerKind, PickerState};

fn detail(level: &str) -> &'static str {
    match level {
        "off" => "关闭思考",
        "minimal" => "最少思考",
        "low" => "低",
        "medium" => "中（缺省）",
        "high" => "高",
        "xhigh" => "很高",
        "max" => "最高",
        _ => "",
    }
}

/// 构建思考档位选择菜单（/effort、/thinking 无参数时打开）。
pub fn build_effort_picker(levels: &[String], current: &str) -> PickerState {
    let items = levels
        .iter()
        .map(|level| PickerItem {
            id: level.clone(),
            label: level.clone(),
            detail: detail(level).to_string(),
            active: level == current,
        })
        .collect();
    let index = levels
        .iter()
        .position(|level| level == current)
        .unwrap_or(0);
    PickerState {
        kind: PickerKind::Effort,
        title: "选择思考档位 (↑/↓ 切换，Enter 确认)",
        items,
        index,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_picker_starts_on_the_current_level() {
        let levels = vec!["off".to_string(), "medium".to_string(), "high".to_string()];
        let picker = build_effort_picker(&levels, "high");
        assert_eq!(picker.index, 2);
        assert!(picker.items[2].active);
        assert_eq!(picker.items[1].detail, "中（缺省）");
    }
}
