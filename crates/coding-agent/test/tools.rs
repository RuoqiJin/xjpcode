//! 对应上游 `test/tools.test.ts` 里的读、写、改、跑命令四段（grep/find/ls 不在本次范围）。
//!
//! 上游用 `ctx.cwd` 覆盖工作目录；Rust 侧工具在构造时拿 cwd，所以那几条改成用不同的 cwd 建工具。

#[path = "utils/mod.rs"]
mod utils;

use std::path::Path;

use base64::Engine;
use serde_json::{json, Value};
use tempfile::TempDir;
use xjpcode_agent::AgentTool;
use xjpcode_coding_agent::core::tools::bash::{create_bash_tool, create_local_bash_operations, BashOperations};
use xjpcode_coding_agent::core::tools::edit::create_edit_tool;
use xjpcode_coding_agent::core::tools::edit_diff::{
    apply_edits_to_normalized_content, apply_replacements_to_original, compute_edits_diff, normalize_to_lf,
    Edit,
};
use xjpcode_coding_agent::core::tools::read::create_read_tool;
use xjpcode_coding_agent::core::tools::write::create_write_tool;
use xjpcode_coding_agent::{BashToolOptions, EditToolOptions, ReadToolOptions, WriteToolOptions};

fn read_text(result: &xjpcode_agent::AgentToolResult) -> String {
    utils::tools::text(result)
}

fn details(result: &xjpcode_agent::AgentToolResult) -> &Value {
    result.details.as_ref().expect("details")
}

fn read_error(error: xjpcode_agent::ToolError) -> String {
    error.message().to_string()
}

fn write(path: &Path, content: &str) {
    std::fs::write(path, content).unwrap();
}

fn write_bytes(path: &Path, content: &[u8]) {
    std::fs::write(path, content).unwrap();
}

// ---------------------------------------------------------------------------
// read
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_reads_file_contents_within_limits() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("test.txt");
    let content = "Hello, world!\nLine 2\nLine 3";
    write(&file, content);
    let tool = create_read_tool(dir.path(), ReadToolOptions::default());
    let result = utils::tools::execute(&tool, json!({ "path": file }))
        .await
        .unwrap();
    assert_eq!(read_text(&result), content);
    assert!(!read_text(&result).contains("Use offset="));
    assert!(result.details.is_none());
    assert_eq!(result.structured_content, Some(json!(content)));
}

#[tokio::test]
async fn read_handles_non_existent_files() {
    let dir = TempDir::new().unwrap();
    let tool = create_read_tool(dir.path(), ReadToolOptions::default());
    let error = utils::tools::execute(&tool, json!({ "path": dir.path().join("nonexistent.txt") }))
        .await
        .unwrap_err();
    let message = read_error(error);
    assert!(
        message.contains("ENOENT") || message.to_lowercase().contains("not found"),
        "{message}"
    );
}

#[tokio::test]
async fn read_truncates_files_exceeding_line_limit() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("large.txt");
    let lines: Vec<String> = (1..=2500).map(|index| format!("Line {index}")).collect();
    write(&file, &lines.join("\n"));
    let tool = create_read_tool(dir.path(), ReadToolOptions::default());
    let result = utils::tools::execute(&tool, json!({ "path": file }))
        .await
        .unwrap();
    let output = read_text(&result);
    assert!(output.contains("Line 1"));
    assert!(output.contains("Line 2000"));
    assert!(!output.contains("Line 2001"));
    assert!(output.contains("[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]"));
}

#[tokio::test]
async fn read_truncates_when_byte_limit_exceeded() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("large-bytes.txt");
    let lines: Vec<String> = (1..=500)
        .map(|index| format!("Line {index}: {}", "x".repeat(200)))
        .collect();
    write(&file, &lines.join("\n"));
    let tool = create_read_tool(dir.path(), ReadToolOptions::default());
    let result = utils::tools::execute(&tool, json!({ "path": file }))
        .await
        .unwrap();
    let output = read_text(&result);
    assert!(output.contains("Line 1:"));
    assert!(output.contains("limit). Use offset="));
}

#[tokio::test]
async fn read_handles_offset_and_limit() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("offset-test.txt");
    let lines: Vec<String> = (1..=100).map(|index| format!("Line {index}")).collect();
    write(&file, &lines.join("\n"));
    let tool = create_read_tool(dir.path(), ReadToolOptions::default());

    let result = utils::tools::execute(&tool, json!({ "path": file, "offset": 51 }))
        .await
        .unwrap();
    let output = read_text(&result);
    assert!(!output.contains("Line 50"));
    assert!(output.contains("Line 51"));
    assert!(output.contains("Line 100"));
    assert!(!output.contains("Use offset="));

    let result = utils::tools::execute(&tool, json!({ "path": file, "limit": 10 }))
        .await
        .unwrap();
    let output = read_text(&result);
    assert!(output.contains("Line 1"));
    assert!(output.contains("Line 10"));
    assert!(!output.contains("Line 11"));
    assert!(output.contains("[90 more lines in file. Use offset=11 to continue.]"));

    let result = utils::tools::execute(&tool, json!({ "path": file, "offset": 41, "limit": 20 }))
        .await
        .unwrap();
    let output = read_text(&result);
    assert!(!output.contains("Line 40"));
    assert!(output.contains("Line 41"));
    assert!(output.contains("Line 60"));
    assert!(!output.contains("Line 61"));
    assert!(output.contains("[40 more lines in file. Use offset=61 to continue.]"));
}

#[tokio::test]
async fn read_shows_error_when_offset_is_beyond_file_length() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("short.txt");
    write(&file, "Line 1\nLine 2\nLine 3");
    let tool = create_read_tool(dir.path(), ReadToolOptions::default());
    let error = utils::tools::execute(&tool, json!({ "path": file, "offset": 100 }))
        .await
        .unwrap_err();
    assert_eq!(
        read_error(error),
        "Offset 100 is beyond end of file (3 lines total)"
    );
}

#[tokio::test]
async fn read_includes_truncation_details_when_truncated() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("large-file.txt");
    let lines: Vec<String> = (1..=2500).map(|index| format!("Line {index}")).collect();
    write(&file, &lines.join("\n"));
    let tool = create_read_tool(dir.path(), ReadToolOptions::default());
    let result = utils::tools::execute(&tool, json!({ "path": file }))
        .await
        .unwrap();
    let truncation = &details(&result)["truncation"];
    assert_eq!(truncation["truncated"], true);
    assert_eq!(truncation["truncatedBy"], "lines");
    assert_eq!(truncation["totalLines"], 2500);
    assert_eq!(truncation["outputLines"], 2000);
}

