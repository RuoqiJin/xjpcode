use std::time::Instant;

#[derive(Debug, Clone)]
pub struct ToastState {
    pub message: String,
    pub created_at: Instant,
}

impl ToastState {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            created_at: Instant::now(),
        }
    }

    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed().as_secs_f32() >= 3.0
    }
}
