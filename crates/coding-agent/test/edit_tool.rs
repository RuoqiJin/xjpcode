//! 对应上游 `test/edit-tool-legacy-input.test.ts`：旧写法（顶层 oldText/newText、字符串 edits）在
//! 参数校验前折成 edits 数组。
//!
//! 上游还测「非对象输入原样返回」；Rust 侧内核只把对象交给工具（`prepare_arguments` 的入参就是
//! `JsonObject`），没有对应的输入，见 `docs/port/milestone-3.md`。

use serde_json::{json, Value};
use tempfile::TempDir;
use xjpcode_agent::AgentTool;
use xjpcode_coding_agent::core::tools::edit::create_edit_tool;
use xjpcode_coding_agent::EditToolOptions;

#[path = "utils/mod.rs"]
mod utils;

#[test]
fn keeps_legacy_fields_out_of_the_public_schema() {
    let tool = create_edit_tool(".", EditToolOptions::default());
    let declaration = tool.declaration();
    let properties = declaration.parameters["properties"].as_object().unwrap();
    assert!(!properties.contains_key("oldText"));
    assert!(!properties.contains_key("newText"));
}

#[test]
fn folds_top_level_old_text_and_new_text_into_edits() {
    let tool = create_edit_tool(".", EditToolOptions::default());
    let prepared = tool
        .prepare_arguments(&utils::tools::object(json!({
            "path": "file.txt",
            "oldText": "before",
            "newText": "after"
        })))
        .unwrap();
    assert_eq!(
        Value::Object(prepared),
        json!({
            "path": "file.txt",
            "edits": [{ "oldText": "before", "newText": "after" }]
        })
    );
}

#[test]
fn appends_legacy_replacement_to_existing_edits() {
    let tool = create_edit_tool(".", EditToolOptions::default());
    let prepared = tool
        .prepare_arguments(&utils::tools::object(json!({
            "path": "file.txt",
            "edits": [{ "oldText": "a", "newText": "b" }],
            "oldText": "c",
            "newText": "d"
        })))
        .unwrap();
    assert_eq!(
        Value::Object(prepared),
        json!({
            "path": "file.txt",
            "edits": [
                { "oldText": "a", "newText": "b" },
                { "oldText": "c", "newText": "d" }
            ]
        })
    );
}

#[test]
fn passes_through_valid_input_unchanged() {
    let tool = create_edit_tool(".", EditToolOptions::default());
    let input = utils::tools::object(json!({
        "path": "file.txt",
        "edits": [{ "oldText": "a", "newText": "b" }]
    }));
    let prepared = tool.prepare_arguments(&input).unwrap();
    assert_eq!(prepared, input);
}

#[tokio::test]
async fn prepared_args_execute_correctly() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("legacy.txt");
    tokio::fs::write(&file_path, "before\n").await.unwrap();

    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    let prepared = tool
        .prepare_arguments(&utils::tools::object(json!({
            "path": "legacy.txt",
            "oldText": "before",
            "newText": "after"
        })))
        .unwrap();
    let result = tool.execute("tool-1", &prepared, None, None).await.unwrap();
    assert_eq!(
        utils::tools::text(&result),
        "Successfully replaced 1 block(s) in legacy.txt."
    );
    assert_eq!(
        tokio::fs::read_to_string(&file_path).await.unwrap(),
        "after\n"
    );
}

#[test]
fn parses_edits_from_a_json_string() {
    let tool = create_edit_tool(".", EditToolOptions::default());
    let prepared = tool
        .prepare_arguments(&utils::tools::object(json!({
            "path": "file.txt",
            "edits": "{\"oldText\":\"a\",\"newText\":\"b\"}"
        })))
        .unwrap();
    assert_eq!(
        Value::Object(prepared),
        json!({
            "path": "file.txt",
            "edits": [{ "oldText": "a", "newText": "b" }]
        })
    );
}

#[test]
fn leaves_edits_alone_when_the_string_is_not_valid_json() {
    let tool = create_edit_tool(".", EditToolOptions::default());
    let prepared = tool
        .prepare_arguments(&utils::tools::object(json!({
            "path": "file.txt",
            "edits": "not json"
        })))
        .unwrap();
    assert_eq!(
        Value::Object(prepared),
        json!({ "path": "file.txt", "edits": "not json" })
    );
}

/// 回归：要换掉的那段话以中文/全角标点/表情结尾，且后面还有内容时，diff 窗口的字节下标
/// 曾经落在字的中间而崩（里程碑三验收记下的 `edit_diff.rs:879`）。
#[tokio::test]
async fn edits_that_end_with_multibyte_characters_before_more_lines() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("doc.md");
    tokio::fs::write(&file_path, "# 标题\n\n旧的一句话。\n\n后面还有一段。\n")
        .await
        .unwrap();

    let tool = create_edit_tool(dir.path(), EditToolOptions::default());
    let prepared = utils::tools::object(json!({
        "path": "doc.md",
        "edits": [{ "oldText": "旧的一句话。", "newText": "新的一句话。" }]
    }));
    let result = tool.execute("tool-1", &prepared, None, None).await.unwrap();
    assert_eq!(
        utils::tools::text(&result),
        "Successfully replaced 1 block(s) in doc.md."
    );
    assert_eq!(
        tokio::fs::read_to_string(&file_path).await.unwrap(),
        "# 标题\n\n新的一句话。\n\n后面还有一段。\n"
    );
}
