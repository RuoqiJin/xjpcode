#![allow(dead_code)]
//! 测试用的工具壳。
//!
//! 上游的测试直接写对象字面量（`const tool: AgentTool<…> = {...}`）；Rust 里要一个类型，所以这里
//! 放一个通用壳：名字、标签、参数 schema、`execute`、`prepareArguments`、执行方式都能填。
//! 上游没有这个文件；每个用例在 pi 里定义的字段在各自的 `TestTool` 里一一写出来。
//!
//! `test/utils/` 不是测试目标，由用到的测试按模块引入（`PORTING.md` 差异表 #6、#7）。
//! 每个测试文件各引一次本模块，只用到其中几样，所以这里关掉 `dead_code`。

use std::sync::Arc;

use serde_json::{json, Value};
use xjpcode_agent::{
    AgentTool, AgentToolResult, AgentToolUpdateCallback, ToolError, ToolExecutionMode,
};
use xjpcode_ai::{AbortSignal, JsonObject, TextContent, Tool, ToolResultContentBlock};

/// `execute` 那一层：参数已经校验/纠正过，失败用 `ToolError`。
pub type TestToolRun = Arc<
    dyn for<'a> Fn(
            &'a str,
            &'a JsonObject,
            Option<&'a AbortSignal>,
            Option<&'a AgentToolUpdateCallback>,
        ) -> xjpcode_ai::BoxFuture<'a, Result<AgentToolResult, ToolError>>
        + Send
        + Sync,
>;

/// `prepareArguments` 那一层。
pub type TestToolPrepare = Arc<dyn Fn(&JsonObject) -> Result<JsonObject, ToolError> + Send + Sync>;

/// 一个工具。字段与上游 `AgentTool` 的字段一一对应。
#[derive(Clone)]
pub struct TestTool {
    pub name: String,
    pub label: String,
    pub description: String,
    pub parameters: Value,
    pub output_schema: Option<Value>,
    pub execution_mode: Option<ToolExecutionMode>,
    pub prepare: Option<TestToolPrepare>,
    pub run: TestToolRun,
}

impl TestTool {
    pub fn new(
        name: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
        run: TestToolRun,
    ) -> Self {
        let name = name.into();
        Self {
            label: label.into(),
            description: description.into(),
            name,
            parameters,
            output_schema: None,
            execution_mode: None,
            prepare: None,
            run,
        }
    }

    /// 只回一条固定文本、参数里读 `value` 的工具（多数用例里的 `echo`）。
    pub fn echo(parameters: Value) -> Self {
        Self::new(
            "echo",
            "Echo",
            "Echo tool",
            parameters,
            Arc::new(|_id, params, _signal, _update| {
                let value = params.get("value").cloned().unwrap_or(Value::Null);
                Box::pin(async move {
                    Ok(AgentToolResult {
                        content: vec![text_block(format!("echoed: {}", js_text(&value)))],
                        details: Some(json!({ "value": value })),
                        structured_content: None,
                        usage: None,
                        is_error: None,
                        terminate: None,
                    })
                })
            }),
        )
    }
}

impl AgentTool for TestTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn declaration(&self) -> Tool {
        Tool {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
            constrained_sampling: None,
        }
    }

    fn prepare_arguments(&self, args: &JsonObject) -> Result<JsonObject, ToolError> {
        match &self.prepare {
            Some(prepare) => prepare(args),
            None => Ok(args.clone()),
        }
    }

    fn output_schema(&self) -> Option<Value> {
        self.output_schema.clone()
    }

    fn execution_mode(&self) -> Option<ToolExecutionMode> {
        self.execution_mode
    }

    fn execute<'a>(
        &'a self,
        tool_call_id: &'a str,
        params: &'a JsonObject,
        signal: Option<&'a AbortSignal>,
        on_update: Option<&'a AgentToolUpdateCallback>,
    ) -> xjpcode_ai::BoxFuture<'a, Result<AgentToolResult, ToolError>> {
        (self.run)(tool_call_id, params, signal, on_update)
    }
}

