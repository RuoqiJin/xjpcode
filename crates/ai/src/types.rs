//! 对应上游 `packages/ai/src/types.ts`（核心子集：`pi-agent-core` 与 `utils/` 用到的部分）。
//!
//! 与上游一致的：类型名、字段名（Rust 侧 snake_case，JSON 侧保持 camelCase）、
//! 字段可选性、嵌套形状。
//!
//! 与上游不同的（见 `PORTING.md` 差异表）：TS 的字符串字面量类型落成单变体枚举；
//! TS 的判别式联合落成"结构体自带判别字段 + untagged 枚举"；`interface extends`
//! 落成字段重复；`Model.compat` 与若干行为型字段（signal / 回调 / fetch / telemetry）
//! 不在本次范围。

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::providers::compat_schema::OpenAICompletionsCompat;
use crate::providers::model_schema::{
    CacheRetention, ModelCost, ModelInputLimits, ModelInputModality, ModelPromptCache,
    ModelThinkingLevel, ThinkingLevel, ThinkingLevelMap,
};
use crate::utils::diagnostics::AssistantMessageDiagnostic;
use crate::utils::identity::ClientIdentity;

/// pi: `Api` = `KnownApi | (string & {})`
///
/// 开放联合在 Rust 里就是 `String`。已知取值：`openai-completions`、
/// `mistral-conversations`、`openai-responses`、`azure-openai-responses`、
/// `openai-codex-responses`、`anthropic-messages`、`bedrock-converse-stream`、
/// `google-generative-ai`、`google-vertex`、`pi-messages`。
pub type Api = String;

/// pi: `ProviderId` = `KnownProvider | string`
pub type ProviderId = String;

/// pi: `JsonValue`
pub type JsonValue = serde_json::Value;

/// pi: `JsonObject` = `{ [key: string]: JsonValue }`
pub type JsonObject = serde_json::Map<String, JsonValue>;

/// pi: `ToolChoice` = `"auto" | "none"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoice {
    Auto,
    None,
}

/// pi: `SamplingParams` = `Record<string, unknown>`
pub type SamplingParams = BTreeMap<String, JsonValue>;

/// pi: `SamplingParamsByThinkingLevel` = `Partial<Record<ModelThinkingLevel, SamplingParams>>`
pub type SamplingParamsByThinkingLevel = BTreeMap<ModelThinkingLevel, SamplingParams>;

/// pi: `ThinkingBudgets`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingBudgets {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimal: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub medium: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high: Option<u64>,
}

/// pi: `Transport` = `"sse" | "websocket" | "websocket-cached" | "auto"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transport {
    #[serde(rename = "sse")]
    Sse,
    #[serde(rename = "websocket")]
    Websocket,
    #[serde(rename = "websocket-cached")]
    WebsocketCached,
    #[serde(rename = "auto")]
    Auto,
}

/// pi: `ProviderEnv` = `Record<string, string>`
pub type ProviderEnv = BTreeMap<String, String>;

/// pi: `ProviderHeaders` = `Record<string, string | null>`
pub type ProviderHeaders = BTreeMap<String, Option<String>>;

/// pi: `ProviderResponse`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
}

/// 一个 boxed future。上游的钩子都可以返回 Promise；Rust 侧统一写成这个形状。
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 行为型字段（回调）的持有方式。
///
/// `Option<Arc<T>>` 没法直接放进要 derive 的结构体：`dyn Fn` 没有 `Debug`/`PartialEq`。这里给出
/// 三样：`Debug` 只写 `"<callback>"`，`PartialEq` 按 `Arc` 的身份比（同一份回调才相等），`Default`
/// 就是缺席——与上游 `field?: fn` 的缺席同义。
pub struct Callback<T: ?Sized>(Option<Arc<T>>);

impl<T: ?Sized> Clone for Callback<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: ?Sized> Callback<T> {
    pub fn new(callback: Arc<T>) -> Self {
        Self(Some(callback))
    }

    pub fn get(&self) -> Option<&T> {
        self.0.as_deref()
    }

    pub fn is_some(&self) -> bool {
        self.0.is_some()
    }

