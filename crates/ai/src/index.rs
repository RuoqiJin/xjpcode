//! `@earendil-works/pi-ai` 的移植目标 crate 根，对应上游 `src/index.ts`。
//!
//! 台子阶段之后：目录层级与 `PORTING.md` 的文件对照表逐一对应，实现按文件推进。
//! 上游 `src/index.ts` 的整包 barrel 不在本次范围，本文件只做模块声明与已搬模块的再导出。

pub mod api;
pub mod auth;
pub mod models;
pub mod providers;
pub mod types;
pub mod utils;

pub use types::{
    Api, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole,
    BoxFuture, Callback, ConstrainedSampling, ConstrainedSamplingConfig, ConstrainedSamplingOff,
    Context, DeferredHandle, DeferredRequest, ImageContent, JsonObject, JsonValue, Message, Model,
    NestedToolCallRecord, NestedToolCallStatus, NestedToolCalls, OnPayload, OnPayloadFn, OnProviderStreamEvent,
    OnProviderStreamEventFn, OnResponse, OnResponseFn, ProviderEnv, ProviderHeaders, ProviderId,
    ProviderRequestOptions, ProviderResponse, SamplingParams, SimpleStreamOptions, StopReason,
    StreamOptions, SystemContent, SystemMessage, SystemRole, TextContent, TextContentType,
    ThinkingBudgets, ThinkingContent, ThinkingContentType, Tool, ToolCall, ToolChoice,
    ToolReference, ToolResultContentBlock, ToolResultMessage, ToolResultRole, TranscriptContext,
    Transport, Usage, UsageCost, UserContent, UserContentBlock, UserMessage, UserRole,
};
pub use utils::abort::{AbortController, AbortSignal};
pub use utils::event_stream::{
    create_assistant_message_event_stream, AssistantMessageEventStream, Consumer, EventStream,
};
pub use utils::identity::{
    merge_client_identity, ClientAttribution, ClientIdentity, UserAgentMode, PI_ATTRIBUTION,
};
pub use utils::json_parse::{
    parse_json_with_repair, parse_tool_call_arguments, repair_json, ParsedToolCallArguments,
};
pub use utils::overflow::{is_context_overflow, is_recoverable_length};
pub use utils::retry::{
    is_retryable_assistant_error, retry_assistant_call, retry_delay_ms, RetryCallbacks,
    RetryPolicy, DEFAULT_MAX_AGENT_RETRY_DELAY_MS,
};
pub use utils::streaming_json::{
    JsonChange, JsonPath, JsonPathSegment, ReplacedToolCallArguments, StreamingJsonError,
    StreamingJsonParser, ToolCallArgumentsStream,
};
pub use utils::text::{
    content_text, get_system_message_text, render_system_message_update, ContentText,
};
pub use utils::transcript::{
    collapse_system_messages, create_initial_system_message, declarations_equal,
    get_current_system_message, get_current_system_prompt, get_current_tools, get_declared_tools,
    get_initial_system_message, get_tool_state_changes, has_non_additive_tool_changes,
    has_tool_redefinitions, normalize_context, resolve_transcript, resolve_transcript_tools,
    to_tool_declaration, without_initial_system_message, ToolStateChanges, TranscriptToolSupport,
    TranscriptTools,
};
pub use utils::validation::{validate_tool_arguments, validate_tool_call};
