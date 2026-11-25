use super::state::ScrollState;
use std::time::Instant;

/// 滚轮/翻页指数缓动的收敛速率（每秒）：约 130ms 收敛 90%，快攻不肉
const EASE_RATE: f32 = 20.0;
/// 单帧 dt 上限：窗口卡顿后不允许动画大步跳跃
const MAX_DT: f32 = 0.05;
/// 直接跟随窗口(毫秒):最近有此时间内的触控板流事件才手指直跟;
/// 超窗(键盘翻页/手势间歇/惯性漂移)一律走指数缓动
const DIRECT_FOLLOW_WINDOW_MS: u128 = 150;

impl ScrollState {
    pub fn update_animation(&mut self, max_offset: f32) -> bool {
        let now = Instant::now();
        let dt = self
            .last_frame_time
            .map(|t| now.duration_since(t).as_secs_f32())
            .unwrap_or(1.0 / 60.0)
            .min(MAX_DT);
        self.last_frame_time = Some(now);

        // 1. 优先钳制触底/触顶硬边界（在跟随前把越界的 target/current 锁回合法区间）
        let mut is_animating = self.snap_bounds(max_offset);

        // 2. 惯性衰减（dt 积分，见 momentum.rs）
        if self.update_momentum(dt) {
            is_animating = true;
        }

        // 3. 位置跟进（60FPS 极细颗粒度逐行指数缓动）
        let diff = self.target - self.current;
        let direct_follow = self.is_trackpad_mode
            && self
                .last_scroll_time
                .is_some_and(|t| t.elapsed().as_millis() < DIRECT_FOLLOW_WINDOW_MS);
        if direct_follow {
            // 触控板手势进行中：直接操纵。无缓动层，流畅感来自手指跟随 + 惯性物理
            self.current = self.target;
        } else if diff.abs() > 0.01 {
            // 滚轮/翻页/惯性：指数缓动（帧率无关，逐行连续）
            self.current += diff * (1.0 - (-EASE_RATE * dt).exp());
            is_animating = true;
        } else {
            self.current = self.target;
        }

        let new_offset = self.current.round().max(0.0) as u16;
        if new_offset != self.offset {
            self.offset = new_offset;
            is_animating = true;
        }

        is_animating
    }
}