/// 把 async 块 boxed 成钩子的返回类型。
///
/// 直接写 `Box::pin(async {…})` 时，闭包的签名要先推出来，返回值不会自动变成 trait object；
/// 过一个返回类型明确的函数就没这个问题。
pub fn boxed<'a, T>(
    future: impl std::future::Future<Output = T> + Send + 'a,
) -> xjpcode_ai::BoxFuture<'a, T> {
    Box::pin(future)
}

pub fn text_block(text: impl Into<String>) -> ToolResultContentBlock {
    ToolResultContentBlock::Text(TextContent::new(text))
}

/// 一个成功的工具结果：一条文本，`details` 是 `{value}`。
pub fn value_result(text: impl Into<String>, value: &Value) -> AgentToolResult {
    AgentToolResult {
        content: vec![text_block(text)],
        details: Some(json!({ "value": value })),
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

/// `{ "value": … }` 这一层的参数 schema（上游 `obj({ value: str() })`）。
pub fn value_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "value": { "type": "string" } },
        "required": ["value"]
    })
}

/// 参数里的 `value` 当成文字读（上游 `params.value`）。
pub fn value_of(params: &JsonObject) -> String {
    match params.get("value") {
        Some(Value::String(value)) => value.clone(),
        Some(value) => js_text(value),
        None => String::new(),
    }
}

/// JS 里 `String(x)` 的写法，只用在测试的期望文字里。
pub fn js_text(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

/// 钩子构造器。
///
/// 直接把闭包塞进 `Arc::new(...)` 时，rustc 推不出闭包参数的高阶生命周期（`for<'a>`），会报
/// “implementation of Fn is not general enough”。过一个带 `for<'a>` 约束的函数就没有这个问题；
/// 与上游字面量直接写钩子等价，只是 Rust 侧的写法。
pub fn before_tool_call<F>(hook: F) -> xjpcode_agent::BeforeToolCall
where
    F: for<'a> Fn(
            xjpcode_agent::BeforeToolCallContext<'a>,
            Option<&'a AbortSignal>,
        )
            -> xjpcode_ai::BoxFuture<'a, Option<xjpcode_agent::BeforeToolCallResult>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(hook)
}

pub fn after_tool_call<F>(hook: F) -> xjpcode_agent::AfterToolCall
where
    F: for<'a> Fn(
            xjpcode_agent::AfterToolCallContext<'a>,
            Option<&'a AbortSignal>,
        ) -> xjpcode_ai::BoxFuture<'a, Option<xjpcode_agent::AfterToolCallResult>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(hook)
}

pub fn finish_turn<F>(hook: F) -> xjpcode_agent::FinishTurn
where
    F: for<'a> Fn(
            &'a xjpcode_agent::AgentTurnContext<'a>,
            Option<&'a AbortSignal>,
        ) -> xjpcode_ai::BoxFuture<'a, Option<xjpcode_agent::AgentTurnDecision>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(hook)
}

pub fn prepare_request<F>(hook: F) -> xjpcode_agent::PrepareRequest
where
    F: for<'a> Fn(
            &'a xjpcode_agent::PrepareRequestContext<'a>,
            Option<&'a AbortSignal>,
        ) -> xjpcode_ai::BoxFuture<'a, Option<xjpcode_agent::AgentRequestUpdate>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(hook)
}

pub fn prepare_next_turn<F>(hook: F) -> xjpcode_agent::PrepareNextTurn
where
    F: for<'a> Fn(
            &'a xjpcode_agent::PrepareNextTurnContext<'a>,
        ) -> xjpcode_ai::BoxFuture<'a, Option<xjpcode_agent::AgentLoopTurnUpdate>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(hook)
}

pub fn transform_context<F>(hook: F) -> xjpcode_agent::TransformContext
where
    F: for<'a> Fn(
            &'a [xjpcode_agent::AgentMessage],
            Option<&'a AbortSignal>,
        ) -> xjpcode_ai::BoxFuture<'a, Vec<xjpcode_agent::AgentMessage>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(hook)
}