#[tokio::test]
async fn read_detects_image_mime_type_from_file_magic() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("image.txt");
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAX+XDSwAAAABJRU5ErkJggg==")
        .unwrap();
    write_bytes(&file, &png);
    let tool = create_read_tool(dir.path(), ReadToolOptions::default());
    let result = utils::tools::execute(&tool, json!({ "path": file }))
        .await
        .unwrap();
    assert!(matches!(
        result.content[0],
        xjpcode_ai::ToolResultContentBlock::Text(_)
    ));
    assert!(read_text(&result).contains("Read image file [image/png]"));
    let image = result
        .content
        .iter()
        .find_map(|block| match block {
            xjpcode_ai::ToolResultContentBlock::Image(image) => Some(image),
            _ => None,
        })
        .expect("image block");
    assert_eq!(image.mime_type, "image/png");
    assert!(!image.data.is_empty());
    assert_eq!(
        result.structured_content,
        Some(json!({
            "type": "image",
            "data": image.data,
            "mimeType": "image/png",
            "note": read_text(&result),
        }))
    );
}

#[tokio::test]
async fn read_treats_files_with_image_extension_but_non_image_content_as_text() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("not-an-image.png");
    write(&file, "definitely not a png");
    let tool = create_read_tool(dir.path(), ReadToolOptions::default());
    let result = utils::tools::execute(&tool, json!({ "path": file }))
        .await
        .unwrap();
    assert!(read_text(&result).contains("definitely not a png"));
    assert!(!result
        .content
        .iter()
        .any(|block| matches!(block, xjpcode_ai::ToolResultContentBlock::Image(_))));
}

// ---------------------------------------------------------------------------
// write
// ---------------------------------------------------------------------------

#[tokio::test]
async fn write_writes_file_contents() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("write-test.txt");
    let tool = create_write_tool(dir.path(), WriteToolOptions::default());
    let result = utils::tools::execute(&tool, json!({ "path": file, "content": "Test content" }))
        .await
        .unwrap();
    assert_eq!(
        read_text(&result),
        format!("Successfully wrote to {}", file.to_string_lossy())
    );
    assert!(result.details.is_none());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "Test content");
}

#[tokio::test]
async fn write_creates_parent_directories() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("nested").join("dir").join("test.txt");
    let tool = create_write_tool(dir.path(), WriteToolOptions::default());
    let result = utils::tools::execute(&tool, json!({ "path": file, "content": "Nested content" }))
        .await
        .unwrap();
    assert!(read_text(&result).contains("Successfully wrote"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "Nested content");
}

// ---------------------------------------------------------------------------
// edit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn edit_replaces_text_in_file() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("edit-test.txt");
    write(&file, "Hello, world!");
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    let result = utils::tools::execute(
        &tool,
        json!({ "path": file, "edits": [{ "oldText": "world", "newText": "testing" }] }),
    )
    .await
    .unwrap();
    assert!(read_text(&result).contains("Successfully replaced"));
    let details = details(&result);
    assert!(details["diff"].as_str().unwrap().contains("testing"));
    let patch = details["patch"].as_str().unwrap();
    assert!(patch.contains("--- "));
    assert!(patch.contains("+++ "));
    assert!(patch.contains("@@"));
    assert!(patch.contains("-Hello, world!"));
    assert!(patch.contains("+Hello, testing!"));
    assert_eq!(
        utils::patch::apply_patch("Hello, world!", patch).unwrap(),
        "Hello, testing!"
    );
}

#[tokio::test]
async fn edit_fails_if_text_not_found() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("edit-test.txt");
    write(&file, "Hello, world!");
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    let error = utils::tools::execute(
        &tool,
        json!({ "path": file, "edits": [{ "oldText": "nonexistent", "newText": "testing" }] }),
    )
    .await
    .unwrap_err();
    assert!(read_error(error).contains("Could not find the exact text"));
}

#[tokio::test]
async fn edit_includes_enoent_when_target_does_not_exist() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("missing.txt");
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    let error = utils::tools::execute(
        &tool,
        json!({ "path": missing, "edits": [{ "oldText": "hello", "newText": "world" }] }),
    )
    .await
    .unwrap_err();
    assert_eq!(
        read_error(error),
        format!(
            "Could not edit file: {}. Error code: ENOENT.",
            missing.to_string_lossy()
        )
    );
}

#[tokio::test]
async fn edit_fails_if_text_appears_multiple_times() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("edit-test.txt");
    write(&file, "foo foo foo");
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    let error = utils::tools::execute(
        &tool,
        json!({ "path": file, "edits": [{ "oldText": "foo", "newText": "bar" }] }),
    )
    .await
    .unwrap_err();
    assert!(read_error(error).contains("Found 3 occurrences"));
}

#[tokio::test]
async fn edit_replaces_multiple_disjoint_regions_in_one_call() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("edit-multi.txt");
    write(&file, "alpha\nbeta\ngamma\ndelta\n");
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    let result = utils::tools::execute(
        &tool,
        json!({
            "path": file,
            "edits": [
                { "oldText": "alpha\n", "newText": "ALPHA\n" },
                { "oldText": "gamma\n", "newText": "GAMMA\n" }
            ]
        }),
    )
    .await
    .unwrap();
    assert!(read_text(&result).contains("Successfully replaced 2 block(s)"));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "ALPHA\nbeta\nGAMMA\ndelta\n"
    );
    let diff = details(&result)["diff"].as_str().unwrap();
    assert!(diff.contains("ALPHA"));
    assert!(diff.contains("GAMMA"));
}

#[tokio::test]
async fn edit_collapses_large_unchanged_gaps_in_multi_edit_diffs() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("edit-multi-large-gap.txt");
    let lines: Vec<String> = (1..=600).map(|index| format!("line {index:03}")).collect();
    write(&file, &format!("{}\n", lines.join("\n")));
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    let result = utils::tools::execute(
        &tool,
        json!({
            "path": file,
            "edits": [
                { "oldText": "line 100\n", "newText": "LINE 100\n" },
                { "oldText": "line 300\n", "newText": "LINE 300\n" },
                { "oldText": "line 500\n", "newText": "LINE 500\n" }
            ]
        }),
    )
    .await
    .unwrap();
    let diff = details(&result)["diff"].as_str().unwrap();
    assert!(diff.contains("LINE 100"));
    assert!(diff.contains("LINE 300"));
    assert!(diff.contains("LINE 500"));
    assert!(diff.contains("..."));
    assert!(!diff.contains("line 250"));
    assert!(diff.split('\n').count() < 50);
}