    /// 取一份可克隆的句柄（`&dyn Fn` 本身不能 clone）。
    pub fn arc(&self) -> Option<Arc<T>> {
        self.0.clone()
    }
}

impl<T: ?Sized> Default for Callback<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<T: ?Sized> fmt::Debug for Callback<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(if self.0.is_some() {
            "Some(<callback>)"
        } else {
            "None"
        })
    }
}

impl<T: ?Sized> PartialEq for Callback<T> {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (None, None) => true,
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            _ => false,
        }
    }
}

impl<T: ?Sized> From<Arc<T>> for Callback<T> {
    fn from(callback: Arc<T>) -> Self {
        Self::new(callback)
    }
}

/// pi: `ProviderRequestOptions.onPayload`——发送前看一眼或换掉 provider payload。
///
/// 上游是 `(payload: unknown, model) => unknown | undefined | Promise<…>`；`undefined` 表示不改。
pub type OnPayloadFn =
    dyn for<'a> Fn(JsonValue, &'a Model) -> BoxFuture<'a, Option<JsonValue>> + Send + Sync;

/// pi: `ProviderRequestOptions.onResponse`——收到 HTTP 响应时看一眼。
pub type OnResponseFn =
    dyn for<'a> Fn(&'a ProviderResponse, &'a Model) -> BoxFuture<'a, ()> + Send + Sync;

/// pi: `StreamOptions.onProviderStreamEvent`——归一化之前看每个 provider 流事件。
pub type OnProviderStreamEventFn =
    dyn for<'a> Fn(&'a JsonValue, &'a Model) -> BoxFuture<'a, ()> + Send + Sync;

/// 同一个回调的持有句柄（`Callback` 里的那份 `Arc`）。
pub type OnPayload = Arc<OnPayloadFn>;
pub type OnResponse = Arc<OnResponseFn>;
pub type OnProviderStreamEvent = Arc<OnProviderStreamEventFn>;

/// pi: `ProviderRequestOptions`
///
/// 行为型字段（回调）落成 `Callback`。`signal` 由 agent 的 `StreamFnOptions` 携带（两边都要用它，
/// 而它不是数据）；`telemetry` 来自上游另一个包；`fetch` 是 HTTP 实现的注入点，等 provider 步骤
/// 再定（见 `PORTING.md` 差异表 #14）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderRequestOptions {
    pub api_key: Option<String>,
    pub env: Option<ProviderEnv>,
    pub on_payload: Callback<OnPayloadFn>,
    pub on_response: Callback<OnResponseFn>,
    pub headers: Option<ProviderHeaders>,
    pub identity: Option<ClientIdentity>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
}

/// pi: `StreamOptions extends ProviderRequestOptions`
///
/// `extends` 在 Rust 里落成字段重复：本类型的字段集与上游的最终字段集一致。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamOptions {
    // 来自 ProviderRequestOptions
    pub api_key: Option<String>,
    pub env: Option<ProviderEnv>,
    pub on_payload: Callback<OnPayloadFn>,
    pub on_response: Callback<OnResponseFn>,
    pub headers: Option<ProviderHeaders>,
    pub identity: Option<ClientIdentity>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
    // 自有字段
    pub on_provider_stream_event: Callback<OnProviderStreamEventFn>,
    pub temperature: Option<f64>,
    pub sampling_params: Option<SamplingParams>,
    pub max_tokens: Option<u64>,
    pub transport: Option<Transport>,
    pub cache_retention: Option<CacheRetention>,
    pub session_id: Option<String>,
    pub disable_continuation: Option<bool>,
    pub websocket_connect_timeout_ms: Option<u64>,
    pub metadata: Option<BTreeMap<String, JsonValue>>,
}

impl StreamOptions {
    /// 把 `ProviderRequestOptions` 的数据字段铺进来，行为等价于上游的 `extends`。
    pub fn from_provider_request(request: ProviderRequestOptions) -> Self {
        Self {
            api_key: request.api_key,
            env: request.env,
            on_payload: request.on_payload,
            on_response: request.on_response,
            headers: request.headers,
            identity: request.identity,
            timeout_ms: request.timeout_ms,
            max_retries: request.max_retries,
            max_retry_delay_ms: request.max_retry_delay_ms,
            ..Self::default()
        }
    }
}

