use super::ScrollState;
use std::time::{Duration, Instant};

fn lifted_ago(ms: u64) -> Instant {
    Instant::now() - Duration::from_millis(ms)
}

/// 抬手速度快(30 行/秒)且手势已结束:惯性点火一次,残余速度被消费,
/// 首帧位移按 dt 积分(30 × 0.016),不是旧版的每帧固定值
#[test]
fn fast_liftoff_fires_momentum_once_with_dt_integration() {
    let mut s = ScrollState {
        velocity: 30.0,
        last_scroll_time: Some(lifted_ago(200)),
        ..ScrollState::default()
    };

    assert!(s.update_momentum(0.016));
    assert_eq!(s.velocity, 0.0, "初速度必须被消费,防重复点火");
    assert!(s.momentum_velocity > 0.0);
    let moved = s.target;
    assert!(
        (moved - 30.0 * 0.016).abs() < 0.05,
        "首帧位移必须按 dt 积分: {moved}"
    );

    // 第二帧:速度已衰减,不再重新点火(velocity 仍为 0)
    let v_before = s.momentum_velocity;
    assert!(s.update_momentum(0.016));
    assert!(s.momentum_velocity < v_before, "惯性必须随时间衰减");
}

/// 慢速停止(6 行/秒):不点火、不漂移——幽灵惯性修复的回归守卫
#[test]
fn slow_stop_does_not_drift() {
    let mut s = ScrollState {
        velocity: 6.0,
        last_scroll_time: Some(lifted_ago(200)),
        ..ScrollState::default()
    };

    assert!(!s.update_momentum(0.016));
    assert_eq!(s.momentum_velocity, 0.0);
    assert_eq!(s.target, 0.0, "慢速停止后画面不得自己移动");
}

/// 手势进行中(事件未超抬手窗口)即使速度很高也不点火
#[test]
fn momentum_waits_for_liftoff() {
    let mut s = ScrollState {
        velocity: 40.0,
        last_scroll_time: Some(lifted_ago(50)), // 50ms 前还有事件,手势未结束
        ..ScrollState::default()
    };

    assert!(!s.update_momentum(0.016));
    assert_eq!(s.momentum_velocity, 0.0);
}

/// 惯性衰减到地板速度以下自动熄火归零
#[test]
fn momentum_dies_below_floor_speed() {
    let mut s = ScrollState {
        momentum_velocity: 0.5, // 低于 MOMENTUM_FLOOR(1.0)
        last_scroll_time: None,
        ..ScrollState::default()
    };

    assert!(!s.update_momentum(0.016));
    assert_eq!(s.momentum_velocity, 0.0);
}