#[tokio::test]
async fn edit_matches_edits_against_the_original_file_not_incrementally() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("edit-multi-original.txt");
    write(&file, "foo\nbar\nbaz\n");
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    utils::tools::execute(
        &tool,
        json!({
            "path": file,
            "edits": [
                { "oldText": "foo\n", "newText": "foo bar\n" },
                { "oldText": "bar\n", "newText": "BAR\n" }
            ]
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "foo bar\nBAR\nbaz\n"
    );
}

#[tokio::test]
async fn edit_fails_when_edits_is_empty() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("edit-empty-edits.txt");
    write(&file, "hello\nworld\n");
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    let error = utils::tools::execute(&tool, json!({ "path": file, "edits": [] }))
        .await
        .unwrap_err();
    assert!(read_error(error).contains("edits must contain at least one replacement"));
}

#[tokio::test]
async fn edit_fails_when_multi_edit_regions_overlap() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("edit-overlap.txt");
    write(&file, "one\ntwo\nthree\n");
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    let error = utils::tools::execute(
        &tool,
        json!({
            "path": file,
            "edits": [
                { "oldText": "one\ntwo\n", "newText": "ONE\nTWO\n" },
                { "oldText": "two\nthree\n", "newText": "TWO\nTHREE\n" }
            ]
        }),
    )
    .await
    .unwrap_err();
    assert!(read_error(error).contains("overlap"));
}

#[tokio::test]
async fn edit_does_not_partially_apply_when_one_edit_fails() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("edit-no-partial.txt");
    let original = "alpha\nbeta\ngamma\n";
    write(&file, original);
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    let error = utils::tools::execute(
        &tool,
        json!({
            "path": file,
            "edits": [
                { "oldText": "alpha\n", "newText": "ALPHA\n" },
                { "oldText": "missing\n", "newText": "MISSING\n" }
            ]
        }),
    )
    .await
    .unwrap_err();
    assert!(read_error(error).contains("Could not find"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
}

#[cfg(unix)]
#[tokio::test]
async fn edit_includes_eacces_for_read_only_files() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("edit-readonly.txt");
    write(&file, "hello\n");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    let error = utils::tools::execute(
        &tool,
        json!({ "path": file, "edits": [{ "oldText": "hello", "newText": "world" }] }),
    )
    .await
    .unwrap_err();
    assert_eq!(
        read_error(error),
        format!(
            "Could not edit file: {}. Error code: EACCES.",
            file.to_string_lossy()
        )
    );
}

#[tokio::test]
async fn edit_includes_the_original_error_message_for_unknown_access_errors() {
    let dir = TempDir::new().unwrap();
    let mut operations = xjpcode_coding_agent::EditOperations::default();
    operations.access = std::sync::Arc::new(|_path| {
        Box::pin(async move { Err(std::io::Error::other("disk offline")) })
    });
    operations.read_file = std::sync::Arc::new(|_path| {
        Box::pin(async move { Ok(b"hello\n".to_vec()) })
    });
    operations.write_file = std::sync::Arc::new(|_path, _content| Box::pin(async move { Ok(()) }));
    let tool = create_edit_tool(
        dir.path(),
        EditToolOptions {
            operations: Some(operations),
        },
    );
    let error = utils::tools::execute(
        &tool,
        json!({ "path": "broken.txt", "edits": [{ "oldText": "hello", "newText": "world" }] }),
    )
    .await
    .unwrap_err();
    assert_eq!(
        read_error(error),
        "Could not edit file: broken.txt. Error: disk offline."
    );
}

#[tokio::test]
async fn edit_diff_preview_includes_enoent_for_missing_files() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("missing-preview.txt");
    let result = compute_edits_diff(
        &missing.to_string_lossy(),
        &[Edit {
            old_text: "hello".into(),
            new_text: "world".into(),
        }],
        &dir.path().to_string_lossy(),
    )
    .await;
    assert_eq!(
        result.unwrap_err(),
        format!(
            "Could not edit file: {}. Error code: ENOENT.",
            missing.to_string_lossy()
        )
    );
}

#[cfg(unix)]
#[tokio::test]
async fn edit_diff_preview_includes_eacces_for_unreadable_files() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("unreadable-preview.txt");
    write(&file, "hello\n");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o222)).unwrap();
    let result = compute_edits_diff(
        &file.to_string_lossy(),
        &[Edit {
            old_text: "hello".into(),
            new_text: "world".into(),
        }],
        &dir.path().to_string_lossy(),
    )
    .await;
    assert_eq!(
        result.unwrap_err(),
        format!(
            "Could not edit file: {}. Error code: EACCES.",
            file.to_string_lossy()
        )
    );
}

// ---------------------------------------------------------------------------
// bash
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bash_executes_simple_commands() {
    let dir = TempDir::new().unwrap();
    let tool = create_bash_tool(dir.path(), BashToolOptions::default());
    let result = utils::tools::execute(&tool, json!({ "command": "echo 'test output'" }))
        .await
        .unwrap();
    assert!(read_text(&result).contains("test output"));
    assert!(result.details.is_none());
}

#[tokio::test]
async fn bash_reports_non_zero_exit_codes_as_error_results() {
    let dir = TempDir::new().unwrap();
    let tool = create_bash_tool(dir.path(), BashToolOptions::default());
    let result = utils::tools::execute(&tool, json!({ "command": "echo out; exit 3" }))
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true));
    assert_eq!(read_text(&result), "out\n\n\nCommand exited with code 3");
    let structured = result.structured_content.unwrap();
    assert_eq!(structured["output"], "out\n");
    assert_eq!(structured["truncated"], false);
    assert_eq!(structured["exit_code"], 3);
    assert!(structured["wall_time_seconds"].is_number());

    let ok = utils::tools::execute(&tool, json!({ "command": "echo fine" }))
        .await
        .unwrap();
    assert_eq!(ok.is_error, None);
    assert_eq!(ok.structured_content.unwrap()["exit_code"], 0);

    let empty = utils::tools::execute(&tool, json!({ "command": "true" }))
        .await
        .unwrap();
    assert_eq!(read_text(&empty), "(no output)");
    let structured = empty.structured_content.unwrap();
    assert_eq!(structured["output"], "");
    assert_eq!(structured["truncated"], false);
}