/// pi: `"15m" | "1h" | "24h"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeferredWindow {
    #[serde(rename = "15m")]
    Min15,
    #[serde(rename = "1h")]
    Hour1,
    #[serde(rename = "24h")]
    Hour24,
}

/// pi: `SimpleStreamOptions.deferred` = `boolean | { window?: "15m" | "1h" | "24h" }`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DeferredRequest {
    Enabled(bool),
    Window {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<DeferredWindow>,
    },
}

/// pi: `SimpleStreamOptions extends StreamOptions`
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SimpleStreamOptions {
    // 来自 StreamOptions（含 ProviderRequestOptions）
    pub api_key: Option<String>,
    pub env: Option<ProviderEnv>,
    pub on_payload: Callback<OnPayloadFn>,
    pub on_response: Callback<OnResponseFn>,
    pub on_provider_stream_event: Callback<OnProviderStreamEventFn>,
    pub headers: Option<ProviderHeaders>,
    pub identity: Option<ClientIdentity>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
    pub temperature: Option<f64>,
    pub sampling_params: Option<SamplingParams>,
    pub max_tokens: Option<u64>,
    pub transport: Option<Transport>,
    pub cache_retention: Option<CacheRetention>,
    pub session_id: Option<String>,
    pub disable_continuation: Option<bool>,
    pub websocket_connect_timeout_ms: Option<u64>,
    pub metadata: Option<BTreeMap<String, JsonValue>>,
    // 自有字段
    pub tool_choice: Option<ToolChoice>,
    pub reasoning: Option<ThinkingLevel>,
    pub deferred: Option<DeferredRequest>,
    pub thinking_budgets: Option<ThinkingBudgets>,
}

impl SimpleStreamOptions {
    /// 把 `StreamOptions` 的字段铺进来，行为等价于上游的 `extends`。
    pub fn from_stream_options(options: StreamOptions) -> Self {
        Self {
            api_key: options.api_key,
            env: options.env,
            on_payload: options.on_payload,
            on_response: options.on_response,
            on_provider_stream_event: options.on_provider_stream_event,
            headers: options.headers,
            identity: options.identity,
            timeout_ms: options.timeout_ms,
            max_retries: options.max_retries,
            max_retry_delay_ms: options.max_retry_delay_ms,
            temperature: options.temperature,
            sampling_params: options.sampling_params,
            max_tokens: options.max_tokens,
            transport: options.transport,
            cache_retention: options.cache_retention,
            session_id: options.session_id,
            disable_continuation: options.disable_continuation,
            websocket_connect_timeout_ms: options.websocket_connect_timeout_ms,
            metadata: options.metadata,
            ..Self::default()
        }
    }
}

// ---------------------------------------------------------------------------
// 内容块
//
// 上游用字面量判别字段 `type: "text"` 等；Rust 用单变体枚举表达"唯一取值"，
// 字段名仍是 `type`（`r#type`）。四个 `*Type` 枚举就是上游的四个字面量。
// ---------------------------------------------------------------------------

/// pi: `type: "text"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextContentType {
    #[serde(rename = "text")]
    Text,
}

/// pi: `TextContent`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    #[serde(rename = "type")]
    pub r#type: TextContentType,
    pub text: String,
    /// 上游注释：provider 私有元数据，按不透明字符串对待。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_signature: Option<String>,
}

/// pi: `type: "thinking"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThinkingContentType {
    #[serde(rename = "thinking")]
    Thinking,
}

/// pi: `ThinkingContent`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingContent {
    #[serde(rename = "type")]
    pub r#type: ThinkingContentType,
    pub thinking: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted: Option<bool>,
}

/// pi: `type: "image"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageContentType {
    #[serde(rename = "image")]
    Image,
}

/// pi: `ImageContent`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
    #[serde(rename = "type")]
    pub r#type: ImageContentType,
    /// base64 编码的图像数据。
    pub data: String,
    /// 例如 `image/jpeg`、`image/png`。
    pub mime_type: String,
}

