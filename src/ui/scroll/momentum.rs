use super::state::ScrollState;

/// 惯性衰减速率(每秒): 加快衰减率（由 6.3 提升至 14.0），抬手后瞬间极速收敛并紧凑停止
const MOMENTUM_DECAY_RATE: f32 = 14.0;
/// 触发惯性的最小抬手速度(行/秒): 提高到 20.0 行/秒，只有真正快滑才触发惯性，中慢速滑动抬手即停
const MIN_FLICK_SPEED: f32 = 20.0;
/// 抬手判定窗口(毫秒): 缩短至 60ms，抬手意图感应更敏捷
const LIFTOFF_WINDOW_MS: u128 = 60;
/// 惯性自然熄火的最低速度(行/秒)
const MOMENTUM_FLOOR: f32 = 2.0;

impl ScrollState {
    pub fn update_momentum(&mut self, dt: f32) -> bool {
        // 手势结束且抬手瞬间够快,才把残余速度交给惯性;速度随即消费,只点火一次
        if let Some(last_event) = self.last_scroll_time {
            let finger_lifted = last_event.elapsed().as_millis() > LIFTOFF_WINDOW_MS;
            if finger_lifted && self.velocity.abs() > MIN_FLICK_SPEED {
                self.momentum_velocity = self.velocity;
                self.velocity = 0.0;
            }
        }

        if self.momentum_velocity.abs() >= MOMENTUM_FLOOR {
            // dt 积分:位移与帧率无关
            self.nudge(self.momentum_velocity * dt);
            self.momentum_velocity *= (-MOMENTUM_DECAY_RATE * dt).exp();
            true
        } else {
            self.momentum_velocity = 0.0;
            false
        }
    }
}