#[tokio::test]
async fn bash_returns_up_to_one_mebibyte_of_output_in_structured_content() {
    let dir = TempDir::new().unwrap();
    let tool = create_bash_tool(dir.path(), BashToolOptions::default());
    let medium = utils::tools::execute(&tool, json!({ "command": "seq 1 3000" }))
        .await
        .unwrap();
    assert!(!read_text(&medium).contains("\n1\n2\n"));
    assert_eq!(details(&medium)["truncation"]["truncated"], true);
    let medium_output = medium.structured_content.unwrap();
    assert_eq!(medium_output["truncated"], false);
    let expected: Vec<String> = (1..=3000).map(|value| value.to_string()).collect();
    assert_eq!(medium_output["output"], format!("{}\n", expected.join("\n")));

    let large = utils::tools::execute(&tool, json!({ "command": "seq 1 300000" }))
        .await
        .unwrap();
    let large_output = large.structured_content.clone().unwrap();
    assert_eq!(large_output["truncated"], true);
    let output = large_output["output"].as_str().unwrap();
    assert!(output.starts_with("1\n2\n3\n"));
    assert!(output.ends_with("299999\n300000\n"));
    assert!(output.contains("[... "));
    assert!(output.contains(" bytes omitted ...]"));
    assert!(output.len() < 1024 * 1024 + 100);
    assert_eq!(
        large_output["full_output_path"],
        details(&large)["fullOutputPath"]
    );
    let path = large_output["full_output_path"].as_str().unwrap();
    assert!(std::fs::read_to_string(path).unwrap().ends_with("300000\n"));
}

