use std::time::Instant;

#[derive(Default)]
pub struct ScrollState {
    pub offset: u16,
    pub target: f32,
    pub current: f32,
    /// 最近一次滚动事件的时刻:设备识别、直接跟随窗口、抬手判定共用它
    pub last_scroll_time: Option<Instant>,
    /// 抬手速度估计(带符号,行/秒;1 事件 = 1 行,事件率即行速),EMA 平滑
    pub velocity: f32,
    /// 当前输入设备是否为触控板:高频流事件的确凿证据,一旦识别粘住不过期
    /// (一次会话设备不换);连续两次孤立单击才改判滚轮
    pub is_trackpad_mode: bool,
    /// 惯性漂移速度(带符号,行/秒),dt 积分,与帧率无关
    pub momentum_velocity: f32,
    /// 连续孤立单击计数:>=2 次才把触控板改判为滚轮(防慢速触控板滚动误降级)
    pub wheel_click_streak: u8,
    /// 可视内容行数，由 engine 每帧写入
    pub viewport_lines: u16,
    /// dt 计算的上一帧时刻
    pub last_frame_time: Option<Instant>,
}

impl ScrollState {
    /// 目标滚动位置的唯一步进入口：任何方向的位置调整都限制在非负空间
    pub fn nudge(&mut self, delta: f32) {
        self.target = (self.target + delta).max(0.0);
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// 由 engine 每帧写入当前可视内容行数（翻页步长的数据源）
    pub fn set_viewport(&mut self, lines: u16) {
        self.viewport_lines = lines;
    }

    /// 翻页步长：可视高度减 2 行重叠，至少 1 行（唯一出处）
    pub fn page_step(&self) -> f32 {
        (self.viewport_lines.saturating_sub(2)).max(1) as f32
    }
}
