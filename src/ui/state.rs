//! 全屏界面的状态。
//!
//! 只放“界面看到的样子”：卡片、输入、滚动、选区、选择框。会话的真相在 `AgentSession` 里，
//! 界面通过 `ui::projection` 把会话事件变成这里的卡片；接着聊时由 `ui::hydrate` 从会话消息重画。

use super::block::ContentBlock;
use super::picker::PickerState;
use super::scroll::ScrollState;
use super::selection::SelectionState;
use super::spinner::SpinnerState;
use super::toast::ToastState;
use ratatui::layout::Rect;
use std::path::PathBuf;
use std::time::Instant;

/// 登录途中要人填的一行（钥匙、回调链接）。
#[derive(Debug, Clone)]
pub struct LoginPrompt {
    pub message: String,
    pub secret: bool,
    pub input: String,
    /// 空回车之后的一句提示（例如“把回调链接贴回来”）。
    pub notice: Option<String>,
}

pub struct App {
    pub cwd: String,
    pub model_id: String,
    pub model_name: String,
    pub model_provider: String,
    pub context_window: u64,
    pub thinking: String,
    pub context_prompt_tokens: u32,
    pub session_id: String,
    pub session_file: Option<PathBuf>,
    pub tool_count: usize,

    pub blocks: Vec<ContentBlock>,
    pub input: String,
    pub cursor_pos: usize,
    pub input_draft: Option<String>,
    pub history: Vec<String>,
    pub history_idx: Option<usize>,
    pub history_path: Option<PathBuf>,

    pub scroll: ScrollState,
    pub selection: SelectionState,
    pub spinner: SpinnerState,
    pub toast: Option<ToastState>,
    pub next_block_id: usize,
    pub active_assistant_block_id: Option<usize>,
    pub active_tool_start: Option<Instant>,

    pub popup_index: usize,
    pub popup_dismissed: bool,
    /// 模态选择菜单（/model、/effort、/resume、/login 等无参数时弹出；None = 未激活）
    pub picker: Option<PickerState>,
    /// 空闲状态下最近一次 Ctrl+C 的时刻（双击 2 秒窗口内再按才退出）。
    pub last_ctrl_c: Option<Instant>,

    /// 内容区最近一帧的 Rect；划选复制据此把快照行号映射到屏幕行。
    pub content_area: Option<Rect>,
    /// 复制用的最后一帧渲染快照：内容区每行的纯文本（spans 拼接）。
    pub rendered_text_lines: Vec<String>,
    /// 底部状态栏 Rect（点击它复制会话编号）。
    pub status_bar_area: Option<Rect>,

    /// 跑着时排进会话的话，按 xj 的风格画在输入框上方。
    pub pending_steering: Vec<String>,
    pub pending_follow_up: Vec<String>,

    /// 登录提示与授权地址。
    pub login_prompt: Option<LoginPrompt>,
    pub login_url: Option<String>,

