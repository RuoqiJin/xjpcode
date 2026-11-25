//! 对应上游 `src/core/tools/read.ts`：读文件（文本按行/按字节截断，图片当附件）。
//!
//! 文本可以流式读：只留要显示的那几行，其余只计数，所以超大文件不会整个进内存。图片没有搬上游的
//! 缩放与格式转换（`image-process.ts`），按原样作为附件；见 `docs/port/milestone-3.md` 差异表。

use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::json;
use tokio::io::AsyncReadExt;
use xjpcode_agent::{AgentTool, AgentToolResult, AgentToolUpdateCallback, ToolError};
use xjpcode_ai::types::ConstrainedSamplingStrict;
use xjpcode_ai::{
    AbortSignal, ConstrainedSampling, ConstrainedSamplingConfig, ImageContent, JsonObject,
    TextContent, Tool, ToolResultContentBlock,
};

use crate::core::tools::path_utils::resolve_read_path_async;
use crate::core::tools::truncate::{
    format_size, truncate_head, TruncationResult, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES,
};
use crate::core::tools::ToolSystemPromptContribution;
use crate::utils::mime::detect_supported_image_mime_type_from_file;

/// pi: `readToolSystemPromptContribution`
pub fn read_tool_system_prompt_contribution() -> ToolSystemPromptContribution {
    ToolSystemPromptContribution {
        snippet: "Read file contents",
        guidelines: &["Use read to examine files instead of cat or sed."],
    }
}

pub type ReadFileFn =
    Arc<dyn Fn(PathBuf) -> xjpcode_ai::BoxFuture<'static, std::io::Result<Vec<u8>>> + Send + Sync>;
pub type ReadAccessFn =
    Arc<dyn Fn(PathBuf) -> xjpcode_ai::BoxFuture<'static, std::io::Result<()>> + Send + Sync>;
pub type DetectImageMimeFn =
    Arc<dyn Fn(PathBuf) -> xjpcode_ai::BoxFuture<'static, Option<String>> + Send + Sync>;
pub type CreateReadStreamFn = Arc<
    dyn Fn(PathBuf, Option<AbortSignal>) -> BoxStream<'static, std::io::Result<Vec<u8>>>
        + Send
        + Sync,
>;

/// pi: `ReadOperations`
#[derive(Clone)]
pub struct ReadOperations {
    pub read_file: ReadFileFn,
    pub access: ReadAccessFn,
    pub detect_image_mime_type: Option<DetectImageMimeFn>,
    pub create_read_stream: Option<CreateReadStreamFn>,
}

async fn default_read_file(path: PathBuf) -> std::io::Result<Vec<u8>> {
    tokio::fs::read(path).await
}

async fn default_access(path: PathBuf) -> std::io::Result<()> {
    tokio::fs::File::open(path).await.map(|_| ())
}

async fn default_detect_image_mime_type(path: PathBuf) -> Option<String> {
    detect_supported_image_mime_type_from_file(&path).await
}

/// 本地文件按 64KB 分块读；打开时记下大小，像 `fs.readFile` 一样只读当时那么多（`/proc` 这类报 0
/// 大小的文件读到 EOF）。
fn local_file_chunks(path: PathBuf) -> BoxStream<'static, std::io::Result<Vec<u8>>> {
    struct State {
        path: PathBuf,
        file: Option<tokio::fs::File>,
        remaining: Option<usize>,
    }
    futures::stream::unfold(
        Some(State {
            path,
            file: None,
            remaining: None,
        }),
        |state| async move {
            let Some(mut state) = state else {
                return None;
            };
            if state.file.is_none() {
                match tokio::fs::File::open(&state.path).await {
                    Ok(file) => {
                        let size = file.metadata().await.map(|metadata| metadata.len()).unwrap_or(0);
                        // `fs.readFile` 只读打开那一刻的大小；大小报 0 的文件读到 EOF。
                        state.remaining = if size > 0 { Some(size as usize) } else { None };
                        state.file = Some(file);
                    }
                    Err(error) => return Some((Err(error), None)),
                }
            }
            if state.remaining == Some(0) {
                return None;
            }
            let wanted = state.remaining.unwrap_or(64 * 1024).min(64 * 1024);
            let mut buffer = vec![0u8; wanted];
            let mut file = state.file.take().expect("file");
            let read = match file.read(&mut buffer).await {
                Ok(read) => read,
                Err(error) => return Some((Err(error), None)),
            };
            if read == 0 {
                return None;
            }
            buffer.truncate(read);
            state.file = Some(file);
            if let Some(remaining) = &mut state.remaining {
                *remaining = remaining.saturating_sub(read);
            }
            Some((Ok(buffer), Some(state)))
        },
    )
    .boxed()
}

