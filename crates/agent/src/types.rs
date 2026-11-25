//! 对应上游 `packages/agent/src/types.ts`。
//!
//! 与上游一致的：类型名、字段名（Rust 侧 snake_case）、字段可选性、联合的形状。钩子与回调统一落成
//! boxed-future 闭包，输入借用循环当下的那一份；落法逐条见 `docs/port/agent-contract.md`，
//! 汇总在 `PORTING.md` 差异表 #75。

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use xjpcode_ai::{
    AbortSignal, AssistantMessage, AssistantMessageEvent, AssistantMessageEventStream, BoxFuture,
    Callback, ClientIdentity, JsonObject, JsonValue, Message, Model, SimpleStreamOptions,
    SystemMessage, Tool, ToolCall, ToolResultContentBlock, ToolResultMessage, TranscriptContext,
    Usage, UserMessage,
};

pub use xjpcode_ai::providers::model_schema::ModelThinkingLevel as ThinkingLevel;

/// 流选项里的思考等级（上游 `SimpleStreamOptions.reasoning`）：六个值，没有 `"off"`。
pub use xjpcode_ai::providers::model_schema::ThinkingLevel as ReasoningLevel;

/// agent 的思考等级（含 `"off"`）换成流选项那六个值：`off` 在流选项里是「不给」。
pub fn reasoning_from_thinking_level(level: ThinkingLevel) -> Option<ReasoningLevel> {
    match level {
        ThinkingLevel::Off => None,
        ThinkingLevel::Minimal => Some(ReasoningLevel::Minimal),
        ThinkingLevel::Low => Some(ReasoningLevel::Low),
        ThinkingLevel::Medium => Some(ReasoningLevel::Medium),
        ThinkingLevel::High => Some(ReasoningLevel::High),
        ThinkingLevel::Xhigh => Some(ReasoningLevel::Xhigh),
        ThinkingLevel::Max => Some(ReasoningLevel::Max),
    }
}

/// 流选项的六个值换回 agent 的等级：没有对应值就是 `"off"`。
pub fn thinking_level_from_reasoning(level: ReasoningLevel) -> ThinkingLevel {
    match level {
        ReasoningLevel::Minimal => ThinkingLevel::Minimal,
        ReasoningLevel::Low => ThinkingLevel::Low,
        ReasoningLevel::Medium => ThinkingLevel::Medium,
        ReasoningLevel::High => ThinkingLevel::High,
        ReasoningLevel::Xhigh => ThinkingLevel::Xhigh,
        ReasoningLevel::Max => ThinkingLevel::Max,
    }
}

/// pi: `StreamFn`——agent 循环用来要一次 assistant 响应。
///
/// 上游的契约（`stream-fn` 的注释）：不得抛错、不得返回 rejected promise；失败写进流里（一条
/// `error` 或 `aborted` 的最终消息）。Rust 侧因此同步返回流，没有 `Result`。
pub type StreamFn = Arc<
    dyn Fn(Model, TranscriptContext, StreamFnOptions) -> AssistantMessageEventStream + Send + Sync,
>;

/// `StreamFn` 的第三个参数。
///
/// 上游是 `{...config, apiKey, identity, signal}`：`signal` 与流选项摊在同一层。Rust 的
/// `SimpleStreamOptions` 是数据（差异 #14），取消信号单独放在这里，等 provider 那一步把它
/// 收回 `StreamOptions` 时再去掉这层包装。
#[derive(Debug, Clone, Default)]
pub struct StreamFnOptions {
    pub options: SimpleStreamOptions,
    pub signal: Option<AbortSignal>,
}

/// pi: `ToolExecutionMode`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolExecutionMode {
    Sequential,
    #[default]
    Parallel,
}

/// pi: `QueueMode`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueueMode {
    #[default]
    #[serde(rename = "one-at-a-time")]
    OneAtATime,
    #[serde(rename = "all")]
    All,
}

/// pi: `AgentToolCall` = `Extract<AssistantMessage["content"][number], { type: "toolCall" }>`
pub type AgentToolCall = ToolCall;

/// pi: `BeforeToolCallResult`
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BeforeToolCallResult {
    pub block: Option<bool>,
    pub reason: Option<String>,
    pub terminate: Option<bool>,
}

