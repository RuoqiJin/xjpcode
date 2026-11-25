use crate::tui::render::markdown::blocks::header::try_render_hr;

#[test]
fn hr_recognizes_all_three_markers() {
    for marker in ["---", "***", "___"] {
        assert!(
            try_render_hr(marker, false).is_some(),
            "{marker} 应识别为分割线"
        );
    }
}

/// 三个以上连续减号也是分割线
#[test]
fn hr_accepts_longer_dash_runs() {
    assert!(try_render_hr("----------", false).is_some());
}

/// 代码块内的 --- 是内容不是分割线
#[test]
fn hr_is_suppressed_inside_code_block() {
    assert!(try_render_hr("---", true).is_none());
}

/// 短于 3 个字符、或混入其它字符的都不是分割线
#[test]
fn hr_rejects_short_or_mixed_runs() {
    assert!(try_render_hr("--", false).is_none());
    assert!(try_render_hr("-", false).is_none());
    assert!(try_render_hr("", false).is_none());
}

/// 与无序列表项区分:"- item" 不是分割线
#[test]
fn hr_does_not_swallow_list_items() {
    assert!(try_render_hr("- item", false).is_none());
    assert!(try_render_hr("* item", false).is_none());
}
