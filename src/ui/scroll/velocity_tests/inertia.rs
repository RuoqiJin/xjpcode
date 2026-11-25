use super::fixtures::*;
use crate::ui::scroll::ScrollState;
use std::time::Instant;

/// 抬手速度估计:60Hz 快滑的 EMA 应逼近事件率上限(60 行/秒)
#[test]
fn fast_flick_velocity_approaches_event_rate() {
    let mut s = ScrollState::default();
    let base = Instant::now();
    burst(&mut s, base, 10, 1.0);
    assert!(s.velocity > 30.0, "快滑抬手速度应足够高: {}", s.velocity);
    assert!(s.velocity <= 60.0, "速度不得超过事件率上限: {}", s.velocity);
}

/// 慢速滚动(150ms 间隔 ≈ 6.7 行/秒)抬手速度必须低于惯性触发阈值 12,
/// 否则停下后画面会自己漂移(幽灵惯性回归)
#[test]
fn slow_scroll_stays_below_flick_threshold() {
    let mut s = ScrollState::default();
    let base = Instant::now();
    for i in 0..6 {
        s.track_velocity_at(1.0, t(base, i * 150));
    }
    assert!(
        s.velocity.abs() < 12.0,
        "慢速滚动不得触发惯性: {}",
        s.velocity
    );
}

/// 方向反转:EMA 快速跟随最新方向——上滑手势立即转下滑,速度在两三个事件内
/// 翻负并达到惯性触发量级,抬手后惯性朝最新方向漂移(不会带着旧方向残速)
#[test]
fn direction_reversal_follows_latest_direction() {
    let mut s = ScrollState::default();
    let base = Instant::now();
    burst(&mut s, base, 5, 1.0);
    assert!(s.velocity > 30.0);

    burst(&mut s, t(base, 5 * MS_16), 4, -1.0);
    assert!(
        s.velocity < -12.0,
        "反向后速度必须跟随最新方向并达到可触发惯性的量级: {}",
        s.velocity
    );
}