/// pi: `AfterToolCallResult`
///
/// 上游用 `??` 合并：`null` 与 `undefined` 一样表示“没给”。这里 `Some(JsonValue::Null)` 也按
/// 没给处理（`declared` 保持缺席）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AfterToolCallResult {
    pub content: Option<Vec<ToolResultContentBlock>>,
    pub details: Option<JsonValue>,
    pub structured_content: Option<JsonValue>,
    pub is_error: Option<bool>,
    pub usage: Option<Usage>,
    pub terminate: Option<bool>,
}

/// pi: `BeforeToolCallContext`
pub struct BeforeToolCallContext<'a> {
    /// 发起这次调用的 assistant 消息。
    pub assistant_message: &'a AssistantMessage,
    /// `assistantMessage.content` 里的那个原始调用块。
    pub tool_call: &'a ToolCall,
    /// 通过校验后的参数；钩子可以改写，改过之后不再校验（上游同样如此）。
    pub args: &'a mut JsonValue,
    /// 准备这次调用时的 agent 上下文。
    pub context: &'a AgentContext,
}

/// pi: `AfterToolCallContext`
pub struct AfterToolCallContext<'a> {
    pub assistant_message: &'a AssistantMessage,
    pub tool_call: &'a ToolCall,
    pub args: &'a JsonValue,
    /// 覆盖之前的执行结果。
    pub result: &'a AgentToolResult,
    /// 这一结果此刻是否算失败。
    pub is_error: bool,
    pub context: &'a AgentContext,
}

/// pi: `AgentTurnContext`
pub struct AgentTurnContext<'a> {
    /// 完成这一轮的 assistant 消息。
    pub message: &'a AssistantMessage,
    /// 这一轮产生的工具结果消息。
    pub tool_results: &'a [ToolResultMessage],
    /// 追加完这一轮的 assistant 消息与工具结果之后的上下文。
    pub context: &'a AgentContext,
    /// 这次循环调用到此为止会返回的消息（prompt 含开头的提示消息，continuation 不含原有上下文）。
    pub new_messages: &'a [AgentMessage],
}

/// pi: `PrepareNextTurnContext extends AgentTurnContext`
pub type PrepareNextTurnContext<'a> = AgentTurnContext<'a>;

/// pi: `AgentTurnDecision`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTurnDecision {
    Continue,
    End,
}

/// pi: `AgentLoopTurnUpdate`
#[derive(Debug, Clone, Default)]
pub struct AgentLoopTurnUpdate {
    pub context: Option<AgentContext>,
    pub messages: Option<Vec<AgentMessage>>,
    pub model: Option<Model>,
    pub thinking_level: Option<ThinkingLevel>,
}

/// pi: `PrepareRequestContext`
pub struct PrepareRequestContext<'a> {
    pub context: &'a AgentContext,
    pub model: &'a Model,
    pub thinking_level: ThinkingLevel,
}

/// pi: `AgentRequestUpdate` = `Omit<AgentLoopTurnUpdate, "messages">`
#[derive(Debug, Clone, Default)]
pub struct AgentRequestUpdate {
    pub context: Option<AgentContext>,
    pub model: Option<Model>,
    pub thinking_level: Option<ThinkingLevel>,
}

/// pi: `AgentLoopConfig extends SimpleStreamOptions`
///
/// `extends` 落成字段重复（差异 #19），行为型字段是 `Callback`（差异 #14）。上游必给的
/// `model` 与 `convertToLlm` 在 Rust 里也是必给的；`Default` 只是给测试一个起点
/// （`model` 是 `agent.ts` 里的 `DEFAULT_MODEL`，`convertToLlm` 缺席，由调用方填）。
pub struct AgentLoopConfig {
    // 来自 SimpleStreamOptions（含 StreamOptions、ProviderRequestOptions）
    pub api_key: Option<String>,
    pub env: Option<xjpcode_ai::types::ProviderEnv>,
    pub on_payload: Callback<xjpcode_ai::types::OnPayloadFn>,
    pub on_response: Callback<xjpcode_ai::types::OnResponseFn>,
    pub on_provider_stream_event: Callback<xjpcode_ai::types::OnProviderStreamEventFn>,
    pub headers: Option<xjpcode_ai::types::ProviderHeaders>,
    pub identity: Option<ClientIdentity>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
    pub temperature: Option<f64>,
    pub sampling_params: Option<xjpcode_ai::types::SamplingParams>,
    pub max_tokens: Option<u64>,
    pub transport: Option<xjpcode_ai::types::Transport>,
    pub cache_retention: Option<xjpcode_ai::providers::model_schema::CacheRetention>,
    pub session_id: Option<String>,
    pub disable_continuation: Option<bool>,
    pub websocket_connect_timeout_ms: Option<u64>,
    pub metadata: Option<std::collections::BTreeMap<String, JsonValue>>,
    pub tool_choice: Option<xjpcode_ai::types::ToolChoice>,
    pub reasoning: Option<ReasoningLevel>,
    pub deferred: Option<xjpcode_ai::types::DeferredRequest>,
    pub thinking_budgets: Option<xjpcode_ai::types::ThinkingBudgets>,