impl Default for ReadOperations {
    fn default() -> Self {
        Self {
            read_file: Arc::new(|path| Box::pin(default_read_file(path))),
            access: Arc::new(|path| Box::pin(default_access(path))),
            detect_image_mime_type: Some(Arc::new(|path| {
                Box::pin(default_detect_image_mime_type(path))
            })),
            create_read_stream: Some(Arc::new(|path, _signal| local_file_chunks(path))),
        }
    }
}

/// pi: `ReadToolOptions`
#[derive(Clone, Default)]
pub struct ReadToolOptions {
    pub operations: Option<ReadOperations>,
}

/// pi: `ReadToolDetails`
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolDetails {
    pub truncation: Option<TruncationResult>,
}

/// 选出来的行与文件总行数。
struct SelectedLines {
    total_file_lines: usize,
    truncation: TruncationResult,
    first_line_bytes: usize,
}

fn is_streamable_range(offset: Option<f64>, limit: Option<f64>) -> bool {
    offset.is_none_or(|value| value.fract() == 0.0)
        && limit.is_none_or(|value| value.fract() == 0.0 && value >= 0.0)
}

fn js_slice<'a, T>(items: &'a [T], start: f64, end: f64) -> &'a [T] {
    let length = items.len() as f64;
    let normalize = |value: f64| -> usize {
        if value < 0.0 {
            (length + value.trunc()).max(0.0) as usize
        } else {
            value.trunc().min(length) as usize
        }
    };
    let start = normalize(start);
    let end = normalize(end);
    if end <= start {
        &items[start..start]
    } else {
        &items[start..end]
    }
}

/// pi: `selectLinesFromBuffer`
fn select_lines_from_buffer(buffer: &[u8], start_line: f64, limit: Option<f64>) -> SelectedLines {
    let text = String::from_utf8_lossy(buffer);
    let all_lines: Vec<&str> = text.split('\n').collect();
    let end_line = limit
        .map(|limit| (start_line + limit).min(all_lines.len() as f64))
        .unwrap_or(all_lines.len() as f64);
    let selected = js_slice(&all_lines, start_line, end_line).join("\n");
    let truncation = truncate_head(&selected, Default::default());
    let first_line_bytes = if truncation.first_line_exceeds_limit {
        all_lines
            .get(start_line.max(0.0) as usize)
            .map(|line| line.len())
            .unwrap_or(0)
    } else {
        0
    };
    SelectedLines {
        total_file_lines: all_lines.len(),
        truncation,
        first_line_bytes,
    }
}

/// 分块解码 UTF-8：不完整的尾字节留到下一块。
#[derive(Debug, Default)]
struct Utf8StreamDecoder {
    pending: Vec<u8>,
}

impl Utf8StreamDecoder {
    fn decode(&mut self, chunk: &[u8]) -> String {
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
                            self.pending.drain(..valid_up_to);
                            break;
                        }
                    }
                }
            }
        }
        output
    }

    fn finish(&mut self) -> String {
        if self.pending.is_empty() {
            return String::new();
        }
        let text = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        text
    }
}

/// pi: `selectLinesFromStream` 的状态：只留要显示的行，其余只计数。
struct LineSelector {
    start_line: usize,
    end_line: usize,
    stored_lines: Vec<String>,
    stored_bytes: usize,
    stored_past_caps: bool,
    line: usize,
    selected_lines: usize,
    selected_bytes: usize,
    first_line_bytes: usize,
    last_line_empty: bool,
    pieces: Option<Vec<String>>,
    line_bytes: usize,
    line_has_text: bool,
}

