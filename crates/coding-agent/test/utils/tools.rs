#![allow(dead_code)]
//! 测试里的工具调用辅助：上游直接 `tool.execute("id", {...})`，Rust 侧参数是 `JsonObject`。

use serde_json::{json, Map, Value};
use xjpcode_agent::{AgentTool, AgentToolResult, ToolError};

pub fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("expected an object, got {other}"),
    }
}

pub async fn execute(
    tool: &dyn AgentTool,
    args: Value,
) -> Result<AgentToolResult, ToolError> {
    tool.execute("test-call", &object(args), None, None).await
}

pub fn text(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            xjpcode_ai::ToolResultContentBlock::Text(text) => Some(text.text.clone()),
            xjpcode_ai::ToolResultContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

pub fn path_arg(path: &std::path::Path) -> Value {
    json!(path.to_string_lossy())
}
