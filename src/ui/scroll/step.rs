use super::state::ScrollState;

/// 触控板单事件步长:1 事件 = 1 行,事件与视觉 1:1 映射——
/// 旧版 0.4 配整行取整,约四成事件屏幕上不动、然后突跳 1 行,即"颗粒感"的来源
const TRACKPAD_STEP: f32 = 1.0;
/// 滚轮单格步长（低频大步，配合指数缓动）
const WHEEL_STEP: f32 = 3.0;

impl ScrollState {
    pub fn adaptive_step(&self) -> f32 {
        if self.is_trackpad_mode {
            TRACKPAD_STEP
        } else {
            WHEEL_STEP
        }
    }
}
