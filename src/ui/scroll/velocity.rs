use super::state::ScrollState;
use std::time::Instant;

/// 高频事件间隔上限(秒):低于此判定为触控板连续手势流的确凿证据
const TRACKPAD_STREAM_GAP: f32 = 0.08;
/// 孤立单击间隔下限(秒):高于此累计两次才改判滚轮(防慢速触控板滚动被误降级)
const WHEEL_CLICK_GAP: f32 = 0.25;
/// 改判滚轮所需的连续孤立单击次数
const WHEEL_CLICK_STREAK: u8 = 2;
/// 抬手速度 EMA 的新样本权重:偏重手势末尾,又不被单个间隔毛刺带飞
const SPEED_EMA_NEW: f32 = 0.6;
/// 事件率上限(次/秒):同毫秒连发的事件批不把速度估计顶穿
const MAX_EVENT_RATE: f32 = 60.0;

impl ScrollState {
    pub fn track_velocity(&mut self, direction: f32) {
        self.track_velocity_at(direction, Instant::now());
    }

    /// 设备识别 + 抬手速度估计。拆开 now 参数是为了测试可注入时间。
    pub(crate) fn track_velocity_at(&mut self, direction: f32, now: Instant) {
        // 新的手势输入来了,旧的惯性漂移立即作废(手指接管)
        self.momentum_velocity = 0.0;

        if let Some(last) = self.last_scroll_time {
            let delta = now.duration_since(last).as_secs_f32();
            if delta > 0.0 {
                if delta < TRACKPAD_STREAM_GAP {
                    self.is_trackpad_mode = true; // 粘住,不设置过期
                    self.wheel_click_streak = 0;
                } else if delta > WHEEL_CLICK_GAP {
                    self.wheel_click_streak += 1;
                    if self.wheel_click_streak >= WHEEL_CLICK_STREAK {
                        self.is_trackpad_mode = false;
                    }
                } else {
                    self.wheel_click_streak = 0;
                }

                // 事件率(次/秒)即行速(行/秒):1 事件 = 1 行。EMA 平滑出抬手瞬间速度
                let rate = (1.0 / delta).min(MAX_EVENT_RATE) * direction;
                self.velocity = self.velocity * (1.0 - SPEED_EMA_NEW) + rate * SPEED_EMA_NEW;
            }
        }
        // 手势首事件(无前值可比):沿用上次手势的设备模式,不重新判定——
        // 杜绝旧版静置后首事件先按滚轮窜 3 行、再切触控板变肉的起步顿挫

        self.last_scroll_time = Some(now);
    }
}