    // 自有字段
    pub model: Model,
    /// pi: `convertToLlm`——每次请求前把 `AgentMessage[]` 变成模型认识的 `Message[]`。
    pub convert_to_llm: ConvertToLlm,
    /// pi: `transformContext?`
    pub transform_context: Option<TransformContext>,
    /// pi: `getApiKey?`——每次请求现取钥匙（OAuth 令牌会过期）。
    pub get_api_key: Option<GetApiKey>,
    /// pi: `finishTurn?`——一轮结束、`turn_end` 之前。
    pub finish_turn: Option<FinishTurn>,
    /// pi: `prepareRequest?`——每次请求之前（含第一次）。
    pub prepare_request: Option<PrepareRequest>,
    /// pi: `prepareNextTurn?`——`turn_end` 之后、下一轮之前。
    pub prepare_next_turn: Option<PrepareNextTurn>,
    /// pi: `getSteeringMessages?`
    pub get_steering_messages: Option<GetSteeringMessages>,
    /// pi: `getFollowUpMessages?`
    pub get_follow_up_messages: Option<GetFollowUpMessages>,
    /// pi: `toolExecution?`（缺省 `parallel`）
    pub tool_execution: ToolExecutionMode,
    /// pi: `beforeToolCall?`
    pub before_tool_call: Option<BeforeToolCall>,
    /// pi: `afterToolCall?`
    pub after_tool_call: Option<AfterToolCall>,
}

impl AgentLoopConfig {
    /// 把配置铺成流选项，行为等价于上游的 `{...config, apiKey, identity, signal}`。
    ///
    /// `api_key` 是 `getApiKey` 的结果（没有就回落到 `config.apiKey`），`identity` 是
    /// `mergeClientIdentity(DEFAULT_AGENT_IDENTITY, config.identity)` 的结果。
    pub fn to_stream_options(
        &self,
        api_key: Option<String>,
        identity: Option<ClientIdentity>,
        signal: Option<AbortSignal>,
    ) -> StreamFnOptions {
        StreamFnOptions {
            options: SimpleStreamOptions {
                api_key,
                env: self.env.clone(),
                on_payload: self.on_payload.clone(),
                on_response: self.on_response.clone(),
                on_provider_stream_event: self.on_provider_stream_event.clone(),
                headers: self.headers.clone(),
                identity,
                timeout_ms: self.timeout_ms,
                max_retries: self.max_retries,
                max_retry_delay_ms: self.max_retry_delay_ms,
                temperature: self.temperature,
                sampling_params: self.sampling_params.clone(),
                max_tokens: self.max_tokens,
                transport: self.transport,
                cache_retention: self.cache_retention,
                session_id: self.session_id.clone(),
                disable_continuation: self.disable_continuation,
                websocket_connect_timeout_ms: self.websocket_connect_timeout_ms,
                metadata: self.metadata.clone(),
                tool_choice: self.tool_choice,
                reasoning: self.reasoning,
                deferred: self.deferred.clone(),
                thinking_budgets: self.thinking_budgets.clone(),
            },
            signal,
        }
    }
}

/// pi: `convertToLlm` 的形状。
pub type ConvertToLlm =
    Arc<dyn for<'a> Fn(&'a [AgentMessage]) -> BoxFuture<'a, Vec<Message>> + Send + Sync>;

/// pi: `transformContext` 的形状。
pub type TransformContext = Arc<
    dyn for<'a> Fn(&'a [AgentMessage], Option<&'a AbortSignal>) -> BoxFuture<'a, Vec<AgentMessage>>
        + Send
        + Sync,
>;

/// pi: `getApiKey` 的形状。`None` 表示没有钥匙可用。
pub type GetApiKey = Arc<dyn for<'a> Fn(&'a str) -> BoxFuture<'a, Option<String>> + Send + Sync>;

