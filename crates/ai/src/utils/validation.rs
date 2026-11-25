//! 对应上游 `packages/ai/src/utils/validation.ts`。
//!
//! 模型发来一次工具调用：先用工具自己的 JSON Schema 把参数纠正（`parse()`，即 `coerce()` 之后再
//! 检查），合格的参数交给工具执行；不合格时把哪里错写成一段话。这段话模型会原样读到，所以文字与
//! pi 逐字相同：一条一条 `  - 路径: 原因`，空一行，再附上收到的参数（`JSON.stringify(_, null, 2)`）。
//!
//! 与上游不同的两处（见 `PORTING.md` 差异表）：上游按 schema 对象用 `WeakMap` 缓存编译好的
//! validator，Rust 侧的值是复制出来的、没有可用的对象身份，所以每次调用现编；上游 `throw new
//! Error(...)` 在这里是 `Err(String)`，文字不变。

use serde_json::Value;
use xjpcode_schema::formats::index::formats;
use xjpcode_schema::js;
use xjpcode_schema::types::{CompileOptions, ParseResult, UnresolvedRefs};
use xjpcode_schema::{compile, Validator};

use crate::types::{Tool, ToolCall};

/// pi: `validateToolCall`——按名字找到工具，再走 [`validate_tool_arguments`]。
///
/// 找不到工具时是 `Tool "<name>" not found`，与上游同一句话。
pub fn validate_tool_call(tools: &[Tool], tool_call: &ToolCall) -> Result<Value, String> {
    let tool = tools
        .iter()
        .find(|tool| tool.name == tool_call.name)
        .ok_or_else(|| format!("Tool \"{}\" not found", tool_call.name))?;
    validate_tool_arguments(tool, tool_call)
}

/// pi: `validateToolArguments`——参数先纠正再检查。
///
/// 合格时给纠正后的参数（可能是新的一份，工具改它不会改到 transcript 里的这次调用）；不合格时是
/// 回给模型看的那段话。参数文本本身不是合法 JSON 时（`argumentsError`）先报这一条。
pub fn validate_tool_arguments(tool: &Tool, tool_call: &ToolCall) -> Result<Value, String> {
    if let Some(error) = &tool_call.arguments_error {
        return Err(format!(
            "Invalid arguments for tool \"{}\": {error}",
            tool_call.name
        ));
    }
    // 上游 `structuredClone(toolCall.arguments)`：工具改自己的参数时，改不到这次调用。
    let arguments = Value::Object(tool_call.arguments.clone());
    let validator = validator(&tool.parameters)?;
    let result = validator
        .parse(&arguments)
        .map_err(|error| error.message().to_string())?;
    match result {
        ParseResult::Ok { value } => Ok(value),
        ParseResult::Errors { errors } => {
            let errors = errors
                .iter()
                .map(|error| {
                    format!(
                        "  - {}: {}",
                        format_validation_path(&error.path),
                        error.message
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Err(format!(
                "Validation failed for tool \"{}\":\n{}\n\nReceived arguments:\n{}",
                tool_call.name,
                errors,
                js::stringify_pretty(&arguments)
            ))
        }
    }
}

/// 工具的 schema 编译成带纠正的 validator。
///
/// 三项选项与上游 `getValidator` 相同：格式检查表、先纠正再检查、指空的 `$ref` 只让那一处的值
/// 不合格（MCP server 的 schema 里常有悬空定义，不能让整个工具都调不动）。
fn validator(parameters: &Value) -> Result<Validator, String> {
    let options = CompileOptions {
        formats: Some(formats().clone()),
        coerce: true,
        unresolved_refs: UnresolvedRefs::Reject,
        ..CompileOptions::default()
    };
    compile(parameters, &options).map_err(|error| error.message().to_string())
}

/// pi: `formatValidationPath`——pi-schema 的路径（JSON Pointer）变成模型看到的点号路径：
/// `/edits/0/oldText` → `edits.0.oldText`；段里的 `~1`/`~0` 先还原成 `/`/`~`。
fn format_validation_path(pointer: &str) -> String {
    if pointer.is_empty() {
        return "root".to_string();
    }
    // 上游 `pointer.slice(1)`：去掉开头那个 `/`。
    pointer
        .get(1..)
        .unwrap_or("")
        .split('/')
        .map(|segment| segment.replace("~1", "/").replace("~0", "~"))
        .collect::<Vec<_>>()
        .join(".")
}
