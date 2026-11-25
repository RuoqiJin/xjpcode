//! 对应上游 `src/core/tools/output-accumulator.ts`：边跑命令边留一份有界的尾部，需要时把完整输出
//! 落到临时文件。
//!
//! 上游用 `TextDecoder` 增量解码；这里用一个自己维护残余字节的小解码器，行为相同（分块边界的半个
//! 字符留到下一块，坏字节换成 U+FFFD）。

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use crate::core::tools::truncate::{
    truncate_tail, TruncatedBy, TruncationResult, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES,
};
use crate::utils::output_files::create_output_file_stream;

/// pi: `OutputAccumulatorOptions`
#[derive(Debug, Clone, Default)]
pub struct OutputAccumulatorOptions {
    pub max_lines: Option<usize>,
    pub max_bytes: Option<usize>,
    pub temp_file_prefix: Option<String>,
}

/// pi: `OutputSnapshot`
#[derive(Debug, Clone)]
pub struct OutputSnapshot {
    pub content: String,
    pub truncation: TruncationResult,
    pub full_output_path: Option<PathBuf>,
}

/// pi: `FullOutput`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullOutput {
    pub content: String,
    /// `content` 里有没有省略一部分。
    pub truncated: bool,
}

/// 分块解码 UTF-8：不完整的尾字节留到下一块。
#[derive(Debug, Default)]
struct Utf8StreamDecoder {
    pending: Vec<u8>,
}

impl Utf8StreamDecoder {
    fn decode(&mut self, chunk: &[u8], stream: bool) -> String {
        self.pending.extend_from_slice(chunk);
        let mut output = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    output.push_str(text);
                    self.pending.clear();
                    break;
                }
                Err(error) => {
                    let valid_up_to = error.valid_up_to();
                    if valid_up_to > 0 {
                        output.push_str(std::str::from_utf8(&self.pending[..valid_up_to]).unwrap_or(""));
                    }
                    match error.error_len() {
                        Some(length) => {
                            output.push('\u{FFFD}');
                            self.pending.drain(..valid_up_to + length);
                        }
                        None => {
                            if stream {
                                self.pending.drain(..valid_up_to);
                            } else {
                                output.push('\u{FFFD}');
                                self.pending.clear();
                            }
                            break;
                        }
                    }
                }
            }
        }
        output
    }
}

fn decode_all(bytes: &[u8]) -> String {
    Utf8StreamDecoder::default().decode(bytes, false)
}

/// pi: `OutputAccumulator`
pub struct OutputAccumulator {
    max_lines: usize,
    max_bytes: usize,
    max_rolling_bytes: usize,
    temp_file_prefix: String,
    decoder: Utf8StreamDecoder,

    raw_chunks: Vec<Vec<u8>>,
    tail_text: String,
    tail_bytes: usize,
    tail_starts_at_line_boundary: bool,
    total_raw_bytes: usize,
    total_decoded_bytes: usize,
    completed_lines: usize,
    total_lines: usize,
    current_line_bytes: usize,
    has_open_line: bool,
    finished: bool,

    temp_file_path: Option<PathBuf>,
    temp_file_stream: Option<File>,
}

impl OutputAccumulator {
    /// pi: `new OutputAccumulator(options)`
    pub fn new(options: OutputAccumulatorOptions) -> Self {
        let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
        let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
        Self {
            max_lines,
            max_bytes,
            max_rolling_bytes: (max_bytes * 2).max(1),
            temp_file_prefix: options
                .temp_file_prefix
                .unwrap_or_else(|| "pi-output".to_string()),
            decoder: Utf8StreamDecoder::default(),
            raw_chunks: Vec::new(),
            tail_text: String::new(),
            tail_bytes: 0,
            tail_starts_at_line_boundary: true,
            total_raw_bytes: 0,
            total_decoded_bytes: 0,
            completed_lines: 0,
            total_lines: 0,
            current_line_bytes: 0,
            has_open_line: false,
            finished: false,
            temp_file_path: None,
            temp_file_stream: None,
        }
    }

    /// pi: `append`
    pub fn append(&mut self, data: &[u8]) {
        if self.finished {
            panic!("Cannot append to a finished output accumulator");
        }
        self.total_raw_bytes += data.len();
        let text = self.decoder.decode(data, true);
        self.append_decoded_text(&text);

        if self.temp_file_stream.is_some() || self.should_use_temp_file() {
            self.ensure_temp_file();
            if let Some(file) = &mut self.temp_file_stream {
                let _ = file.write_all(data);
            }
        } else if !data.is_empty() {
            self.raw_chunks.push(data.to_vec());
        }
    }

