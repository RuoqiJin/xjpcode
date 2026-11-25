#[derive(Debug, Clone, Default)]
pub struct SelectionState {
    pub anchor: Option<(u16, u16)>,
    pub current: Option<(u16, u16)>,
    pub is_selecting: bool,
}

impl SelectionState {
    pub fn start(&mut self, x: u16, y: u16) {
        self.anchor = Some((x, y));
        self.current = Some((x, y));
        self.is_selecting = false;
    }

    pub fn drag(&mut self, x: u16, y: u16) {
        if let Some((ax, ay)) = self.anchor {
            if ax != x || ay != y {
                self.is_selecting = true;
            }
        }
        self.current = Some((x, y));
    }

    pub fn clear(&mut self) {
        self.anchor = None;
        self.current = None;
        self.is_selecting = false;
    }

    pub fn get_normalized_range(&self) -> Option<((u16, u16), (u16, u16))> {
        if !self.is_selecting {
            return None;
        }
        let a = self.anchor?;
        let c = self.current?;

        if a.1 < c.1 || (a.1 == c.1 && a.0 <= c.0) {
            Some((a, c))
        } else {
            Some((c, a))
        }
    }
}