impl LineSelector {
    fn new(start_line: usize, end_line: usize) -> Self {
        Self {
            start_line,
            end_line,
            stored_lines: Vec::new(),
            stored_bytes: 0,
            stored_past_caps: false,
            line: 0,
            selected_lines: 0,
            selected_bytes: 0,
            first_line_bytes: 0,
            last_line_empty: true,
            pieces: Some(Vec::new()),
            line_bytes: 0,
            line_has_text: false,
        }
    }

    fn append_to_line(&mut self, text: &str) {
        self.line_has_text = true;
        if let Some(pieces) = &mut self.pieces {
            // 超过字节上限的行整行都不留；第一行只量大小，免得 1GB 的单行进内存。
            if self.line_bytes <= DEFAULT_MAX_BYTES {
                pieces.push(text.to_string());
            }
        }
        self.line_bytes += text.len();
    }

    fn finish_line(&mut self) {
        if let Some(pieces) = &mut self.pieces {
            if self.selected_lines == 0 {
                self.first_line_bytes = self.line_bytes;
            }
            // 多留一行越过上限：`truncateHead` 会忽略末尾空行，要靠下一行判断是否截断。
            if self.stored_lines.len() > DEFAULT_MAX_LINES || self.stored_bytes > DEFAULT_MAX_BYTES {
                self.stored_past_caps = true;
            }
            let text = std::mem::take(pieces).concat();
            self.stored_bytes += text.len() + usize::from(!self.stored_lines.is_empty());
            self.stored_lines.push(text);
        }
        self.selected_lines += 1;
        self.last_line_empty = !self.line_has_text;
        self.pieces = if self.stored_past_caps {
            None
        } else {
            Some(Vec::new())
        };
        self.line_bytes = 0;
        self.line_has_text = false;
    }

    fn select(&mut self, text: &str) {
        let mut position = 0usize;
        // 这一段里选中的文字从哪儿开始；只在选区的最后一行记一次总字节。
        let mut selection_start: Option<usize> = None;
        loop {
            let selected = self.line >= self.start_line && self.line < self.end_line;
            if selected && selection_start.is_none() {
                selection_start = Some(position);
            }
            let newline = text[position..].find('\n').map(|index| index + position);
            let end = newline.unwrap_or(text.len());
            if selected && end > position {
                self.append_to_line(&text[position..end]);
            }
            let Some(newline) = newline else {
                break;
            };
            if selected {
                self.finish_line();
                if self.line + 1 == self.end_line {
                    if let Some(start) = selection_start {
                        self.selected_bytes += text[start..newline].len();
                    }
                    selection_start = None;
                }
            }
            self.line += 1;
            position = newline + 1;
        }
        if let Some(start) = selection_start {
            self.selected_bytes += text[start..].len();
        }
    }

    fn into_selected(self) -> SelectedLines {
        let joined = self.stored_lines.join("\n");
        let head = truncate_head(&joined, Default::default());
        let truncation = TruncationResult {
            total_lines: if self.selected_bytes == 0 {
                0
            } else {
                self.selected_lines - usize::from(self.last_line_empty)
            },
            total_bytes: self.selected_bytes,
            ..head
        };
        SelectedLines {
            total_file_lines: self.line + 1,
            truncation,
            first_line_bytes: self.first_line_bytes,
        }
    }
}

/// pi: `selectLinesFromStream`
async fn select_lines_from_stream(
    mut chunks: BoxStream<'static, std::io::Result<Vec<u8>>>,
    start_line: usize,
    limit: Option<usize>,
    signal: Option<&AbortSignal>,
) -> Result<SelectedLines, ToolError> {
    let end_line = limit
        .map(|limit| start_line.saturating_add(limit))
        .unwrap_or(usize::MAX);
    let mut decoder = Utf8StreamDecoder::default();
    let mut selector = LineSelector::new(start_line, end_line);

    while let Some(chunk) = chunks.next().await {
        if signal.is_some_and(AbortSignal::aborted) {
            return Err(ToolError::from("Operation aborted"));
        }
        let chunk = chunk.map_err(|error| ToolError::from(error.to_string()))?;
        let text = decoder.decode(&chunk);
        selector.select(&text);
    }
    let tail = decoder.finish();
    if !tail.is_empty() {
        selector.select(&tail);
    }
    if selector.line >= start_line && selector.line < end_line {
        selector.finish_line();
    }
    Ok(selector.into_selected())
}

