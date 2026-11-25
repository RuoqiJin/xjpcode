use super::fixtures::*;
use crate::ui::scroll::ScrollState;
use std::time::Instant;

/// 不顿挫核心不变量:静置再久,手势首事件也沿用上次手势的设备模式,
/// 绝不先按滚轮步长窜一下再降级
#[test]
fn gesture_start_after_long_idle_keeps_trackpad_mode() {
    let mut s = ScrollState::default();
    let base = Instant::now();
    burst(&mut s, base, 5, 1.0);
    assert!(s.is_trackpad_mode);

    // 静置 10 秒后的手势首事件:不得改判滚轮
    s.track_velocity_at(1.0, t(base, 10_000));
    assert!(
        s.is_trackpad_mode,
        "静置后首事件必须沿用触控板模式,否则起步窜 3 行"
    );
    assert_eq!(s.adaptive_step(), 1.0, "触控板步长必须 1 事件 = 1 行");
}

/// 滚轮用户的出路:连续两次孤立单击(间隔 > 250ms)才从触控板改判滚轮,
/// 单次单击不误判(慢速触控板滚动也是孤立事件)
#[test]
fn two_consecutive_isolated_clicks_downgrade_to_wheel() {
    let mut s = ScrollState::default();
    let base = Instant::now();
    burst(&mut s, base, 5, 1.0);

    s.track_velocity_at(1.0, t(base, 10_000)); // 第一次孤立单击
    assert!(s.is_trackpad_mode, "单次孤立单击不得改判");

    s.track_velocity_at(1.0, t(base, 10_400)); // 第二次孤立单击(间隔 400ms)
    assert!(!s.is_trackpad_mode, "连续两次孤立单击应改判滚轮");
    assert_eq!(s.adaptive_step(), 3.0);
}

/// 滚轮高频证据:快速拨轮产生的高频事件流同样会被识别为连续流
/// (设备不可知,只看事件间隔),随后两次孤立单击又回到滚轮
#[test]
fn high_frequency_stream_marks_trackpad_device() {
    let mut s = ScrollState::default();
    let base = Instant::now();
    assert!(!s.is_trackpad_mode);
    burst(&mut s, base, 3, 1.0);
    assert!(s.is_trackpad_mode);
}