/// pi: `FinishTurn` 的形状。`None` 就是上游的 `undefined`（照常排下一轮）。
pub type FinishTurn = Arc<
    dyn for<'a> Fn(
            &'a AgentTurnContext<'a>,
            Option<&'a AbortSignal>,
        ) -> BoxFuture<'a, Option<AgentTurnDecision>>
        + Send
        + Sync,
>;

/// pi: `PrepareRequest` 的形状。
pub type PrepareRequest = Arc<
    dyn for<'a> Fn(
            &'a PrepareRequestContext<'a>,
            Option<&'a AbortSignal>,
        ) -> BoxFuture<'a, Option<AgentRequestUpdate>>
        + Send
        + Sync,
>;

/// pi: `prepareNextTurn` 的形状（loop 那一份：拿整份轮结束快照）。
pub type PrepareNextTurn = Arc<
    dyn for<'a> Fn(&'a PrepareNextTurnContext<'a>) -> BoxFuture<'a, Option<AgentLoopTurnUpdate>>
        + Send
        + Sync,
>;

/// pi: `prepareNextTurnWithContext(context, signal?)` 的形状（`Agent` 上的那个槽位）。
pub type PrepareNextTurnWithContext = Arc<
    dyn for<'a> Fn(
            &'a PrepareNextTurnContext<'a>,
            Option<&'a AbortSignal>,
        ) -> BoxFuture<'a, Option<AgentLoopTurnUpdate>>
        + Send
        + Sync,
>;

/// pi: `getSteeringMessages`
pub type GetSteeringMessages = Arc<dyn Fn() -> BoxFuture<'static, Vec<AgentMessage>> + Send + Sync>;

/// pi: `getFollowUpMessages`
pub type GetFollowUpMessages = Arc<dyn Fn() -> BoxFuture<'static, Vec<AgentMessage>> + Send + Sync>;

/// pi: `beforeToolCall`
pub type BeforeToolCall = Arc<
    dyn for<'a> Fn(
            BeforeToolCallContext<'a>,
            Option<&'a AbortSignal>,
        ) -> BoxFuture<'a, Option<BeforeToolCallResult>>
        + Send
        + Sync,
>;

/// pi: `afterToolCall`
pub type AfterToolCall = Arc<
    dyn for<'a> Fn(
            AfterToolCallContext<'a>,
            Option<&'a AbortSignal>,
        ) -> BoxFuture<'a, Option<AfterToolCallResult>>
        + Send
        + Sync,
>;

/// pi: `AgentMessage`——四种 LLM 消息加应用自定义消息。
///
/// 上游的自定义消息靠声明合并（`CustomAgentMessages`）；Rust 没有声明合并，落成一个 `Custom`
/// 变体，`role` 与载荷原样带进 JSON（见 `PORTING.md` 差异表 #75）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentMessage {
    System(SystemMessage),
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
    Custom(CustomAgentMessage),
}

impl AgentMessage {
    /// 上游的 `message.role`。
    pub fn role(&self) -> &str {
        match self {
            Self::System(_) => "system",
            Self::User(_) => "user",
            Self::Assistant(_) => "assistant",
            Self::ToolResult(_) => "toolResult",
            Self::Custom(message) => message.role.as_str(),
        }
    }
}

/// pi: 声明合并出来的自定义消息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomAgentMessage {
    pub role: String,
    /// `role` 以外的字段原样保留（JSON 与上游同形）。
    #[serde(flatten)]
    pub payload: JsonObject,
}

impl From<SystemMessage> for AgentMessage {
    fn from(message: SystemMessage) -> Self {
        Self::System(message)
    }
}

impl From<UserMessage> for AgentMessage {
    fn from(message: UserMessage) -> Self {
        Self::User(message)
    }
}

impl From<AssistantMessage> for AgentMessage {
    fn from(message: AssistantMessage) -> Self {
        Self::Assistant(message)
    }
}

impl From<ToolResultMessage> for AgentMessage {
    fn from(message: ToolResultMessage) -> Self {
        Self::ToolResult(message)
    }
}

