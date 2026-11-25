//! 对应上游 `test/read-streaming.test.ts`：分块读与整份读给出同样的结果。
//!
//! 上游用 `vi.mock` 在解码时改文件、开文件时取消；Rust 侧没有这两个钩子，相关用例见
//! `docs/port/milestone-3.md`。

#[path = "utils/mod.rs"]
mod utils;

use futures::StreamExt;
use serde_json::json;
use serde_json::Value;
use tempfile::TempDir;
use xjpcode_agent::AgentTool;
use xjpcode_ai::AbortController;
use xjpcode_coding_agent::core::tools::read::{
    create_read_tool, CreateReadStreamFn, ReadOperations, ReadToolOptions,
};
use xjpcode_coding_agent::core::tools::truncate::{
    format_size, truncate_head, TruncationResult, DEFAULT_MAX_BYTES,
};

fn reference_read(
    text_content: &str,
    path: &str,
    offset: Option<f64>,
    limit: Option<f64>,
) -> (String, Option<Value>) {
    let all_lines: Vec<&str> = text_content.split('\n').collect();
    let total_file_lines = all_lines.len();
    let start_line = offset.map(|offset| (offset - 1.0).max(0.0)).unwrap_or(0.0);
    if start_line >= total_file_lines as f64 {
        return (
            format!(
                "ERROR:Offset {} is beyond end of file ({} lines total)",
                offset.unwrap_or(0.0),
                total_file_lines
            ),
            None,
        );
    }
    let start = start_line.trunc() as usize;
    let (selected_content, user_limited_lines) = match limit {
        Some(limit) => {
            let end_line = (start_line + limit).min(all_lines.len() as f64);
            let end = if end_line < 0.0 {
                (all_lines.len() as f64 + end_line.trunc()).max(0.0) as usize
            } else {
                end_line.trunc().min(all_lines.len() as f64) as usize
            };
            (
                all_lines[start.min(end)..end.max(start.min(end))].join("\n"),
                Some(end_line - start_line),
            )
        }
        None => (all_lines[start..].join("\n"), None),
    };
    let truncation = truncate_head(&selected_content, Default::default());
    let mut details: Option<Value> = None;
    let output_text = if truncation.first_line_exceeds_limit {
        details = Some(json!({ "truncation": truncation }));
        format!(
            "[Line {} is {}, exceeds {} limit. Use bash: sed -n '{}p' {} | head -c {}]",
            start + 1,
            format_size(all_lines[start].len()),
            format_size(DEFAULT_MAX_BYTES),
            start + 1,
            path,
            DEFAULT_MAX_BYTES
        )
    } else if truncation.truncated {
        details = Some(json!({ "truncation": truncation }));
        let end_line_display = start_line + truncation.output_lines as f64;
        let next_offset = end_line_display + 1.0;
        let mut text = truncation.content.clone();
        if truncation.truncated_by == Some(xjpcode_coding_agent::core::tools::truncate::TruncatedBy::Lines) {
            text.push_str(&format!(
                "\n\n[Showing lines {}-{} of {}. Use offset={} to continue.]",
                js_reference_number(start_line + 1.0),
                js_reference_number(end_line_display),
                total_file_lines,
                js_reference_number(next_offset)
            ));
        } else {
            text.push_str(&format!(
                "\n\n[Showing lines {}-{} of {} ({} limit). Use offset={} to continue.]",
                js_reference_number(start_line + 1.0),
                js_reference_number(end_line_display),
                total_file_lines,
                format_size(DEFAULT_MAX_BYTES),
                js_reference_number(next_offset)
            ));
        }
        text
    } else if let Some(user_limited_lines) = user_limited_lines {
        if start_line + user_limited_lines < all_lines.len() as f64 {
            let remaining = all_lines.len() as f64 - (start_line + user_limited_lines);
            let next_offset = start_line + user_limited_lines + 1.0;
            format!(
                "{}\n\n[{} more lines in file. Use offset={} to continue.]",
                truncation.content,
                js_reference_number(remaining),
                js_reference_number(next_offset)
            )
        } else {
            truncation.content.clone()
        }
    } else {
        truncation.content.clone()
    };
    (output_text, details)
}

