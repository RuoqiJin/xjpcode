//! 移植自上游 `packages/ai/test/transcript-tool-changes.test.ts`。
//!
//! 上游该文件共 21 个用例，其中 20 个通过 `compat.ts` 的 `streamSimple` 抓取 provider 请求体，
//! 断言的是 Anthropic / OpenAI / Kimi 适配层怎么把工具变化写进 payload——那属于 provider 层，
//! 不在本次范围（见 `PORTING.md` 差异表 #12）。只有下面这一个用例完全在 `transcript.ts` 内部，
//! 它是本文件唯一被搬过来的用例。

use serde_json::{json, Value};

use xjpcode_ai::types::{Message, Tool};
use xjpcode_ai::utils::transcript::{
    declarations_equal, resolve_transcript_tools, to_tool_declaration, TranscriptToolSupport,
    TranscriptTools,
};

fn message(value: Value) -> Message {
    serde_json::from_value(value).expect("fixture decodes")
}

fn tool(name: &str) -> Tool {
    Tool {
        name: name.into(),
        description: format!("{name} tool"),
        parameters: json!({}),
        constrained_sampling: None,
    }
}

fn with_description(tool: &Tool, description: &str) -> Tool {
    Tool {
        description: description.into(),
        ..tool.clone()
    }
}

#[test]
fn resolves_the_top_level_tool_boundary_per_transport() {
    let base_tool = tool("base_tool");
    let drop_tool = tool("drop_tool");
    let late_tool = tool("late_tool");

    let messages = vec![
        message(json!({
            "role": "system",
            "content": "base prompt",
            "toolsAdded": [
                { "name": "base_tool", "description": "base_tool tool", "parameters": {} },
                { "name": "drop_tool", "description": "drop_tool tool", "parameters": {} }
            ],
            "timestamp": 0
        })),
        message(json!({
            "role": "system",
            "content": "",
            "toolsRemoved": [{ "name": "drop_tool" }],
            "timestamp": 1
        })),
        message(json!({
            "role": "system",
            "content": "",
            "toolsAdded": [{ "name": "drop_tool", "description": "drop_tool tool", "parameters": {} }],
            "timestamp": 2
        })),
        message(json!({
            "role": "system",
            "content": "",
            "toolsAdded": [{ "name": "late_tool", "description": "x", "parameters": {} }],
            "timestamp": 3
        })),
        message(json!({
            "role": "system",
            "content": "",
            "toolsAdded": [{ "name": "late_tool", "description": "y", "parameters": {} }],
            "timestamp": 4
        })),
    ];

    assert_eq!(
        resolve_transcript_tools(&messages[..2], TranscriptToolSupport::Additions),
        TranscriptTools {
            request_tools: vec![base_tool.clone()],
            folded_system_messages: 2,
        }
    );
    // Kimi K3 会忽略被删除、随后又被工具消息重新声明的工具，所以重新加回的 drop_tool 会移动
    // 边界；它之后新出现的 late_tool 保持锚定。
    assert_eq!(
        resolve_transcript_tools(&messages[..4], TranscriptToolSupport::Additions),
        TranscriptTools {
            request_tools: vec![base_tool.clone(), drop_tool.clone()],
            folded_system_messages: 3,
        }
    );
    // late_tool 的重新定义把边界移到它所在的消息。
    assert_eq!(
        resolve_transcript_tools(&messages, TranscriptToolSupport::Additions),
        TranscriptTools {
            request_tools: vec![
                base_tool.clone(),
                drop_tool.clone(),
                with_description(&late_tool, "y")
            ],
            folded_system_messages: 5,
        }
    );
    assert_eq!(
        resolve_transcript_tools(&messages, TranscriptToolSupport::Changes),
        TranscriptTools {
            request_tools: Vec::new(),
            folded_system_messages: 0,
        }
    );
    assert_eq!(
        resolve_transcript_tools(&messages, TranscriptToolSupport::None),
        TranscriptTools {
            request_tools: vec![base_tool, drop_tool, with_description(&late_tool, "y")],
            folded_system_messages: 5,
        }
    );
}

/// 上游没有这一条：它守的是 `declarationsEqual` 背后 `JSON.stringify` 的两条 JS 规矩——像数字的键
/// 按 JS 的键序排，`1` 与 `1.0` 是同一个数（见 `PORTING.md` 差异表）。少了这两条，同一份声明换个键
/// 序就会被当成改过，工具表里多出一次“先移除再加回”。
#[test]
fn treats_declarations_that_js_writes_the_same_way_as_equal() {
    let left = Tool {
        name: "t".into(),
        description: "t tool".into(),
        parameters: serde_json::from_str(
            r#"{"type":"object","properties":{"b":{"type":"string"},"1":{"type":"string"}},"minimum":1}"#,
        )
        .expect("json"),
        constrained_sampling: None,
    };
    let right = Tool {
        parameters: serde_json::from_str(
            r#"{"type":"object","properties":{"1":{"type":"string"},"b":{"type":"string"}},"minimum":1.0}"#,
        )
        .expect("json"),
        ..left.clone()
    };
    assert!(declarations_equal(&left, &right));

    // 规范化之后对象的键序与 JS 一样：像数字的键在前。
    assert_eq!(
        serde_json::to_string(&to_tool_declaration(&left).parameters).expect("json"),
        r#"{"type":"object","properties":{"1":{"type":"string"},"b":{"type":"string"}},"minimum":1}"#
    );

    // 真正不同的声明仍然不同。
    let renamed = Tool {
        name: "other".into(),
        ..left.clone()
    };
    assert!(!declarations_equal(&left, &renamed));
}
