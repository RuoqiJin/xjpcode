//! 移植自上游 `packages/ai/test/json-parse.test.ts`。
//!
//! 上游这个文件有两个 describe：`parseStreamingJson` 与 `parseToolCallArguments`。前者本次不搬
//! （`parseStreamingJson()` 在 pi 里没有调用方，连同它用的 `partial-json`，见 `PORTING.md` 差异表
//! #103）；它守的那套“没写完的文字怎么读”由 `streaming_json.rs` 的测试接着守。
//!
//! 与上游的文字有一处不同：`JSON.parse` 的报错原文换成 `serde_json` 的话（差异表 #106）。用例只断言
//! 上游也断言的那部分——原文附着在后面、整段以模型发来的文字结尾。

use serde_json::{json, Map, Value};
use xjpcode_ai::utils::json_parse::{
    parse_tool_call_arguments, repair_json, ParsedToolCallArguments,
};

fn object(value: Value) -> Map<String, Value> {
    value.as_object().expect("an object").clone()
}

#[test]
fn parses_complete_json_and_repairs_invalid_escapes() {
    assert_eq!(
        parse_tool_call_arguments(r#"{"kind":"seek","note":"\url"}"#),
        ParsedToolCallArguments {
            arguments: object(json!({ "kind": "seek", "note": "\\url" })),
            arguments_error: None,
        }
    );
}

#[test]
fn uses_empty_arguments_for_empty_text() {
    assert_eq!(
        parse_tool_call_arguments(""),
        ParsedToolCallArguments::default()
    );
    // 上游的参数还可以是 `undefined`；空文字就是那个情形。
    assert_eq!(
        parse_tool_call_arguments("  \n\t "),
        ParsedToolCallArguments::default()
    );
}

#[test]
fn does_not_keep_the_valid_prefix_of_malformed_json() {
    let raw = r#"{"action":"apply","actions":[{"kind":"seek","time":.5}]}"#;
    let parsed = parse_tool_call_arguments(raw);
    assert_eq!(parsed.arguments, Map::new());
    let error = parsed.arguments_error.expect("an error");
    assert!(error.contains("\n\nReceived arguments:\n"), "{error}");
    assert!(error.ends_with(raw), "{error}");
}

#[test]
fn does_not_complete_cut_off_json() {
    let raw = r#"{"kind":"seek","clipId":"clip-fl"#;
    let parsed = parse_tool_call_arguments(raw);
    assert_eq!(parsed.arguments, Map::new());
    assert!(parsed
        .arguments_error
        .as_deref()
        .expect("an error")
        .ends_with(raw));
}

#[test]
fn rejects_json_that_is_not_an_object() {
    assert_eq!(
        parse_tool_call_arguments("[1,2]"),
        ParsedToolCallArguments {
            arguments: Map::new(),
            arguments_error: Some(
                "Tool arguments must be a JSON object\n\nReceived arguments:\n[1,2]".to_string()
            ),
        }
    );
}

#[test]
fn repairs_control_characters_and_invalid_escapes() {
    // 上游 `repairJson` 的两条规矩：字符串里的原始控制字符转义，非法转义的反斜杠加倍。
    assert_eq!(repair_json("{}"), "{}");
    assert_eq!(repair_json("{\"a\":\"x\ny\"}"), "{\"a\":\"x\\ny\"}");
    assert_eq!(repair_json("{\"a\":\"\\d\"}"), "{\"a\":\"\\\\d\"}");
    assert_eq!(
        repair_json("{\"a\":\"caf\\u00e9\"}"),
        "{\"a\":\"caf\\u00e9\"}"
    );
    assert_eq!(repair_json("{\"a\":\"\\u12\"}"), "{\"a\":\"\\\\u12\"}");
    assert_eq!(repair_json("{\"a\":\"x\\"), "{\"a\":\"x\\\\");
    // 一份完整的文字：修一遍再读，值不变。
    assert_eq!(
        parse_tool_call_arguments(r#"{"text":"caf\u00e9 \url"}"#),
        ParsedToolCallArguments {
            arguments: object(json!({ "text": "café \\url" })),
            arguments_error: None,
        }
    );
    // 读不了的文字：修过之后仍读不了，报的是修之后那份的错。
    assert!(parse_tool_call_arguments(r#"{"a":"\u12"#)
        .arguments_error
        .is_some());
}