#[cfg(unix)]
#[tokio::test]
async fn bash_maps_signal_killed_commands_to_128_plus_the_signal_number() {
    let dir = TempDir::new().unwrap();
    let operations = create_local_bash_operations(None);
    for (signal, exit_code) in [("KILL", 137), ("TERM", 143)] {
        let result = (operations.exec)(
            format!("kill -{signal} $$"),
            dir.path().to_path_buf(),
            xjpcode_coding_agent::core::tools::bash::BashExecOptions {
                on_data: std::sync::Arc::new(|_| {}),
                signal: None,
                timeout: None,
                env: Default::default(),
            },
        )
        .await
        .unwrap();
        assert_eq!(result.exit_code, Some(exit_code));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn bash_reports_signal_killed_commands_as_errors_with_partial_output() {
    let dir = TempDir::new().unwrap();
    let tool = create_bash_tool(dir.path(), BashToolOptions::default());
    for (signal, exit_code) in [("KILL", 137), ("TERM", 143)] {
        let result = utils::tools::execute(
            &tool,
            json!({ "command": format!("printf 'before-kill\\n'; kill -{signal} $$") }),
        )
        .await
        .unwrap();
        assert_eq!(result.is_error, Some(true));
        let text = read_text(&result);
        assert!(text.contains("before-kill"));
        assert!(text.ends_with(&format!("Command exited with code {exit_code}")));
    }
}

#[tokio::test]
async fn bash_rejects_a_null_exit_code_from_custom_operations() {
    let dir = TempDir::new().unwrap();
    let operations = BashOperations {
        exec: std::sync::Arc::new(|_command, _cwd, options| {
            Box::pin(async move {
                (options.on_data)(b"partial\n".to_vec());
                Ok(xjpcode_coding_agent::core::tools::bash::BashExecResult { exit_code: None })
            })
        }),
    };
    let tool = create_bash_tool(
        dir.path(),
        BashToolOptions {
            operations: Some(operations),
            ..BashToolOptions::default()
        },
    );
    let error = utils::tools::execute(&tool, json!({ "command": "remote" }))
        .await
        .unwrap_err();
    assert!(read_error(error).ends_with("partial\n\n\nCommand terminated without an exit code"));
}

#[tokio::test]
async fn bash_respects_timeout() {
    let dir = TempDir::new().unwrap();
    let tool = create_bash_tool(dir.path(), BashToolOptions::default());
    let error = utils::tools::execute(
        &tool,
        json!({ "command": "sleep 5", "timeout": 0.05 }),
    )
    .await
    .unwrap_err();
    assert!(read_error(error).to_lowercase().contains("timed out"));
}

#[tokio::test]
async fn bash_includes_full_output_path_for_truncated_timeout_and_abort_errors() {
    let dir = TempDir::new().unwrap();
    for (failure, expected) in [
        ("timeout:5", "Command timed out after 5 seconds"),
        ("aborted", "Command aborted"),
    ] {
        let operations = BashOperations {
            exec: std::sync::Arc::new(move |_command, _cwd, options| {
                let failure = failure.to_string();
                Box::pin(async move {
                    for index in 1..=3000 {
                        (options.on_data)(format!("{index}\n").into_bytes());
                    }
                    Err(xjpcode_agent::ToolError::from(failure))
                })
            }),
        };
        let tool = create_bash_tool(
            dir.path(),
            BashToolOptions {
                operations: Some(operations),
                ..BashToolOptions::default()
            },
        );
        let error = utils::tools::execute(&tool, json!({ "command": "chatty-fail" }))
            .await
            .unwrap_err();
        let message = read_error(error);
        assert!(message.contains(expected));
        assert!(message.contains("Full output: "));
        assert!(!message.contains("Full output: undefined"));
        let path = message
            .split("Full output: ")
            .nth(1)
            .unwrap()
            .split(']')
            .next()
            .unwrap()
            .trim();
        let full_output = std::fs::read_to_string(path).unwrap();
        assert!(full_output.contains("1\n2\n3"));
        assert!(full_output.contains("2998\n2999\n3000"));
    }
}

#[tokio::test]
async fn bash_throws_when_cwd_does_not_exist() {
    let tool = create_bash_tool(
        "/this/directory/definitely/does/not/exist/12345",
        BashToolOptions::default(),
    );
    let error = utils::tools::execute(&tool, json!({ "command": "echo test" }))
        .await
        .unwrap_err();
    assert!(read_error(error).contains("Working directory does not exist"));
}

#[tokio::test]
async fn bash_reports_a_missing_custom_shell() {
    // 上游 mock 掉 shell 解析，让 spawn 报 ENOENT；这里用显式的 shellPath，报的是上游同一条
    // `Custom shell path not found`（见 docs/port/milestone-3.md）。
    let dir = TempDir::new().unwrap();
    let tool = create_bash_tool(
        dir.path(),
        BashToolOptions {
            shell_path: Some("/nonexistent-shell-path-xyz123".to_string()),
            ..BashToolOptions::default()
        },
    );
    let error = utils::tools::execute(&tool, json!({ "command": "echo test" }))
        .await
        .unwrap_err();
    assert!(read_error(error).contains("Custom shell path not found"));
}

#[tokio::test]
async fn bash_passes_shell_path_through_to_shell_resolution() {
    let dir = TempDir::new().unwrap();
    let tool = create_bash_tool(
        dir.path(),
        BashToolOptions {
            shell_path: Some("/custom/bash".to_string()),
            operations: Some(BashOperations {
                exec: std::sync::Arc::new(|_command, _cwd, _options| {
                    Box::pin(async move {
                        Ok(xjpcode_coding_agent::core::tools::bash::BashExecResult {
                            exit_code: Some(0),
                        })
                    })
                }),
            }),
            ..BashToolOptions::default()
        },
    );
    utils::tools::execute(&tool, json!({ "command": "echo test" }))
        .await
        .unwrap();

    let operations = create_local_bash_operations(Some("/custom/bash".to_string()));
    let error = (operations.exec)(
        "echo test".to_string(),
        dir.path().to_path_buf(),
        xjpcode_coding_agent::core::tools::bash::BashExecOptions {
            on_data: std::sync::Arc::new(|_| {}),
            signal: None,
            timeout: None,
            env: Default::default(),
        },
    )
    .await
    .unwrap_err();
    assert!(read_error(error).contains("Custom shell path not found: /custom/bash"));
}

#[tokio::test]
async fn bash_prepends_command_prefix_when_configured() {
    let dir = TempDir::new().unwrap();
    let tool = create_bash_tool(
        dir.path(),
        BashToolOptions {
            command_prefix: Some("export TEST_VAR=hello".to_string()),
            ..BashToolOptions::default()
        },
    );
    let result = utils::tools::execute(&tool, json!({ "command": "echo $TEST_VAR" }))
        .await
        .unwrap();
    assert_eq!(read_text(&result).trim(), "hello");

    let tool = create_bash_tool(
        dir.path(),
        BashToolOptions {
            command_prefix: Some("echo prefix-output".to_string()),
            ..BashToolOptions::default()
        },
    );
    let result = utils::tools::execute(&tool, json!({ "command": "echo command-output" }))
        .await
        .unwrap();
    assert_eq!(read_text(&result).trim(), "prefix-output\ncommand-output");

    let tool = create_bash_tool(dir.path(), BashToolOptions::default());
    let result = utils::tools::execute(&tool, json!({ "command": "echo no-prefix" }))
        .await
        .unwrap();
    assert_eq!(read_text(&result).trim(), "no-prefix");
}

#[tokio::test]
async fn bash_coalesces_streaming_updates_for_chatty_output() {
    let dir = TempDir::new().unwrap();
    let operations = BashOperations {
        exec: std::sync::Arc::new(|_command, _cwd, options| {
            Box::pin(async move {
                for index in 0..5000 {
                    (options.on_data)(format!("line {index}\n").into_bytes());
                }
                Ok(xjpcode_coding_agent::core::tools::bash::BashExecResult {
                    exit_code: Some(0),
                })
            })
        }),
    };
    let tool = create_bash_tool(
        dir.path(),
        BashToolOptions {
            operations: Some(operations),
            ..BashToolOptions::default()
        },
    );
    let updates = std::sync::Arc::new(std::sync::Mutex::new(0usize));
    let updates_clone = std::sync::Arc::clone(&updates);
    let callback: xjpcode_agent::AgentToolUpdateCallback =
        std::sync::Arc::new(move |_update| {
            let updates_clone = std::sync::Arc::clone(&updates_clone);
            Box::pin(async move {
                *updates_clone.lock().unwrap() += 1;
            })
        });
    let result = tool
        .execute(
            "test-call-chatty-updates",
            &utils::tools::object(json!({ "command": "chatty" })),
            None,
            Some(&callback),
        )
        .await
        .unwrap();
    assert!(*updates.lock().unwrap() < 25);
    assert!(read_text(&result).contains("line 4999"));
}

#[tokio::test]
async fn bash_does_not_count_a_trailing_newline_as_an_extra_line() {
    let dir = TempDir::new().unwrap();
    let operations = BashOperations {
        exec: std::sync::Arc::new(|_command, _cwd, options| {
            Box::pin(async move {
                for index in 1..=4000 {
                    (options.on_data)(format!("line-{index:04}\n").into_bytes());
                }
                Ok(xjpcode_coding_agent::core::tools::bash::BashExecResult {
                    exit_code: Some(0),
                })
            })
        }),
    };
    let tool = create_bash_tool(
        dir.path(),
        BashToolOptions {
            operations: Some(operations),
            ..BashToolOptions::default()
        },
    );
    let result = utils::tools::execute(&tool, json!({ "command": "many-lines" }))
        .await
        .unwrap();
    let output = read_text(&result);
    let truncation = &details(&result)["truncation"];
    assert_eq!(truncation["totalLines"], 4000);
    assert_eq!(truncation["outputLines"], 2000);
    assert!(output.contains("line-2001"));
    assert!(output.contains("line-4000"));
    assert!(output.contains("[Showing lines 2001-4000 of 4000. Full output: "));
    assert!(!output.contains("4001"));
}

#[tokio::test]
async fn bash_shows_the_end_of_an_over_long_last_line_after_a_short_line() {
    let dir = TempDir::new().unwrap();
    let long_line = format!("{}END", "y".repeat(219_000));
    let operations = BashOperations {
        exec: std::sync::Arc::new(move |_command, _cwd, options| {
            let long_line = long_line.clone();
            Box::pin(async move {
                let data = format!("first line\n{long_line}\n");
                for chunk in data.as_bytes().chunks(8192) {
                    (options.on_data)(chunk.to_vec());
                }
                Ok(xjpcode_coding_agent::core::tools::bash::BashExecResult {
                    exit_code: Some(0),
                })
            })
        }),
    };
    let tool = create_bash_tool(
        dir.path(),
        BashToolOptions {
            operations: Some(operations),
            ..BashToolOptions::default()
        },
    );
    let result = utils::tools::execute(&tool, json!({ "command": "grep -n x bundle.min.js" }))
        .await
        .unwrap();
    let output = read_text(&result);
    assert_eq!(details(&result)["truncation"]["lastLinePartial"], true);
    assert!(output.contains("yyyyEND"));
    assert!(output.contains("[Showing last 50.0KB of line 2 "));
}

#[tokio::test]
async fn bash_shows_the_end_of_an_over_long_line_followed_by_a_blank_line() {
    for (length, eol) in [(60_000usize, "\n"), (219_000, "\n"), (219_000, "\r\n")] {
        let dir = TempDir::new().unwrap();
        let long_line = format!("{}END", "y".repeat(length));
        let operations = BashOperations {
            exec: std::sync::Arc::new(move |_command, _cwd, options| {
                let data = format!("first line{eol}{long_line}{eol}{eol}");
                Box::pin(async move {
                    for chunk in data.as_bytes().chunks(8192) {
                        (options.on_data)(chunk.to_vec());
                    }
                    Ok(xjpcode_coding_agent::core::tools::bash::BashExecResult {
                        exit_code: Some(0),
                    })
                })
            }),
        };
        let tool = create_bash_tool(
            dir.path(),
            BashToolOptions {
                operations: Some(operations),
                ..BashToolOptions::default()
            },
        );
        let result = utils::tools::execute(&tool, json!({ "command": "long-line" }))
            .await
            .unwrap();
        let output = read_text(&result);
        assert_eq!(details(&result)["truncation"]["lastLinePartial"], true);
        assert!(output.starts_with('y'));
        assert!(output.contains("END"));
        assert!(output.contains("[Showing last 50.0KB of line 2 "));
    }
}

#[tokio::test]
async fn bash_decodes_utf8_characters_split_across_output_chunks() {
    let dir = TempDir::new().unwrap();
    let operations = BashOperations {
        exec: std::sync::Arc::new(|_command, _cwd, options| {
            Box::pin(async move {
                let euro = "€\n".as_bytes().to_vec();
                (options.on_data)(euro[..1].to_vec());
                (options.on_data)(euro[1..].to_vec());
                Ok(xjpcode_coding_agent::core::tools::bash::BashExecResult {
                    exit_code: Some(0),
                })
            })
        }),
    };
    let tool = create_bash_tool(
        dir.path(),
        BashToolOptions {
            operations: Some(operations),
            ..BashToolOptions::default()
        },
    );
    let result = utils::tools::execute(&tool, json!({ "command": "split-utf8" }))
        .await
        .unwrap();
    assert_eq!(read_text(&result).trim(), "€");
}

#[tokio::test]
async fn bash_exposes_local_bash_operations_with_the_given_env() {
    let dir = TempDir::new().unwrap();
    let operations = create_local_bash_operations(None);
    let mut env: std::collections::BTreeMap<String, String> =
        std::env::vars().collect();
    env.insert("TEST_LOCAL_BASH_OPS".to_string(), "from-local-ops".to_string());
    let chunks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let chunks_clone = std::sync::Arc::clone(&chunks);
    let result = (operations.exec)(
        "echo $TEST_LOCAL_BASH_OPS".to_string(),
        dir.path().to_path_buf(),
        xjpcode_coding_agent::core::tools::bash::BashExecOptions {
            on_data: std::sync::Arc::new(move |data| {
                chunks_clone.lock().unwrap().extend_from_slice(&data);
            }),
            signal: None,
            timeout: None,
            env,
        },
    )
    .await
    .unwrap();
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(
        String::from_utf8_lossy(&chunks.lock().unwrap()).trim(),
        "from-local-ops"
    );
}

#[tokio::test]
async fn bash_persists_full_output_when_truncation_happens_by_line_count_only() {
    let dir = TempDir::new().unwrap();
    let tool = create_bash_tool(dir.path(), BashToolOptions::default());
    let result = utils::tools::execute(&tool, json!({ "command": "seq 3000" }))
        .await
        .unwrap();
    let truncation = &details(&result)["truncation"];
    assert_eq!(truncation["truncated"], true);
    assert_eq!(truncation["truncatedBy"], "lines");
    let full_output_path = details(&result)["fullOutputPath"].as_str().unwrap();
    let full_output = std::fs::read_to_string(full_output_path).unwrap();
    assert!(full_output.contains("1\n2\n3"));
    assert!(full_output.contains("2998\n2999\n3000"));
}

// ---------------------------------------------------------------------------
// 工具的工作目录
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_write_and_edit_use_the_tools_cwd() {
    let dir = TempDir::new().unwrap();
    write(&dir.path().join("a.txt"), "hello\n");

    let read = create_read_tool(dir.path(), ReadToolOptions::default());
    assert_eq!(
        read_text(&utils::tools::execute(&read, json!({ "path": "a.txt" })).await.unwrap()),
        "hello\n"
    );

    let write_tool = create_write_tool(dir.path(), WriteToolOptions::default());
    utils::tools::execute(&write_tool, json!({ "path": "b.txt", "content": "written\n" }))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
        "written\n"
    );

    let edit = create_edit_tool(dir.path(), EditToolOptions::default());
    utils::tools::execute(
        &edit,
        json!({ "path": "a.txt", "edits": [{ "oldText": "hello", "newText": "changed" }] }),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
        "changed\n"
    );
}

#[tokio::test]
async fn bash_uses_the_tools_cwd() {
    let dir = TempDir::new().unwrap();
    let tool = create_bash_tool(dir.path(), BashToolOptions::default());
    let result = utils::tools::execute(&tool, json!({ "command": "pwd" }))
        .await
        .unwrap();
    assert_eq!(
        std::fs::canonicalize(read_text(&result).trim()).unwrap(),
        std::fs::canonicalize(dir.path()).unwrap()
    );
}

// ---------------------------------------------------------------------------
// edit 的模糊匹配
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fuzzy_matches_trailing_whitespace_and_unicode_variants() {
    let cases: Vec<(&str, &str, &str, &str)> = vec![
        (
            "trailing-ws.txt",
            "line one   \nline two  \nline three\n",
            "line one\nline two\n",
            "replaced\n",
        ),
        (
            "chinese-punctuation.txt",
            "你好，世界\n你好（世界）\n",
            "你好,世界\n你好(世界)\n",
            "你好，pi\n你好(pi)\n",
        ),
        (
            "unicode-compatibility.txt",
            "ＡＢＣ１２３\ncafe\u{0301}\n",
            "ABC123\ncafé\n",
            "XYZ789\ncoffee\n",
        ),
        (
            "smart-quotes.txt",
            "console.log(\u{2018}hello\u{2019});\n",
            "console.log('hello');",
            "console.log('world');",
        ),
        (
            "smart-double-quotes.txt",
            "const msg = \u{201C}Hello World\u{201D};\n",
            "const msg = \"Hello World\";",
            "const msg = \"Goodbye\";",
        ),
        (
            "unicode-dashes.txt",
            "range: 1\u{2013}5\nbreak\u{2014}here\n",
            "range: 1-5\nbreak-here",
            "range: 10-50\nbreak--here",
        ),
        (
            "nbsp.txt",
            "hello\u{00A0}world\n",
            "hello world",
            "hello universe",
        ),
    ];
    for (name, original, old_text, new_text) in cases {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join(name);
        write(&file, original);
        let tool = create_edit_tool(dir.path(), EditToolOptions::default());
        let result = utils::tools::execute(
            &tool,
            json!({ "path": file, "edits": [{ "oldText": old_text, "newText": new_text }] }),
        )
        .await
        .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(read_text(&result).contains("Successfully replaced"), "{name}");
        let content = std::fs::read_to_string(&file).unwrap();
        assert!(content.contains(new_text.lines().next().unwrap()), "{name}: {content}");
    }
}

#[tokio::test]
async fn fuzzy_prefers_exact_match_over_fuzzy_match() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("exact-preferred.txt");
    write(&file, "const x = 'exact';\nconst y = 'other';\n");
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    utils::tools::execute(
        &tool,
        json!({ "path": file, "edits": [{ "oldText": "const x = 'exact';", "newText": "const x = 'changed';" }] }),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "const x = 'changed';\nconst y = 'other';\n"
    );
}

