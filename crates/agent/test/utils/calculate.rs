#![allow(dead_code)]
//! 移植自上游 `packages/agent/test/utils/calculate.ts`。
//!
//! 上游用 `new Function(\`return ${expression}\`)()` 让 JS 求值；Rust 没有 `eval`，所以这里只认
//! `数 运算符 数` 这一种写法（`+`、`-`、`*`、`/`、`%`），够 e2e 那条 `123 * 456` 用。见
//! `PORTING.md` 差异表 #75 的测试辅助那条。

use std::sync::Arc;

use serde_json::{json, Value};
use xjpcode_agent::{AgentToolResult, ToolError};
use xjpcode_ai::JsonObject;

use crate::tools::{js_text, text_block, TestTool};

/// 上游 `obj({ expression: str({ description }) }, { open: true })`：`open` 即不写
/// `additionalProperties`，多给的参数不算错。
pub fn calculate_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "expression": {
                "type": "string",
                "description": "The mathematical expression to evaluate"
            }
        },
        "required": ["expression"]
    })
}

/// 上游 `calculate`：求值，结果写成 `表达式 = 结果`；失败给一句话。
pub fn calculate(expression: &str) -> Result<String, String> {
    let result = evaluate(expression)?;
    Ok(format!("{expression} = {result}"))
}

/// 上游 `calculateTool`。
pub fn calculate_tool() -> TestTool {
    TestTool::new(
        "calculate",
        "Calculator",
        "Evaluate mathematical expressions",
        calculate_schema(),
        Arc::new(|_id, params: &JsonObject, _signal, _update| {
            let expression = params.get("expression").map(js_text).unwrap_or_default();
            Box::pin(async move {
                match calculate(&expression) {
                    Ok(text) => Ok(text_result(text)),
                    Err(message) => Err(ToolError::from(message)),
                }
            })
        }),
    )
}

/// 上游 `createCalculateToolWithUsage`：同一个工具，结果多带一份 `usage`。
pub fn create_calculate_tool_with_usage(usage: xjpcode_ai::Usage) -> TestTool {
    let mut tool = calculate_tool();
    let previous = Arc::clone(&tool.run);
    tool.run = Arc::new(move |id, params, signal, update| {
        let previous = Arc::clone(&previous);
        let usage = usage.clone();
        Box::pin(async move {
            let mut result = previous(id, params, signal, update).await?;
            result.usage = Some(usage);
            Ok(result)
        })
    });
    tool
}

fn text_result(text: String) -> AgentToolResult {
    AgentToolResult {
        content: vec![text_block(text)],
        details: None,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

/// 只在运算符上切一刀；两侧各是一个数。
fn evaluate(expression: &str) -> Result<f64, String> {
    let expression = expression.trim();
    let mut digits = expression.char_indices().skip(1);
    while let Some((index, character)) = digits.next() {
        if !matches!(character, '+' | '-' | '*' | '/' | '%') {
            continue;
        }
        let left = expression[..index].trim();
        let right = expression[index + 1..].trim();
        let left: f64 = left
            .parse()
            .map_err(|_| format!("Unsupported expression: {expression}"))?;
        let right: f64 = right
            .parse()
            .map_err(|_| format!("Unsupported expression: {expression}"))?;
        let value = match character {
            '+' => left + right,
            '-' => left - right,
            '*' => left * right,
            '/' => left / right,
            '%' => left % right,
            _ => unreachable!("only the operators above reach here"),
        };
        return Ok(value);
    }
    Err(format!("Unsupported expression: {expression}"))
}
