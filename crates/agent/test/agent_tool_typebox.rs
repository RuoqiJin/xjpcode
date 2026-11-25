//! 移植自上游 `packages/agent/test/agent-tool-typebox.test.ts`。
//!
//! 上游这个文件通篇是 `expectTypeOf`：证明 `AgentTool` 的 `execute()` 与 `prepareArguments()`
//! 参数类型由 TypeBox schema 推出来（`{ command: string; mode?: "fast" | "deep" }`）。Rust 没有
//! `Static<>` 的对应物（差异表 #9）：工具参数在擦除层是 `JsonObject`。
//!
//! 这里保留运行时可验证的那一半：同一份 TypeBox 形状的 schema（含 `~kind` 元数据）原样交给工具，
//! 声明里逐字保留，参数按它校验/纠正之后再交给 `execute()`。类型级断言缺失本身登记在差异表 #9。

#[path = "utils/tools.rs"]
mod tools;

use std::sync::{Arc, Mutex};

use serde_json::json;
use xjpcode_agent::{AgentTool, AgentToolResult, ToolExecutionMode};
use xjpcode_ai::{validate_tool_arguments, JsonObject, Tool, ToolCall};

use tools::{boxed, TestTool};

/// 上游 `TObject` / `TString` / `TLiteral` / `TUnion` / `TOptional` 的那份 schema，逐字照抄。
fn typebox_parameters() -> serde_json::Value {
    json!({
        "~kind": "Object",
        "type": "object",
        "properties": {
            "command": { "~kind": "String", "type": "string" },
            "mode": {
                "~kind": "Union",
                "anyOf": [
                    { "~kind": "Literal", "const": "fast" },
                    { "~kind": "Literal", "const": "deep" }
                ],
                "~optional": true
            }
        },
        "required": ["command"]
    })
}

fn build_tool(seen: Arc<Mutex<Vec<JsonObject>>>) -> TestTool {
    let mut tool = TestTool::new(
        "bash",
        "Bash",
        "Runs a command",
        typebox_parameters(),
        Arc::new(move |_id, params, _signal, _update| {
            let params = params.clone();
            let seen = Arc::clone(&seen);
            boxed(async move {
                seen.lock().expect("params").push(params);
                Ok(AgentToolResult::text("ran"))
            })
        }),
    );
    tool.execution_mode = Some(ToolExecutionMode::Parallel);
    tool
}

#[tokio::test]
async fn keeps_the_typebox_schema_and_hands_the_validated_arguments_to_execute() {
    let seen: Arc<Mutex<Vec<JsonObject>>> = Arc::new(Mutex::new(Vec::new()));
    let tool = build_tool(Arc::clone(&seen));

    // 声明就是那份 schema，`~kind` 元数据不丢。
    let declaration: Tool = tool.declaration();
    assert_eq!(declaration.name, "bash");
    assert_eq!(declaration.description, "Runs a command");
    assert_eq!(declaration.parameters, typebox_parameters());

    // 参数按它校验：`mode` 是可选的两个字面量之一，多给的字面量不合格。
    let call = ToolCall::new(
        "call-1",
        "bash",
        json!({ "command": "ls", "mode": "fast" })
            .as_object()
            .expect("object")
            .clone(),
    );
    let validated = validate_tool_arguments(&declaration, &call).expect("valid arguments");
    assert_eq!(validated, json!({ "command": "ls", "mode": "fast" }));

    let invalid = ToolCall::new(
        "call-2",
        "bash",
        json!({ "command": "ls", "mode": "slow" })
            .as_object()
            .expect("object")
            .clone(),
    );
    assert!(validate_tool_arguments(&declaration, &invalid).is_err());

    // 走一遍真正的调用：工具拿到的就是校验后的那一份。
    let options = xjpcode_agent::RunToolCallOptions {
        tools: vec![Arc::new(tool)],
        assistant_message: xjpcode_agent::empty_assistant_message("faux", "faux", "faux-model"),
        context: xjpcode_agent::AgentContext::default(),
        signal: None,
        on_update: None,
        before_tool_call: None,
        after_tool_call: None,
    };
    let outcome = xjpcode_agent::run_tool_call(&call, options).await;
    assert!(!outcome.is_error);
    assert_eq!(
        *seen.lock().expect("params"),
        vec![json!({ "command": "ls", "mode": "fast" })
            .as_object()
            .expect("object")
            .clone()]
    );
}