/// pi: `type: "toolCall"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolCallType {
    #[serde(rename = "toolCall")]
    ToolCall,
}

/// pi: `ToolCall`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    #[serde(rename = "type")]
    pub r#type: ToolCallType,
    pub id: String,
    pub name: String,
    pub arguments: JsonObject,
    /// Google 私有：复用思考上下文的不透明签名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
    /// OpenAI Responses 中动态加载/命名空间工具的命名空间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// 完整参数文本不是合法 JSON 对象时置位，此时 `arguments` 为 `{}`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments_error: Option<String>,
}

impl TextContent {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            r#type: TextContentType::Text,
            text: text.into(),
            text_signature: None,
        }
    }
}

impl ImageContent {
    pub fn new(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self {
            r#type: ImageContentType::Image,
            data: data.into(),
            mime_type: mime_type.into(),
        }
    }
}

impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: JsonObject) -> Self {
        Self {
            r#type: ToolCallType::ToolCall,
            id: id.into(),
            name: name.into(),
            arguments,
            thought_signature: None,
            namespace: None,
            arguments_error: None,
        }
    }
}

// ---------------------------------------------------------------------------
// 用量
// ---------------------------------------------------------------------------

/// pi: `Usage.cost`（上游是内联对象字面量，Rust 必须命名）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_from_provider: Option<bool>,
}

/// pi: `Usage`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// `cacheWrite` 中按 1h 保留写入的子集。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_1h: Option<u64>,
    /// 已包含在 `output` 内，不要重复相加。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    pub total_tokens: u64,
    pub cost: UsageCost,
}

// ---------------------------------------------------------------------------
// 停止原因
// ---------------------------------------------------------------------------

/// pi: `StopReason`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    Pending,
    Stop,
    Length,
    ToolUse,
    Error,
    Aborted,
    Deferred,
}

// ---------------------------------------------------------------------------
// 延迟响应
// ---------------------------------------------------------------------------

/// pi: `DeferredHandle`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredHandle {
    pub provider: String,
    pub model_id: String,
    pub api: String,
    /// provider 令牌，例如 response id 或 batch id 加行 id。
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_after_ms: Option<u64>,
    /// 重建最终 assistant 消息所需的 provider 转换数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonValue>,
}

// ---------------------------------------------------------------------------
// 消息
// ---------------------------------------------------------------------------

/// pi: `role: "system"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SystemRole {
    #[serde(rename = "system")]
    System,
}

/// pi: `role: "user"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserRole {
    #[serde(rename = "user")]
    User,
}

/// pi: `role: "assistant"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssistantRole {
    #[serde(rename = "assistant")]
    Assistant,
}

/// pi: `role: "toolResult"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolResultRole {
    #[serde(rename = "toolResult")]
    ToolResult,
}

/// pi: `SystemMessage.content` = `string | TextContent[]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemContent {
    Text(String),
    Blocks(Vec<TextContent>),
}

/// pi: `SystemMessage`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemMessage {
    pub role: SystemRole,
    /// 首条消息上是基础 prompt；后续消息上是追加指令。
    pub content: SystemContent,
    /// 具名有序片段；`null` 表示移除该片段。顺序可观察（渲染顺序、缓存前缀），
    /// 因此用保留插入顺序的 `IndexMap`，与上游 `Object.entries` 一致。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<IndexMap<String, Option<String>>>,
    /// 此刻起可用的完整工具定义。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_added: Option<Vec<Tool>>,
    /// 此刻起不再可用的工具。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_removed: Option<Vec<ToolReference>>,
    /// Unix 毫秒时间戳。
    pub timestamp: u64,
}

/// pi: `UserMessage.content` = `string | (TextContent | ImageContent)[]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<UserContentBlock>),
}

/// pi: `TextContent | ImageContent`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContentBlock {
    Text(TextContent),
    Image(ImageContent),
}

/// pi: `UserMessage`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserMessage {
    pub role: UserRole,
    pub content: UserContent,
    /// Unix 毫秒时间戳。
    pub timestamp: u64,
}