fn js_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

/// pi: `createReadTool`
pub struct ReadTool {
    cwd: PathBuf,
    operations: ReadOperations,
}

impl ReadTool {
    /// 上游工具定义上的 `promptSnippet` / `promptGuidelines`。
    pub fn system_prompt_contribution(&self) -> ToolSystemPromptContribution {
        read_tool_system_prompt_contribution()
    }

    pub fn new(cwd: impl Into<PathBuf>, options: ReadToolOptions) -> Self {
        Self {
            cwd: cwd.into(),
            operations: options.operations.unwrap_or_default(),
        }
    }
}

/// pi: `createReadTool`
pub fn create_read_tool(cwd: impl Into<PathBuf>, options: ReadToolOptions) -> ReadTool {
    ReadTool::new(cwd, options)
}

impl AgentTool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn label(&self) -> &str {
        "read"
    }

    fn declaration(&self) -> Tool {
        Tool {
            name: self.name().to_string(),
            description: format!(
                "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
                DEFAULT_MAX_BYTES / 1024
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the file to read (relative or absolute)" },
                    "offset": { "type": "number", "description": "Line number to start reading from (1-indexed)" },
                    "limit": { "type": "number", "description": "Maximum number of lines to read" }
                },
                "required": ["path"]
            }),
            constrained_sampling: Some(ConstrainedSampling::Config(
                ConstrainedSamplingConfig::JsonSchema {
                    strict: ConstrainedSamplingStrict::Prefer,
                },
            )),
        }
    }

    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: &'a JsonObject,
        signal: Option<&'a AbortSignal>,
        _on_update: Option<&'a AgentToolUpdateCallback>,
    ) -> xjpcode_ai::BoxFuture<'a, Result<AgentToolResult, ToolError>> {
        Box::pin(async move {
            if signal.is_some_and(AbortSignal::aborted) {
                return Err(ToolError::from("Operation aborted"));
            }
            let path = params
                .get("path")
                .and_then(|value| value.as_str())
                .ok_or_else(|| ToolError::from("read requires a path"))?
                .to_string();
            let offset = params.get("offset").and_then(|value| value.as_f64());
            let limit = params.get("limit").and_then(|value| value.as_f64());

            let absolute_path: PathBuf = resolve_read_path_async(&path, &self.cwd.to_string_lossy()).await;
            if signal.is_some_and(AbortSignal::aborted) {
                return Err(ToolError::from("Operation aborted"));
            }
            (self.operations.access)(absolute_path.clone())
                .await
                .map_err(|error| ToolError::from(crate::core::tools::edit_diff::describe_io_error(&error)))?;
            if signal.is_some_and(AbortSignal::aborted) {
                return Err(ToolError::from("Operation aborted"));
            }

            let mime_type = match &self.operations.detect_image_mime_type {
                Some(detect) => detect(absolute_path.clone()).await,
                None => None,
            };

            if let Some(mime_type) = mime_type {
                let buffer = (self.operations.read_file)(absolute_path.clone())
                    .await
                    .map_err(|error| {
                        ToolError::from(crate::core::tools::edit_diff::describe_io_error(&error))
                    })?;
                let data = base64::engine::general_purpose::STANDARD.encode(&buffer);
                let note = format!("Read image file [{mime_type}]");
                let content = vec![
                    ToolResultContentBlock::Text(TextContent::new(note.clone())),
                    ToolResultContentBlock::Image(ImageContent::new(data.clone(), mime_type.clone())),
                ];
                return Ok(AgentToolResult {
                    content,
                    details: None,
                    structured_content: Some(json!({
                        "type": "image",
                        "data": data,
                        "mimeType": mime_type,
                        "note": note,
                    })),
                    usage: None,
                    is_error: None,
                    terminate: None,
                });
            }

            let start_line = offset.filter(|value| *value > 0.0).map(|value| value - 1.0).unwrap_or(0.0);
            let selected = if self.operations.create_read_stream.is_some()
                && is_streamable_range(offset, limit)
            {
                let stream = self
                    .operations
                    .create_read_stream
                    .as_ref()
                    .expect("create read stream")(absolute_path.clone(), signal.cloned());
                select_lines_from_stream(
                    stream,
                    start_line as usize,
                    limit.map(|limit| limit as usize),
                    signal,
                )
                .await?
            } else {
                let buffer = (self.operations.read_file)(absolute_path.clone())
                    .await
                    .map_err(|error| {
                        ToolError::from(crate::core::tools::edit_diff::describe_io_error(&error))
                    })?;
                select_lines_from_buffer(&buffer, start_line, limit)
            };
            if signal.is_some_and(AbortSignal::aborted) {
                return Err(ToolError::from("Operation aborted"));
            }

            if start_line >= selected.total_file_lines as f64 {
                return Err(ToolError::from(format!(
                    "Offset {} is beyond end of file ({} lines total)",
                    js_number(offset.unwrap_or(0.0)),
                    selected.total_file_lines
                )));
            }

            let truncation = selected.truncation;
            let user_limited_lines = limit.map(|limit| {
                (start_line + limit).min(selected.total_file_lines as f64) - start_line
            });
            let start_line_display = js_number(start_line + 1.0);
            let mut details: Option<ReadToolDetails> = None;
            let output_text = if truncation.first_line_exceeds_limit {
                let first_line_size = format_size(selected.first_line_bytes);
                details = Some(ReadToolDetails {
                    truncation: Some(truncation.clone()),
                });
                format!(
                    "[Line {start_line_display} is {first_line_size}, exceeds {} limit. Use bash: sed -n '{}p' {} | head -c {}]",
                    format_size(DEFAULT_MAX_BYTES),
                    start_line_display,
                    path,
                    DEFAULT_MAX_BYTES
                )
            } else if truncation.truncated {
                let end_line_display = start_line + truncation.output_lines as f64;
                let next_offset = end_line_display + 1.0;
                let mut text = truncation.content.clone();
                if truncation.truncated_by
                    == Some(crate::core::tools::truncate::TruncatedBy::Lines)
                {
                    text.push_str(&format!(
                        "\n\n[Showing lines {start_line_display}-{} of {}. Use offset={} to continue.]",
                        js_number(end_line_display),
                        selected.total_file_lines,
                        js_number(next_offset)
                    ));
                } else {
                    text.push_str(&format!(
                        "\n\n[Showing lines {start_line_display}-{} of {} ({} limit). Use offset={} to continue.]",
                        js_number(end_line_display),
                        selected.total_file_lines,
                        format_size(DEFAULT_MAX_BYTES),
                        js_number(next_offset)
                    ));
                }
                details = Some(ReadToolDetails {
                    truncation: Some(truncation.clone()),
                });
                text
            } else if let Some(user_limited_lines) = user_limited_lines {
                if (start_line + user_limited_lines) < selected.total_file_lines as f64 {
                    let remaining = selected.total_file_lines as f64 - (start_line + user_limited_lines);
                    let next_offset = start_line + user_limited_lines + 1.0;
                    format!(
                        "{}\n\n[{} more lines in file. Use offset={} to continue.]",
                        truncation.content,
                        js_number(remaining),
                        js_number(next_offset)
                    )
                } else {
                    truncation.content.clone()
                }
            } else {
                truncation.content.clone()
            };

            Ok(AgentToolResult {
                content: vec![ToolResultContentBlock::Text(TextContent::new(output_text.clone()))],
                details: details.and_then(|details| serde_json::to_value(details).ok()),
                structured_content: Some(json!(output_text)),
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}
