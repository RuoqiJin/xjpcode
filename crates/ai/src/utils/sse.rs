//! 服务端事件流（SSE）的增量解码。
//!
//! 上游的 OpenAI 路径由 `openai` 这个包代劳切帧，pi 的源码里没有对应文件；Anthropic 路径的
//! `iterateSseMessages` 把规则写清楚了，这里照它落成一份共用的（见 `PORTING.md` 文件对照表）。
//!
//! 规矩：
//! - 行尾可以是 `\n`、`\r\n` 或 `\r`；一个 `\r` 落在这一段的末尾时先留着，等下一段确认是不是 `\r\n`。
//! - 空行结束一个事件；`data:` 的多行按 `\n` 连起来。
//! - `:` 开头是注释，忽略；`event:`/`id:`/`retry:` 记下来，`data` 以外的字段不影响正文。
//! - 流结束时把最后一个没被空行结束的事件交出去（上游同样在 EOF flush）。
//! - UTF-8 跨块：一段字节可能劈开一个多字节字符，所以按字节缓存，只在拿到完整一行时解码。

/// 一个完整的 SSE 事件。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub id: Option<String>,
    pub data: String,
}

/// 增量 SSE 解码器。
#[derive(Debug, Default)]
pub struct SseDecoder {
    /// 还没凑成完整一行的字节。
    buffer: Vec<u8>,
    /// 当前事件已经收到的 `data` 行。
    data_lines: Vec<String>,
    event: Option<String>,
    id: Option<String>,
}

/// 从字节缓冲里取出一行；`\r` 在末尾时返回 `None`（等下一段确认 `\r\n`）。
fn take_line(buffer: &mut Vec<u8>) -> Option<String> {
    let mut index = 0;
    while index < buffer.len() {
        match buffer[index] {
            b'\n' => {
                let line = String::from_utf8_lossy(&buffer[..index]).into_owned();
                buffer.drain(..=index);
                return Some(line);
            }
            b'\r' => {
                if index + 1 == buffer.len() {
                    // 可能是被劈开的 `\r\n`。
                    return None;
                }
                let line = String::from_utf8_lossy(&buffer[..index]).into_owned();
                let width = if buffer[index + 1] == b'\n' { 2 } else { 1 };
                buffer.drain(..index + width);
                return Some(line);
            }
            _ => index += 1,
        }
    }
    None
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂一段字节，交出这段里凑齐的事件。
    pub fn push_bytes(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(line) = take_line(&mut self.buffer) {
            if let Some(event) = self.push_line(&line) {
                events.push(event);
            }
        }
        events
    }

    /// 流结束：把缓冲里剩下的一行与没被空行结束的事件交出去。
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if !self.buffer.is_empty() {
            // 末尾可能是孤立的 `\r`：按行尾处理。
            let bytes = std::mem::take(&mut self.buffer);
            let text = String::from_utf8_lossy(&bytes);
            let line = text.strip_suffix('\r').unwrap_or(&text);
            if let Some(event) = self.push_line(line) {
                events.push(event);
            }
        }
        if let Some(event) = self.flush_event() {
            events.push(event);
        }
        events
    }

    fn push_line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.flush_event();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "data" => self.data_lines.push(value.to_string()),
            "event" => self.event = Some(value.to_string()),
            "id" => self.id = Some(value.to_string()),
            _ => {}
        }
        None
    }

    fn flush_event(&mut self) -> Option<SseEvent> {
        if self.data_lines.is_empty() {
            self.event = None;
            self.id = None;
            return None;
        }
        let event = SseEvent {
            event: self.event.take(),
            id: self.id.take(),
            data: self.data_lines.join("\n"),
        };
        self.data_lines.clear();
        Some(event)
    }
}