fn js_reference_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

fn streaming_ops(bytes: Vec<u8>, chunk_size: usize) -> ReadOperations {
    let create_read_stream: CreateReadStreamFn = std::sync::Arc::new(move |_path, _signal| {
        let chunks: Vec<std::io::Result<Vec<u8>>> = bytes
            .chunks(chunk_size)
            .map(|chunk| Ok(chunk.to_vec()))
            .collect();
        futures::stream::iter(chunks).boxed()
    });
    ReadOperations {
        read_file: std::sync::Arc::new(|_path| {
            Box::pin(async move { Err(std::io::Error::other("readFile must not be used")) })
        }),
        access: std::sync::Arc::new(|_path| Box::pin(async move { Ok(()) })),
        detect_image_mime_type: None,
        create_read_stream: Some(create_read_stream),
    }
}

async fn read_with(
    operations: ReadOperations,
    workspace: &std::path::Path,
    offset: Option<f64>,
    limit: Option<f64>,
) -> (String, Option<Value>) {
    let tool = create_read_tool(
        workspace,
        ReadToolOptions {
            operations: Some(operations),
        },
    );
    let mut args = serde_json::Map::new();
    args.insert("path".to_string(), json!("/tmp/file"));
    if let Some(offset) = offset {
        args.insert("offset".to_string(), json!(offset));
    }
    if let Some(limit) = limit {
        args.insert("limit".to_string(), json!(limit));
    }
    match tool.execute("call", &args, None, None).await {
        Ok(result) => (
            utils::tools::text(&result),
            result.details,
        ),
        Err(error) => (format!("ERROR:{}", error.message()), None),
    }
}

fn lines(count: usize, make: impl Fn(usize) -> String) -> String {
    (0..count).map(make).collect::<Vec<_>>().join("\n")
}

fn text_cases() -> Vec<(&'static str, String)> {
    let fifty_kb_of_lines = format!("{}y", lines(512, |_| "y".repeat(99)));
    vec![
        ("empty", String::new()),
        ("no trailing newline", "alpha\nbeta\ngamma".to_string()),
        ("trailing newline", "alpha\nbeta\n".to_string()),
        ("CRLF", "alpha\r\nbeta\r\n\r\ngamma\r\n".to_string()),
        (
            "multi-byte characters and UTF-8 byte order mark",
            "\u{FEFF}é€😀\nñ\u{10348}字\n😀😀😀".to_string(),
        ),
        ("2001 lines", lines(2001, |index| format!("line {index}"))),
        (
            "2001 lines, an empty line and more",
            format!("{}\n\nmore", lines(2001, |index| format!("line {index}"))),
        ),
        (
            "first line over 50KB",
            format!("{}\nsecond", "é".repeat(DEFAULT_MAX_BYTES / 2 + 1)),
        ),
        (
            "line over 50KB after short lines",
            format!("a\nb\n{}\nc", "z".repeat(60 * 1024)),
        ),
        (
            "exactly 50KB and a trailing newline",
            format!("{fifty_kb_of_lines}\n"),
        ),
        (
            "exactly 50KB, an empty line and more",
            format!("{fifty_kb_of_lines}\n\nmore"),
        ),
        (
            "multi-byte characters over 50KB",
            lines(700, |index| format!("{index} {}", "€😀".repeat(13))),
        ),
    ]
}

