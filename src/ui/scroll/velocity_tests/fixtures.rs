use crate::ui::scroll::ScrollState;
use std::time::{Duration, Instant};

pub const MS_16: u64 = 16;

pub fn t(base: Instant, ms: u64) -> Instant {
    base + Duration::from_millis(ms)
}

/// 连发一组 16ms 间隔的同向滚动事件(模拟触控板手势流)
pub fn burst(state: &mut ScrollState, start: Instant, count: u64, direction: f32) {
    for i in 0..count {
        state.track_velocity_at(direction, t(start, i * MS_16));
    }
}