/// pi: `TextContent | ThinkingContent | ToolCall`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AssistantContentBlock {
    Text(TextContent),
    Thinking(ThinkingContent),
    ToolCall(ToolCall),
}

/// pi: `AssistantMessage`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    pub role: AssistantRole,
    pub content: Vec<AssistantContentBlock>,
    pub api: Api,
    pub provider: ProviderId,
    pub model: String,
    /// provider 回报的具体模型，可能等于 `model`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    /// provider 原生 effort 等级原值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_thinking_level: Option<String>,
    /// agent loop 为本响应请求的 pi thinking 等级。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ModelThinkingLevel>,
    /// 已脱敏的 provider/运行时诊断。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<AssistantMessageDiagnostic>>,
    pub usage: Usage,
    pub stop_reason: StopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred: Option<DeferredHandle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_stop_reason: Option<String>,
    /// provider 是否显式结束了回合；当前不影响 agent 控制流。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_turn: Option<bool>,
    /// 请求开始时的 Unix 毫秒时间戳。
    pub timestamp: u64,
    /// 从 `timestamp` 到响应结束的毫秒数（单调时钟）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// pi: `NestedToolCallRecord`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedToolCallRecord {
    pub id: String,
    pub name: String,
    /// 超过大小限制时省略，此时用 `argumentsBytes` 给出体积。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<JsonObject>,
    /// 参数作为 JSON 的 UTF-8 字节数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments_bytes: Option<u64>,
    pub status: NestedToolCallStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// pi: `NestedToolCallRecord.status` = `"ok" | "error" | "unfinished"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NestedToolCallStatus {
    Ok,
    Error,
    Unfinished,
}

/// pi: `NestedToolCalls`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NestedToolCalls {
    pub calls: Vec<NestedToolCallRecord>,
    /// 有调用被丢弃、参数被省略或有调用未完成时为 `false`。
    pub complete: bool,
}

/// pi: `TextContent | ImageContent`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContentBlock {
    Text(TextContent),
    Image(ImageContent),
}

/// pi: `ToolResultMessage<TDetails = JsonValue>`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage<TDetails = JsonValue> {
    pub role: ToolResultRole,
    pub tool_call_id: String,
    pub tool_name: String,
    pub content: Vec<ToolResultContentBlock>,
    /// 工具私有细节；上游用 `IsJsonCompatible` 约束为 JSON，Rust 侧无该约束。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<TDetails>,
    /// 工具自身产生的用量，不计入主 LLM 上下文用量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// 该工具对其他工具发起的调用，仅留档，不发给模型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nested_calls: Option<NestedToolCalls>,
    pub is_error: bool,
    /// Unix 毫秒时间戳。
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// pi: `Message = SystemMessage | UserMessage | AssistantMessage | ToolResultMessage`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    System(SystemMessage),
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
}

impl Message {
    /// 上游的 `message.role`。
    pub fn role(&self) -> &'static str {
        match self {
            Self::System(_) => "system",
            Self::User(_) => "user",
            Self::Assistant(_) => "assistant",
            Self::ToolResult(_) => "toolResult",
        }
    }
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

/// pi: `GrammarFormat` = `"openai_lark" | "openai_regex"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrammarFormat {
    OpenaiLark,
    OpenaiRegex,
}

/// pi: `GrammarVariants` = `Partial<Record<GrammarFormat, string>>`
pub type GrammarVariants = BTreeMap<GrammarFormat, String>;

/// pi: `ConstrainedSamplingConfig`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ConstrainedSamplingConfig {
    #[serde(rename = "json_schema")]
    JsonSchema { strict: ConstrainedSamplingStrict },
    #[serde(rename = "grammar")]
    Grammar { variants: GrammarVariants },
}

/// pi: `ConstrainedSamplingConfig.strict` = `"prefer" | "require"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConstrainedSamplingStrict {
    Prefer,
    Require,
}

/// pi: `false` —— 「显式关闭」本身是一个取值，与字段缺席不同（`transcript.ts` 比较工具
/// 定义时会区分二者），因此不能收敛成 `None`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstrainedSamplingOff;