#[tokio::test]
async fn fuzzy_still_fails_when_the_text_is_not_found() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("no-match.txt");
    write(&file, "completely different content\n");
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    let error = utils::tools::execute(
        &tool,
        json!({ "path": file, "edits": [{ "oldText": "this does not exist", "newText": "replacement" }] }),
    )
    .await
    .unwrap_err();
    assert!(read_error(error).contains("Could not find the exact text"));
}

#[tokio::test]
async fn fuzzy_detects_duplicates_after_normalization() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("fuzzy-dups.txt");
    write(&file, "hello world   \nhello world\n");
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    let error = utils::tools::execute(
        &tool,
        json!({ "path": file, "edits": [{ "oldText": "hello world", "newText": "replaced" }] }),
    )
    .await
    .unwrap_err();
    assert!(read_error(error).contains("Found 2 occurrences"));
}

#[tokio::test]
async fn fuzzy_supports_multi_edit_mode() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("fuzzy-multi.txt");
    write(&file, "console.log(\u{2018}hello\u{2019});\nhello\u{00A0}world\n");
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    utils::tools::execute(
        &tool,
        json!({
            "path": file,
            "edits": [
                { "oldText": "console.log('hello');\n", "newText": "console.log('world');\n" },
                { "oldText": "hello world\n", "newText": "hello universe\n" }
            ]
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "console.log('world');\nhello universe\n"
    );
}

#[tokio::test]
async fn fuzzy_preserves_the_correct_occurrence_and_produces_an_applicable_patch() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("fuzzy-preserve-duplicate-line.txt");
    let original = ["replace me   ", "after   ", ""].join("\n");
    write(&file, &original);
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    let result = utils::tools::execute(
        &tool,
        json!({ "path": file, "edits": [{ "oldText": "replace me\n", "newText": "after\n" }] }),
    )
    .await
    .unwrap();
    let expected = ["after", "after   ", ""].join("\n");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), expected);
    assert_eq!(
        utils::patch::apply_patch(&original, details(&result)["patch"].as_str().unwrap()).unwrap(),
        expected
    );
}