/// pi: `AgentState`（读取时给的是当下的快照）。
pub struct AgentState {
    /// 从记录里的系统消息重放出来的提示词（只读；改提示词就追加系统消息）。
    pub system_prompt: String,
    pub model: Model,
    pub thinking_level: ThinkingLevel,
    pub tools: Vec<Arc<dyn AgentTool>>,
    pub messages: Vec<AgentMessage>,
    /// 正在处理一次 prompt 或 continuation（到 `agent_end` 的监听器 settle 才变回 `false`）。
    pub is_streaming: bool,
    /// 当前流式响应的半成品消息。
    pub streaming_message: Option<AgentMessage>,
    /// 正在执行的工具调用 id。
    pub pending_tool_calls: BTreeSet<String>,
    /// 最近一次失败或中断的 assistant 轮留下的错误。
    pub error_message: Option<String>,
}

/// pi: `AgentToolResult`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolResult {
    /// 交给模型看的文本或图像。
    pub content: Vec<ToolResultContentBlock>,
    /// 给日志或界面看的结构化细节。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
    /// 与工具的 `outputSchema` 对得上的机读结果，不发给模型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<JsonValue>,
    /// 工具自身的用量，不计入主上下文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 不抛错地报告失败：模型看到的是错误结果，`details`/`structuredContent` 留给界面。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// 提示这一批之后就该停；整批都这么写才生效。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

impl AgentToolResult {
    /// 一条纯文本结果。
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ToolResultContentBlock::Text(xjpcode_ai::TextContent::new(
                text,
            ))],
            details: None,
            structured_content: None,
            usage: None,
            is_error: None,
            terminate: None,
        }
    }

    /// 结果里的全部文本拼起来（测试与界面用）。
    pub fn text_content(&self) -> String {
        xjpcode_ai::content_text(&self.content, "\n")
    }
}

/// pi: `AgentToolCallOutcome`
#[derive(Debug, Clone, PartialEq)]
pub struct AgentToolCallOutcome {
    pub tool_call: AgentToolCall,
    pub result: AgentToolResult,
    pub is_error: bool,
    /// `execute()` 用单调时钟量出的毫秒数；没跑的工具没有。
    pub duration_ms: Option<u64>,
}

/// pi: `AgentToolUpdateCallback`
///
/// 上游是同步回调（把 promise 塞进一个列表，等执行完再一起 await）。Rust 侧回调返回一个
/// future，循环把它收集起来，`execute()` 返回之后按到达顺序 await（见差异 #75）。
pub type AgentToolUpdateCallback =
    Arc<dyn Fn(AgentToolResult) -> BoxFuture<'static, ()> + Send + Sync>;

/// pi: `AgentTool.replay`——效果已经落定的工具，重放时怎么对待。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplayMode {
    Never,
    Safe,
}

/// pi: 工具执行失败的那句话。
///
/// 上游是 `throw new Error(message)`；Rust 用 `Result`，文字就是 `Error.message`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError(pub String);
impl ToolError {
    pub fn message(&self) -> &str {
        self.0.as_str()
    }
}

impl From<&str> for ToolError {
    fn from(message: &str) -> Self {
        Self(message.to_string())
    }
}