impl Serialize for ConstrainedSamplingOff {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(false)
    }
}

impl<'de> Deserialize<'de> for ConstrainedSamplingOff {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match bool::deserialize(deserializer)? {
            false => Ok(Self),
            true => Err(serde::de::Error::custom(
                "constrainedSampling must be false or a config object",
            )),
        }
    }
}

/// pi: `Tool.constrainedSampling` = `false | ConstrainedSamplingConfig`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConstrainedSampling {
    /// 上游的 `false` 字面量。
    Off(ConstrainedSamplingOff),
    Config(ConstrainedSamplingConfig),
}

/// pi: `Tool<TParameters = object>`
///
/// 上游 `parameters` 是 JSON Schema（pi-schema builder / TypeBox / 任意 schema 对象）；
/// Rust 侧保持泛型，默认 `JsonValue`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool<TParameters = JsonValue> {
    pub name: String,
    pub description: String,
    pub parameters: TParameters,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constrained_sampling: Option<ConstrainedSampling>,
}

/// pi: `ToolReference`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolReference {
    pub name: String,
}

// ---------------------------------------------------------------------------
// 请求上下文
// ---------------------------------------------------------------------------

/// pi: `Context`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
}

/// pi: `TranscriptContext`
///
/// 上游用 `unique symbol` 品牌位禁止裸 `Context` 进入 provider 代码；Rust 的
/// newtype 本身就是品牌，因此这里只有 `messages` 一个字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptContext {
    pub messages: Vec<Message>,
}

// ---------------------------------------------------------------------------
// 流事件
// ---------------------------------------------------------------------------

/// pi: `AssistantMessageEvent`
///
/// 上游 `reason: Extract<StopReason, "stop" | "length" | "toolUse" | "deferred">`
/// 与 `reason: Extract<StopReason, "aborted" | "error">` 的收窄没有 Rust 等价物，
/// 因此两个 `reason` 字段都用完整的 `StopReason`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AssistantMessageEvent {
    #[serde(rename = "start")]
    Start { partial: AssistantMessage },
    #[serde(rename = "text_start")]
    TextStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        partial: AssistantMessage,
    },
    #[serde(rename = "text_delta")]
    TextDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "text_end")]
    TextEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        content: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "thinking_start")]
    ThinkingStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        partial: AssistantMessage,
    },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "thinking_end")]
    ThinkingEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        content: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_start")]
    ToolcallStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_delta")]
    ToolcallDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_end")]
    ToolcallEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        #[serde(rename = "toolCall")]
        tool_call: ToolCall,
        partial: AssistantMessage,
    },
    #[serde(rename = "done")]
    Done {
        reason: StopReason,
        message: AssistantMessage,
    },
    #[serde(rename = "error")]
    Error {
        reason: StopReason,
        error: AssistantMessage,
    },
}

// ---------------------------------------------------------------------------
// 模型
// ---------------------------------------------------------------------------

/// pi: `ModelType` = `"chat" | "image" | "classifier"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelType {
    Chat,
    Image,
    Classifier,
}

/// pi: `Model<TApi>`（`BaseModel<TApi>` 的字段在此内联，因为 `BaseModel` 不在本次闭包内）
///
/// `compat` 依赖 `providers/compat-schema.ts` 的 8 组 provider 兼容配置，不在本次范围。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model<TApi = Api> {
    pub id: String,
    pub name: String,
    pub api: TApi,
    pub provider: ProviderId,
    pub base_url: String,
    pub input: Vec<ModelInputModality>,
    /// provider 输入限制与缓存安全预处理元数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_limits: Option<ModelInputLimits>,
    pub cost: ModelCost,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    /// 缺省即 chat；本字段在 `Model` 上只可能是 `"chat"`。
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<ModelType>,
    pub reasoning: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<ModelPromptCache>,
    pub context_window: u64,
    pub max_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<SamplingParams>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params_by_thinking_level: Option<SamplingParamsByThinkingLevel>,
    /// provider 兼容配置（上游的 `Model.compat`，只有 OpenAI 兼容路径会读）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<OpenAICompletionsCompat>,
}