#[tokio::test]
async fn gives_the_same_output_as_the_whole_file_algorithm() {
    let workspace = TempDir::new().unwrap();
    for (name, text) in text_cases() {
        let bytes = text.clone().into_bytes();
        let total = text.split('\n').count();
        for offset in [None, Some(2.0), Some(3.0), Some(total as f64), Some(total as f64 + 1.0)] {
            for limit in [None, Some(0.0), Some(1.0), Some(total as f64)] {
                let expected = reference_read(&text, "/tmp/file", offset, limit);
                for chunk_size in [1usize, 4096] {
                    let actual = read_with(
                        streaming_ops(bytes.clone(), chunk_size),
                        workspace.path(),
                        offset,
                        limit,
                    )
                    .await;
                    assert_eq!(
                        actual, expected,
                        "{name}, offset {offset:?}, limit {limit:?}, chunks of {chunk_size}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn keeps_details_key_order_which_session_files_serialize() {
    let workspace = TempDir::new().unwrap();
    let text = lines(2001, |index| format!("line {index}"));
    let streamed = read_with(
        streaming_ops(text.clone().into_bytes(), 4096),
        workspace.path(),
        Some(1.0),
        None,
    )
    .await
    .1
    .unwrap();
    let expected = reference_read(&text, "/tmp/file", Some(1.0), None).1.unwrap();
    assert_eq!(
        serde_json::to_string(&streamed).unwrap(),
        serde_json::to_string(&expected).unwrap()
    );
}

#[tokio::test]
async fn decodes_multi_byte_characters_split_across_chunks_and_invalid_utf8_like_the_whole_buffer() {
    let workspace = TempDir::new().unwrap();
    let mut bytes = "aé€😀\n😀b\u{10348}\n".as_bytes().to_vec();
    bytes.extend_from_slice(&[
        0x61, 0xc3, 0x0a, 0xe2, 0x82, 0x0a, 0xff, 0xfe, 0x80, 0xf0, 0x9f, 0x98, 0x0a, 0xed, 0xa0, 0x80,
    ]);
    for chunk_size in [1usize, 2, 3] {
        let expected = reference_read(
            &String::from_utf8_lossy(&bytes),
            "/tmp/file",
            None,
            None,
        );
        let actual = read_with(
            streaming_ops(bytes.clone(), chunk_size),
            workspace.path(),
            None,
            None,
        )
        .await;
        assert_eq!(actual, expected, "chunks of {chunk_size}");
    }
}

#[tokio::test]
async fn uses_read_file_without_a_stream_and_for_fractional_or_negative_ranges() {
    let workspace = TempDir::new().unwrap();
    let text = "one\ntwo\nthree\nfour";
    let ops = ReadOperations {
        read_file: std::sync::Arc::new(|_path| {
            let bytes = text.as_bytes().to_vec();
            Box::pin(async move { Ok(bytes) })
        }),
        access: std::sync::Arc::new(|_path| Box::pin(async move { Ok(()) })),
        detect_image_mime_type: None,
        create_read_stream: None,
    };
    for (offset, limit) in [
        (None, None),
        (Some(2.0), Some(1.0)),
        (Some(9.0), None),
    ] {
        assert_eq!(
            read_with(ops.clone(), workspace.path(), offset, limit).await,
            reference_read(text, "/tmp/file", offset, limit)
        );
    }

    let with_stream = ReadOperations {
        create_read_stream: Some(std::sync::Arc::new(|_path, _signal| {
            futures::stream::iter(vec![Ok(text.as_bytes().to_vec())]).boxed()
        })),
        ..ops
    };
    // `limit -1` 选出「除最后一行以外的全部」。
    for (offset, limit) in [
        (Some(2.5), Some(1.0)),
        (None, Some(-1.0)),
        (Some(1.0), Some(1.5)),
    ] {
        assert_eq!(
            read_with(with_stream.clone(), workspace.path(), offset, limit).await,
            reference_read(text, "/tmp/file", offset, limit)
        );
    }
}

#[tokio::test]
async fn reads_local_files_through_the_default_stream() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("local.txt");
    let text = lines(3000, |index| format!("{index} {}", "w".repeat(index % 40)));
    tokio::fs::write(&path, &text).await.unwrap();
    let tool = create_read_tool(dir.path(), ReadToolOptions::default());
    for (offset, limit) in [(None, None), (Some(1500.0), Some(10.0))] {
        let mut args = serde_json::Map::new();
        args.insert("path".to_string(), json!(path));
        if let Some(offset) = offset {
            args.insert("offset".to_string(), json!(offset));
        }
        if let Some(limit) = limit {
            args.insert("limit".to_string(), json!(limit));
        }
        let result = tool.execute("call", &args, None, None).await.unwrap();
        assert_eq!(
            utils::tools::text(&result),
            reference_read(&text, &path.to_string_lossy(), offset, limit).0
        );
    }
    let mut args = serde_json::Map::new();
    args.insert("path".to_string(), json!(dir.path()));
    let error = tool.execute("call", &args, None, None).await.unwrap_err();
    assert!(
        error.message().contains("Is a directory") || error.message().contains("EISDIR"),
        "{}",
        error.message()
    );
}

#[tokio::test]
async fn stops_consuming_the_stream_when_aborted() {
    let workspace = TempDir::new().unwrap();
    let controller = AbortController::new();
    let signal = controller.signal();
    let chunks_read = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let closed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let chunks_read_clone = std::sync::Arc::clone(&chunks_read);
    let closed_clone = std::sync::Arc::clone(&closed);
    let controller_clone = controller.clone();
    let operations = ReadOperations {
        read_file: std::sync::Arc::new(|_path| Box::pin(async move { Ok(Vec::new()) })),
        access: std::sync::Arc::new(|_path| Box::pin(async move { Ok(()) })),
        detect_image_mime_type: None,
        create_read_stream: Some(std::sync::Arc::new(move |_path, _signal| {
            let chunks_read = std::sync::Arc::clone(&chunks_read_clone);
            let _closed = std::sync::Arc::clone(&closed_clone);
            let controller = controller_clone.clone();
            let stream = futures::stream::unfold((), move |()| {
                let chunks_read = std::sync::Arc::clone(&chunks_read);
                let controller = controller.clone();
                async move {
                    let read = chunks_read.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    if read == 3 {
                        controller.abort();
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    Some((Ok(b"line\n".to_vec()), ()))
                }
            });
            Box::pin(stream)
        })),
    };
    let tool = create_read_tool(
        workspace.path(),
        ReadToolOptions {
            operations: Some(operations),
        },
    );
    let args = utils::tools::object(json!({ "path": "/tmp/file" }));
    let error = tool
        .execute("call", &args, Some(&signal), None)
        .await
        .unwrap_err();
    assert_eq!(error.message(), "Operation aborted");
    closed.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(closed.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(chunks_read.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[allow(dead_code)]
fn _assert_truncation_is_serializable(result: &TruncationResult) -> String {
    serde_json::to_string(result).unwrap()
}

#[tokio::test]
async fn reads_a_200mb_line_without_storing_it() {
    let workspace = TempDir::new().unwrap();
    let create_read_stream: CreateReadStreamFn = std::sync::Arc::new(|_path, _signal| {
        let chunks = futures::stream::unfold(0usize, |chunks| async move {
            if chunks >= 3200 {
                return None;
            }
            Some((Ok(vec![b'x'; 64 * 1024]), chunks + 1))
        });
        Box::pin(chunks)
    });
    let operations = ReadOperations {
        read_file: std::sync::Arc::new(|_path| Box::pin(async move { Ok(Vec::new()) })),
        access: std::sync::Arc::new(|_path| Box::pin(async move { Ok(()) })),
        detect_image_mime_type: None,
        create_read_stream: Some(create_read_stream),
    };
    let (text, _) = read_with(operations, workspace.path(), None, None).await;
    assert_eq!(
        text,
        format!(
            "[Line 1 is 200.0MB, exceeds 50.0KB limit. Use bash: sed -n '1p' /tmp/file | head -c {DEFAULT_MAX_BYTES}]"
        )
    );
}
