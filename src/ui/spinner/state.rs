use super::verbs::DEFAULT_VERB;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct SpinnerState {
    pub is_processing: bool,
    pub processing_start: Option<Instant>,
    pub frame: usize,
    pub verb: String,
    pub total_chars: usize,
    /// 本轮真实 token 用量 (prompt, completion)；流式期间为 None。
    pub usage: Option<(u64, u64)>,
}

impl Default for SpinnerState {
    fn default() -> Self {
        Self {
            is_processing: false,
            processing_start: None,
            frame: 0,
            verb: DEFAULT_VERB.to_string(),
            total_chars: 0,
            usage: None,
        }
    }
}

impl SpinnerState {
    pub fn start(&mut self, verb: impl Into<String>) {
        self.is_processing = true;
        self.processing_start = Some(Instant::now());
        self.frame = 0;
        self.verb = verb.into();
        self.total_chars = 0;
        self.usage = None;
    }

    pub fn set_verb(&mut self, verb: impl Into<String>) {
        self.verb = verb.into();
    }

    pub fn add_chars(&mut self, count: usize) {
        self.total_chars += count;
    }

    pub fn set_usage(&mut self, prompt_tokens: u64, completion_tokens: u64) {
        self.usage = Some((prompt_tokens, completion_tokens));
    }

    pub fn stop(&mut self) {
        self.is_processing = false;
        self.processing_start = None;
        self.frame = 0;
    }

    /// 动态 Spinner 状态栏高度（正在处理时占用 1 行，空闲时 0 行）。
    pub fn status_bar_height(&self) -> u16 {
        if self.is_processing {
            1
        } else {
            0
        }
    }

    pub fn tick(&mut self, total_frames: usize) {
        if self.is_processing && total_frames > 0 {
            self.frame = (self.frame + 1) % total_frames;
        }
    }

    pub fn elapsed_seconds(&self) -> f32 {
        self.processing_start
            .map(|start| start.elapsed().as_secs_f32())
            .unwrap_or(0.0)
    }

    pub fn format_elapsed(&self) -> String {
        let secs = self.elapsed_seconds();
        if secs < 60.0 {
            format!("{secs:.1}s")
        } else {
            let mins = (secs / 60.0) as u32;
            let rem_secs = (secs % 60.0) as u32;
            format!("{mins}m {rem_secs}s")
        }
    }

    pub fn format_tokens(&self) -> String {
        use crate::format::numbers::humanize_count;
        match self.usage {
            Some((prompt, completion)) => format!(
                "↑{} ↓{} tok",
                humanize_count(prompt),
                humanize_count(completion)
            ),
            None => format!("↓ {} chars", humanize_count(self.total_chars as u64)),
        }
    }
}