#[tokio::test]
async fn fuzzy_preserves_untouched_lines_and_produces_an_applicable_patch_for_multi_edits() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("fuzzy-preserve-multi.txt");
    let original = [
        "keep before  ",
        "first target  ",
        "first after",
        "keep middle   ",
        "second target  ",
        "second after",
        "keep after  ",
        "",
    ]
    .join("\n");
    write(&file, &original);
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    let result = utils::tools::execute(
        &tool,
        json!({
            "path": file,
            "edits": [
                { "oldText": "first target\nfirst after", "newText": "FIRST\nFIRST2" },
                { "oldText": "second target\nsecond after", "newText": "SECOND\nSECOND2" }
            ]
        }),
    )
    .await
    .unwrap();
    let expected = [
        "keep before  ",
        "FIRST",
        "FIRST2",
        "keep middle   ",
        "SECOND",
        "SECOND2",
        "keep after  ",
        "",
    ]
    .join("\n");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), expected);
    assert_eq!(
        utils::patch::apply_patch(&original, details(&result)["patch"].as_str().unwrap()).unwrap(),
        expected
    );
}

// ---------------------------------------------------------------------------
// edit 的行尾与 BOM
// ---------------------------------------------------------------------------

async fn edit_bytes(original: &[u8], old_text: &str, new_text: &str) -> Vec<u8> {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("file.txt");
    write_bytes(&file, original);
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    utils::tools::execute(
        &tool,
        json!({ "path": file, "edits": [{ "oldText": old_text, "newText": new_text }] }),
    )
    .await
    .unwrap();
    std::fs::read(&file).unwrap()
}

#[tokio::test]
async fn edit_matches_lf_old_text_against_crlf_content() {
    let content = edit_bytes(b"line one\r\nline two\r\nline three\r\n", "line two\n", "replaced line\n").await;
    assert_eq!(content, b"line one\r\nreplaced line\r\nline three\r\n");
}

#[tokio::test]
async fn edit_preserves_crlf_and_lf_line_endings_and_bom() {
    assert_eq!(
        edit_bytes(b"first\r\nsecond\r\nthird\r\n", "second\n", "REPLACED\n").await,
        b"first\r\nREPLACED\r\nthird\r\n"
    );
    assert_eq!(
        edit_bytes(b"first\nsecond\nthird\n", "second\n", "REPLACED\n").await,
        b"first\nREPLACED\nthird\n"
    );
    assert_eq!(
        edit_bytes("\u{FEFF}first\r\nsecond\r\nthird\r\n".as_bytes(), "second\n", "REPLACED\n").await,
        "\u{FEFF}first\r\nREPLACED\r\nthird\r\n".as_bytes()
    );
}