    pub mouse_capture_enabled: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            cwd: String::new(),
            model_id: String::new(),
            model_name: String::new(),
            model_provider: String::new(),
            context_window: 0,
            thinking: String::new(),
            context_prompt_tokens: 0,
            session_id: String::new(),
            session_file: None,
            tool_count: 0,
            blocks: Vec::new(),
            input: String::new(),
            cursor_pos: 0,
            input_draft: None,
            history: Vec::new(),
            history_idx: None,
            history_path: None,
            scroll: ScrollState::default(),
            selection: SelectionState::default(),
            spinner: SpinnerState::default(),
            toast: None,
            next_block_id: 1,
            active_assistant_block_id: None,
            active_tool_start: None,
            popup_index: 0,
            popup_dismissed: false,
            picker: None,
            last_ctrl_c: None,
            content_area: None,
            rendered_text_lines: Vec::new(),
            status_bar_area: None,
            pending_steering: Vec::new(),
            pending_follow_up: Vec::new(),
            login_prompt: None,
            login_url: None,
            mouse_capture_enabled: true,
        }
    }

    pub fn show_toast(&mut self, message: impl Into<String>) {
        self.toast = Some(ToastState::new(message));
    }

    pub fn queue_rows(&self) -> u16 {
        (self.pending_steering.len() + self.pending_follow_up.len())
            .min(crate::limits::PENDING_QUEUE_ROWS_DEFAULT) as u16
    }

    pub fn reset_for_session(&mut self) {
        let history = std::mem::take(&mut self.history);
        let history_path = self.history_path.take();
        let mouse = self.mouse_capture_enabled;
        *self = Self::new();
        self.history = history;
        self.history_path = history_path;
        self.mouse_capture_enabled = mouse;
    }

    pub fn set_pending_queue(&mut self, steering: Vec<String>, follow_up: Vec<String>) {
        self.pending_steering = steering;
        self.pending_follow_up = follow_up;
    }

    pub fn clear_login(&mut self) {
        self.login_prompt = None;
        self.login_url = None;
    }

    // ── 输入框 ──

    pub fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
        self.history_idx = None;
    }

    pub fn insert_text(&mut self, text: &str) {
        self.input.insert_str(self.cursor_pos, text);
        self.cursor_pos += text.len();
        self.history_idx = None;
    }

    pub fn backspace(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let prev = self.input[..self.cursor_pos]
            .char_indices()
            .last()
            .map(|(index, _)| index)
            .unwrap_or(0);
        self.input.remove(prev);
        self.cursor_pos = prev;
    }

    pub fn delete_char(&mut self) {
        if self.cursor_pos < self.input.len() {
            self.input.remove(self.cursor_pos);
        }
    }

    pub fn cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos = self.input[..self.cursor_pos]
                .char_indices()
                .last()
                .map(|(index, _)| index)
                .unwrap_or(0);
        }
    }

    pub fn cursor_right(&mut self) {
        if self.cursor_pos < self.input.len() {
            self.cursor_pos = self.input[self.cursor_pos..]
                .char_indices()
                .nth(1)
                .map(|(index, _)| self.cursor_pos + index)
                .unwrap_or(self.input.len());
        }
    }

    pub fn cursor_home(&mut self) {
        let line_start = self.input[..self.cursor_pos]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        self.cursor_pos = line_start;
    }

    pub fn cursor_end(&mut self) {
        let line_end = self.input[self.cursor_pos..]
            .find('\n')
            .map(|index| self.cursor_pos + index)
            .unwrap_or(self.input.len());
        self.cursor_pos = line_end;
    }

    pub fn clear_input(&mut self) {
        self.input.clear();
        self.cursor_pos = 0;
        self.history_idx = None;
        self.input_draft = None;
    }

    pub fn take_input(&mut self) -> String {
        let text = self.input.clone();
        self.input.clear();
        self.cursor_pos = 0;
        self.history_idx = None;
        self.input_draft = None;
        text
    }

    // ── 历史 ──

    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_idx {
            None => {
                self.input_draft = Some(self.input.clone());
                let index = self.history.len() - 1;
                self.history_idx = Some(index);
                self.input = self.history[index].clone();
            }
            Some(index) if index > 0 => {
                let index = index - 1;
                self.history_idx = Some(index);
                self.input = self.history[index].clone();
            }
            _ => {}
        }
        self.cursor_pos = self.input.len();
    }

    pub fn history_next(&mut self) {
        if let Some(index) = self.history_idx {
            if index + 1 < self.history.len() {
                let index = index + 1;
                self.history_idx = Some(index);
                self.input = self.history[index].clone();
            } else {
                self.history_idx = None;
                self.input = self.input_draft.take().unwrap_or_default();
            }
            self.cursor_pos = self.input.len();
        }
    }

    pub fn remember_input(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        self.history.push(text.to_string());
        if let Some(path) = &self.history_path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(file, "{text}");
            }
        }
        let limit = crate::limits::INPUT_HISTORY_ENTRIES_DEFAULT;
        if self.history.len() > limit {
            let drain = self.history.len() - limit;
            self.history.drain(..drain);
        }
    }

    pub fn save_history(&self) {
        let Some(path) = &self.history_path else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, self.history.join("\n"));
    }

    // ── 复制 ──

    pub fn assistant_texts(&self) -> Vec<&str> {
        self.blocks
            .iter()
            .filter_map(|block| match block.kind {
                super::block::BlockKind::AssistantAnswer if !block.content.is_empty() => {
                    Some(block.content.as_str())
                }
                _ => None,
            })
            .collect()
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_editing_keeps_unicode_boundaries() {
        let mut app = App::new();
        app.insert_text("你好");
        app.cursor_left();
        app.insert_text("，paste\n");
        assert_eq!(app.input, "你，paste\n好");
        assert!(app.input.is_char_boundary(app.cursor_pos));
        app.cursor_home();
        assert_eq!(app.cursor_pos, app.input.rfind('\n').unwrap() + 1);
        app.cursor_end();
        assert_eq!(app.cursor_pos, app.input.len());
    }

    #[test]
    fn home_and_end_stay_on_the_current_line() {
        let mut app = App::new();
        app.insert_text("one\ntwo\nthree");
        app.cursor_pos = "one\ntw".len();
        app.cursor_home();
        assert_eq!(app.cursor_pos, "one\n".len());
        app.cursor_end();
        assert_eq!(app.cursor_pos, "one\ntwo".len());
    }
}