impl From<String> for ToolError {
    fn from(message: String) -> Self {
        Self(message)
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ToolError {}

/// 一次 run 的失败：上游的 `throw new Error(message)` / rejected promise 里的那句话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentError(pub String);

impl AgentError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    pub fn message(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for AgentError {}

impl From<&str> for AgentError {
    fn from(message: &str) -> Self {
        Self(message.to_string())
    }
}

impl From<String> for AgentError {
    fn from(message: String) -> Self {
        Self(message)
    }
}

/// pi: `AgentTool<TParameters, TDetails>`——agent 运行时看到的工具。
///
/// 上游 `AgentTool extends Tool`（`name`/`description`/`parameters`/`constrainedSampling`），另外有
/// `label`、`prepareArguments`、`outputSchema`、`execute`、`replay`、`executionMode`。Rust 侧参数
/// 类型被擦除成 `JsonObject`（校验与纠正之后的那一份）：
///
/// - `declaration()` 给出发给模型的声明，`name()` 给查找用的名字（上游同名字段）。
/// - `execute` 只有擦除的一层；`TypedTool<P>` 适配器等用出需要再加（`docs/port/agent-contract.md`）。
pub trait AgentTool: Send + Sync {
    /// 上游的 `tool.name`。
    fn name(&self) -> &str;

    /// 上游的 `tool.label`。
    fn label(&self) -> &str;

    /// 上游的 `toToolDeclaration(tool)`：发给模型的工具声明。
    fn declaration(&self) -> Tool;

    /// 上游的 `prepareArguments`：校验之前的兼容改写。缺省原样返回。
    fn prepare_arguments(&self, args: &JsonObject) -> Result<JsonObject, ToolError> {
        Ok(args.clone())
    }

    /// 上游的 `outputSchema`。
    fn output_schema(&self) -> Option<JsonValue> {
        None
    }

    /// 上游的 `replay`。
    fn replay(&self) -> Option<ReplayMode> {
        None
    }

    /// 上游的 `executionMode`。
    fn execution_mode(&self) -> Option<ToolExecutionMode> {
        None
    }

    fn execute<'a>(
        &'a self,
        tool_call_id: &'a str,
        params: &'a JsonObject,
        signal: Option<&'a AbortSignal>,
        on_update: Option<&'a AgentToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<AgentToolResult, ToolError>>;
}

/// pi: `AgentContext`
#[derive(Clone, Default)]
pub struct AgentContext {
    pub messages: Vec<AgentMessage>,
    pub tools: Option<Vec<Arc<dyn AgentTool>>>,
}

/// 工具是 `dyn AgentTool`（没有 `Debug`），只把名字写出来。
impl fmt::Debug for AgentContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentContext")
            .field("messages", &self.messages)
            .field(
                "tools",
                &self
                    .tools
                    .as_ref()
                    .map(|tools| tools.iter().map(|tool| tool.name()).collect::<Vec<_>>()),
            )
            .finish()
    }
}

/// pi: `AgentEvent`
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    AgentStart,
    AgentEnd {
        messages: Vec<AgentMessage>,
    },
    TurnStart,
    TurnEnd {
        message: AgentMessage,
        tool_results: Vec<ToolResultMessage>,
    },
    MessageStart {
        message: AgentMessage,
    },
    MessageUpdate {
        message: AgentMessage,
        assistant_message_event: AssistantMessageEvent,
    },
    MessageEnd {
        message: AgentMessage,
    },
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        /// 上游是 `any`：实际就是未 prepare、未校验的那份 `toolCall.arguments`。
        args: JsonObject,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: JsonObject,
        partial_result: AgentToolResult,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: AgentToolResult,
        is_error: bool,
        /// 单调时钟量出的 `execute()` 毫秒数；没跑的工具没有。
        duration_ms: Option<u64>,
    },
}

impl AgentEvent {
    /// 上游的 `event.type`。
    pub fn r#type(&self) -> &'static str {
        match self {
            Self::AgentStart => "agent_start",
            Self::AgentEnd { .. } => "agent_end",
            Self::TurnStart => "turn_start",
            Self::TurnEnd { .. } => "turn_end",
            Self::MessageStart { .. } => "message_start",
            Self::MessageUpdate { .. } => "message_update",
            Self::MessageEnd { .. } => "message_end",
            Self::ToolExecutionStart { .. } => "tool_execution_start",
            Self::ToolExecutionUpdate { .. } => "tool_execution_update",
            Self::ToolExecutionEnd { .. } => "tool_execution_end",
        }
    }
}

/// pi: `AgentInitialState`——`systemPrompt` 与 `tools` 会变成开头那条系统消息。
#[derive(Clone, Default)]
pub struct AgentInitialState {
    pub system_prompt: Option<String>,
    pub model: Option<Model>,
    pub thinking_level: Option<ThinkingLevel>,
    pub tools: Option<Vec<Arc<dyn AgentTool>>>,
    pub messages: Option<Vec<AgentMessage>>,
}

/// 上游到处用的 `Date.now()`：Unix 毫秒。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// 上游到处写的那个全零 `usage` 字面量（`agent.ts` 的 `EMPTY_USAGE`、`proxy.ts` 里的 partial）。
pub fn empty_usage() -> Usage {
    Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: xjpcode_ai::UsageCost::default(),
    }
}

/// 上游 `agent.ts`、`proxy.ts` 里各自内联的那份空 assistant 消息。
///
/// 上游没有名字（两个文件各写一遍字面量）；Rust 侧抽成一个函数，字段与那份字面量逐项相同。
pub fn empty_assistant_message(
    api: impl Into<String>,
    provider: impl Into<String>,
    model: impl Into<String>,
) -> AssistantMessage {
    AssistantMessage {
        role: xjpcode_ai::AssistantRole::Assistant,
        content: Vec::new(),
        api: api.into(),
        provider: provider.into(),
        model: model.into(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: empty_usage(),
        stop_reason: xjpcode_ai::StopReason::Pending,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: now_ms(),
        duration_ms: None,
    }
}