    /// pi: `finish`
    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let text = self.decoder.decode(&[], false);
        self.append_decoded_text(&text);
        if self.should_use_temp_file() {
            self.ensure_temp_file();
        }
    }

    /// pi: `snapshot`
    pub fn snapshot(&mut self, persist_if_truncated: bool) -> OutputSnapshot {
        let clipped_text = self.get_snapshot_text();
        let snapshot_text = if clipped_text.trim().is_empty() {
            self.tail_text.clone()
        } else {
            clipped_text
        };
        let tail_truncation = truncate_tail(
            &snapshot_text,
            crate::core::tools::truncate::TruncationOptions {
                max_lines: Some(self.max_lines),
                max_bytes: Some(self.max_bytes),
            },
        );
        let truncated = self.total_lines > self.max_lines || self.total_decoded_bytes > self.max_bytes;
        let truncated_by = if truncated {
            tail_truncation.truncated_by.or_else(|| {
                if self.total_decoded_bytes > self.max_bytes {
                    Some(TruncatedBy::Bytes)
                } else {
                    Some(TruncatedBy::Lines)
                }
            })
        } else {
            None
        };
        let truncation = TruncationResult {
            truncated,
            truncated_by,
            total_lines: self.total_lines,
            total_bytes: self.total_decoded_bytes,
            max_lines: self.max_lines,
            max_bytes: self.max_bytes,
            ..tail_truncation
        };

        if persist_if_truncated && truncation.truncated {
            self.ensure_temp_file();
        }

        OutputSnapshot {
            content: truncation.content.clone(),
            truncation,
            full_output_path: self.temp_file_path.clone(),
        }
    }

    /// pi: `closeTempFile`
    pub fn close_temp_file(&mut self) {
        if let Some(mut file) = self.temp_file_stream.take() {
            let _ = file.flush();
        }
    }

    /// pi: `readFullOutput`——给程序调用方看完整输出；超过上限时留头尾、标出省了多少字节。
    pub fn read_full_output(&self, max_bytes: usize) -> FullOutput {
        let Some(path) = &self.temp_file_path else {
            let all: Vec<u8> = self.raw_chunks.concat();
            return FullOutput {
                content: decode_all(&all),
                truncated: false,
            };
        };
        let bytes = std::fs::read(path).unwrap_or_default();
        if bytes.len() <= max_bytes {
            return FullOutput {
                content: decode_all(&bytes),
                truncated: false,
            };
        }
        let head_bytes = max_bytes / 2;
        let tail_bytes = max_bytes - head_bytes;
        let mut head = Vec::from(&bytes[..head_bytes]);
        let mut head_decoder = Utf8StreamDecoder::default();
        let head_text = head_decoder.decode(&head, true);
        head.clear();
        let mut tail_start = bytes.len() - tail_bytes;
        while tail_start < bytes.len() && (bytes[tail_start] & 0xc0) == 0x80 {
            tail_start += 1;
        }
        let tail_text = decode_all(&bytes[tail_start..]);
        let omitted = bytes.len() - head_bytes - tail_bytes;
        FullOutput {
            content: format!("{head_text}\n\n[... {omitted} bytes omitted ...]\n\n{tail_text}"),
            truncated: true,
        }
    }

    /// pi: `getLastLineBytes`
    pub fn get_last_line_bytes(&self) -> usize {
        self.current_line_bytes
    }

    fn append_decoded_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let bytes = text.len();
        self.total_decoded_bytes += bytes;
        self.tail_text.push_str(text);
        self.tail_bytes += bytes;
        if self.tail_bytes > self.max_rolling_bytes * 2 {
            self.trim_tail();
        }

        let mut newlines = 0usize;
        let mut last_newline: Option<usize> = None;
        for (index, character) in text.char_indices() {
            if character == '\n' {
                newlines += 1;
                last_newline = Some(index);
            }
        }
        if newlines == 0 {
            self.current_line_bytes += bytes;
            self.has_open_line = true;
        } else {
            self.completed_lines += newlines;
            let tail = &text[last_newline.expect("last newline") + 1..];
            self.current_line_bytes = tail.len();
            self.has_open_line = !tail.is_empty();
        }
        self.total_lines = self.completed_lines + usize::from(self.has_open_line);
    }

    fn trim_tail(&mut self) {
        let bytes = self.tail_text.as_bytes();
        if bytes.len() <= self.max_rolling_bytes {
            self.tail_bytes = bytes.len();
            return;
        }
        let mut start = bytes.len() - self.max_rolling_bytes;
        while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
            start += 1;
        }
        self.tail_starts_at_line_boundary = if start == 0 {
            self.tail_starts_at_line_boundary
        } else {
            bytes[start - 1] == b'\n'
        };
        self.tail_text = String::from_utf8_lossy(&bytes[start..]).into_owned();
        self.tail_bytes = self.tail_text.len();
    }

    fn get_snapshot_text(&self) -> String {
        if self.tail_starts_at_line_boundary {
            return self.tail_text.clone();
        }
        match self.tail_text.find('\n') {
            Some(index) => self.tail_text[index + 1..].to_string(),
            None => self.tail_text.clone(),
        }
    }

    fn should_use_temp_file(&self) -> bool {
        self.total_raw_bytes > self.max_bytes
            || self.total_decoded_bytes > self.max_bytes
            || self.total_lines > self.max_lines
    }

    fn ensure_temp_file(&mut self) {
        if self.temp_file_path.is_some() {
            return;
        }
        let Ok((path, mut file)) = create_output_file_stream(&self.temp_file_prefix, ".log") else {
            return;
        };
        for chunk in &self.raw_chunks {
            let _ = file.write_all(chunk);
        }
        self.raw_chunks.clear();
        self.temp_file_path = Some(path);
        self.temp_file_stream = Some(file);
    }
}
