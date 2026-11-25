//! 对应上游 `src/core/tools/truncate.ts`：工具输出按行数或字节数截断，先到哪条算哪条。
//!
//! 上游按 UTF-16 码元数数的两个辅助函数（`sliceHead`/`sliceTail`）照搬了同一套「不切开代理对」
//! 的规则；Rust 字符串是 UTF-8，所以先转成 UTF-16 再切，见 `docs/port/milestone-3.md`。

use serde::{Deserialize, Serialize};

/// pi: `DEFAULT_MAX_LINES`
pub const DEFAULT_MAX_LINES: usize = 2000;
/// pi: `DEFAULT_MAX_BYTES`
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
/// pi: `GREP_MAX_LINE_LENGTH`
pub const GREP_MAX_LINE_LENGTH: usize = 500;

/// pi: `TruncationResult`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncationResult {
    pub content: String,
    pub truncated: bool,
    /// 哪一条限制先到：`lines`、`bytes`，没截断时是 `None`。
    pub truncated_by: Option<TruncatedBy>,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    pub last_line_partial: bool,
    pub first_line_exceeds_limit: bool,
    pub max_lines: usize,
    pub max_bytes: usize,
}

/// pi: `"lines" | "bytes"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TruncatedBy {
    Lines,
    Bytes,
}

/// pi: `TruncationOptions`
#[derive(Debug, Clone, Copy, Default)]
pub struct TruncationOptions {
    pub max_lines: Option<usize>,
    pub max_bytes: Option<usize>,
}

fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn is_blank_line(line: &str) -> bool {
    line.trim().is_empty()
}

/// pi: `formatSize`
pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// pi: `truncateHead`——留下开头。
pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    // 第一行自己就超过字节上限：不给出内容，只报这件事。
    let first_line_bytes = lines.first().map(|line| line.len()).unwrap_or(0);
    if first_line_bytes > max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }

    let mut output_lines_array: Vec<&str> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;

    for (index, line) in lines.iter().enumerate() {
        if index >= max_lines {
            break;
        }
        let line_bytes = line.len() + if index > 0 { 1 } else { 0 };
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        output_lines_array.push(line);
        output_bytes_count += line_bytes;
    }

    if output_lines_array.len() >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = output_lines_array.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: output_lines_array.len(),
        output_bytes: final_output_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// pi: `truncateTail`——留下结尾（跑命令的输出用）。
pub fn truncate_tail(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    let mut output_lines_array: Vec<&str> = Vec::new();
    let mut output_bytes_count = 0usize;
    let mut truncated_by = TruncatedBy::Lines;
    let mut last_line_partial = false;
    let mut partial_owned: Option<String> = None;

    for line in lines.iter().rev() {
        if output_lines_array.len() >= max_lines {
            break;
        }
        let line_bytes = line.len() + if output_lines_array.is_empty() { 0 } else { 1 };
        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            // 这一行自己放不下：如果后面只有空行，就给它结尾一段，免得只剩空行。
            let separator = if output_lines_array.is_empty() { 0 } else { 1 };
            let remaining_bytes = max_bytes.saturating_sub(output_bytes_count + separator);
            if output_lines_array.is_empty()
                || (!is_blank_line(line) && output_lines_array.iter().all(|line| is_blank_line(line)) && remaining_bytes > 0)
            {
                let truncated_line = truncate_string_to_bytes_from_end(line, remaining_bytes);
                output_bytes_count += line_bytes - line.len() + truncated_line.len();
                partial_owned = Some(truncated_line);
                last_line_partial = true;
            }
            break;
        }
        output_lines_array.push(line);
        output_bytes_count += line_bytes;
    }

    if output_lines_array.len() >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let mut ordered: Vec<&str> = output_lines_array.iter().rev().copied().collect();
    if let Some(partial) = &partial_owned {
        ordered.insert(0, partial.as_str());
    }
    let output_content = ordered.join("\n");
    let final_output_bytes = output_content.len();

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: ordered.len(),
        output_bytes: final_output_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// pi: `truncateStringToBytesFromEnd`
fn truncate_string_to_bytes_from_end(text: &str, max_bytes: usize) -> String {
    let bytes = text.as_bytes();
    if bytes.len() <= max_bytes {
        return text.to_string();
    }
    let mut start = bytes.len() - max_bytes;
    while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
        start += 1;
    }
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// pi: `truncateLine`
pub fn truncate_line(line: &str, max_chars: usize) -> (String, bool) {
    let units: Vec<u16> = line.encode_utf16().collect();
    if units.len() <= max_chars {
        return (line.to_string(), false);
    }
    (
        format!("{}... [truncated]", slice_head(line, max_chars)),
        true,
    )
}

/// pi: `sliceHead`——最多前 `max_units` 个 UTF-16 码元，不切开代理对。
pub fn slice_head(text: &str, max_units: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let end = max_units.min(units.len());
    let cut = if end > 0 && end < units.len() && is_high_surrogate(units[end - 1]) && is_low_surrogate(units[end])
    {
        end - 1
    } else {
        end
    };
    String::from_utf16_lossy(&units[..cut])
}

/// pi: `sliceTail`——最多后 `max_units` 个 UTF-16 码元，不切开代理对。
pub fn slice_tail(text: &str, max_units: usize) -> String {
    if max_units == 0 {
        return String::new();
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    let start = units.len().saturating_sub(max_units);
    let start = if start > 0
        && start < units.len()
        && is_low_surrogate(units[start])
        && is_high_surrogate(units[start - 1])
    {
        start + 1
    } else {
        start
    };
    String::from_utf16_lossy(&units[start..])
}

fn is_high_surrogate(unit: u16) -> bool {
    (0xd800..=0xdbff).contains(&unit)
}

fn is_low_surrogate(unit: u16) -> bool {
    (0xdc00..=0xdfff).contains(&unit)
}
