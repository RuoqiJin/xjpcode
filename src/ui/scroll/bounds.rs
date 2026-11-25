use super::state::ScrollState;

impl ScrollState {
    pub fn snap_bounds(&mut self, max_offset: f32) -> bool {
        let mut animating = false;

        // 硬边界物理约束：无弹簧形变，直接锁定在 [0.0, max_offset] 区间
        if self.target < 0.0 {
            self.target = 0.0;
            self.momentum_velocity = 0.0;
        } else if self.target > max_offset {
            self.target = max_offset;
            self.momentum_velocity = 0.0;
        }

        if self.current < 0.0 {
            self.current = 0.0;
            animating = true;
        } else if self.current > max_offset {
            self.current = max_offset;
            animating = true;
        }

        animating
    }
}