#[tokio::test]
async fn edit_detects_duplicates_across_crlf_and_lf_variants() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("mixed-endings.txt");
    write(&file, "hello\r\nworld\r\n---\r\nhello\nworld\n");
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    let error = utils::tools::execute(
        &tool,
        json!({ "path": file, "edits": [{ "oldText": "hello\nworld\n", "newText": "replaced\n" }] }),
    )
    .await
    .unwrap_err();
    assert!(read_error(error).contains("Found 2 occurrences"));
}

#[tokio::test]
async fn edit_preserves_crlf_and_bom_in_multi_edit_mode() {
    assert_eq!(
        edit_bytes(
            "\u{FEFF}first\r\nsecond\r\nthird\r\nfourth\r\n".as_bytes(),
            "second\n",
            "SECOND\n"
        )
        .await,
        "\u{FEFF}first\r\nSECOND\r\nthird\r\nfourth\r\n".as_bytes()
    );
}

#[tokio::test]
async fn edit_writes_the_files_line_ending_for_inserted_lines() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("crlf-bytes.txt");
    write_bytes(&file, b"first\r\nsecond\r\nthird");
    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    utils::tools::execute(
        &tool,
        json!({ "path": file, "edits": [{ "oldText": "second\nthird", "newText": "2a\r\n2b\nthird!" }] }),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"first\r\n2a\r\n2b\r\nthird!");

    let file = dir.path().join("lf-bytes.txt");
    write_bytes(&file, b"first\nsecond\nthird");
    utils::tools::execute(
        &tool,
        json!({ "path": file, "edits": [{ "oldText": "second\nthird", "newText": "2a\r\n2b\nthird!" }] }),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"first\n2a\n2b\nthird!");
}

#[tokio::test]
async fn edit_keeps_mixed_line_endings_and_lone_crs() {
    // #8544：第一行以 LF 结尾时，后面的 CRLF 行保持不变。
    assert_eq!(
        edit_bytes(b"a\nb\r\nX = 1\r\n", "X = 1", "X = 2").await,
        b"a\nb\r\nX = 2\r\n"
    );
    // 第一行以 CRLF 结尾时，后面的 LF 行保持不变。
    assert_eq!(edit_bytes(b"a\r\nb\nc", "c", "C").await, b"a\r\nb\nC");
    // 未改动行里的孤 CR 保留。
    assert_eq!(edit_bytes(b"a\rb\nX = 1\n", "X = 1", "X = 2").await, b"a\rb\nX = 2\n");
    // 多改：插入行跟随被替换那一行的行尾。
    assert_eq!(
        edit_bytes(b"lf1\nlf2\ncr1\r\ncr2\r\n", "lf1\n", "A\nB\n").await,
        b"A\nB\nlf2\ncr1\r\ncr2\r\n"
    );
}

#[tokio::test]
async fn edit_keeps_untouched_line_endings_when_fuzzy_matching_widens_to_whole_lines() {
    assert_eq!(
        edit_bytes(b"a\r\nfoo   \r\nb\n", "foo\n", "FOO\n").await,
        b"a\r\nFOO\r\nb\n"
    );
}

#[tokio::test]
async fn edit_does_not_merge_a_kept_lone_cr_with_a_line_feed_after_it() {
    let dir = TempDir::new().unwrap();
    for (index, (edit, written, diffed)) in [
        (("b", "\nB"), "a\r\n\nB\nc", "a\n\nB\nc"),
        (("b", ""), "a\r\n\nc", "a\n\nc"),
    ]
    .into_iter()
    .enumerate()
    {
        let file = dir.path().join(format!("cr-seam-{index}.txt"));
        write_bytes(&file, b"a\rb\nc");
        let tool = create_edit_tool(dir.path(), EditToolOptions::default());
        let result = utils::tools::execute(
            &tool,
            json!({ "path": file, "edits": [{ "oldText": edit.0, "newText": edit.1 }] }),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), written.as_bytes());
        assert_eq!(
            utils::patch::apply_patch("a\nb\nc", details(&result)["patch"].as_str().unwrap()).unwrap(),
            diffed
        );
    }
}

#[test]
fn edit_does_not_merge_an_inserted_cr_line_ending_with_a_kept_line_feed() {
    let written = apply_replacements_to_original(
        "a\rb\nc",
        &[xjpcode_coding_agent::core::tools::edit_diff::TextReplacement {
            match_index: 2,
            match_length: 1,
            new_text: "\r".to_string(),
        }],
    );
    assert_eq!(normalize_to_lf(&written), "a\n\n\nc");
}

#[test]
fn edit_writes_content_that_normalizes_to_the_diffed_content_for_random_line_endings() {
    let mut seed: u64 = 1;
    let mut random = |n: usize| -> usize {
        seed = (seed.wrapping_mul(1_103_515_245).wrapping_add(12_345)) & 0x7fff_ffff;
        ((seed as f64 / 0x8000_0000u64 as f64) * n as f64) as usize
    };
    let endings = ["\n", "\r\n", "\r", ""];
    for _ in 0..3000 {
        let mut original = String::new();
        for _ in 0..=(random(12)) {
            original.push_str(&format!(
                "w{}{}{}",
                random(100),
                if random(5) == 0 { "  " } else { "" },
                endings[random(4)]
            ));
        }
        let normalized = normalize_to_lf(&original);
        if normalized.is_empty() {
            continue;
        }
        let mut start = random(normalized.len());
        while !normalized.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = (start + 1 + random(12)).min(normalized.len());
        while !normalized.is_char_boundary(end) {
            end -= 1;
        }
        let old_text = normalized[start..end].to_string();
        let new_text = ["", "\n", "X\n", "\nY", "\r", "Z"][random(6)].to_string();
        let applied = match apply_edits_to_normalized_content(
            &normalized,
            &[Edit {
                old_text,
                new_text,
            }],
            "f.txt",
        ) {
            Ok(applied) => applied,
            Err(_) => continue,
        };
        assert_eq!(
            normalize_to_lf(&apply_replacements_to_original(
                &original,
                &applied.replacements
            )),
            applied.new_content
        );
    }
}
