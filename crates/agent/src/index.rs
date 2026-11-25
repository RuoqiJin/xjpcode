//! `@earendil-works/pi-agent-core` 的 crate 根，对应上游 `src/index.ts`。
//!
//! 上游的 `index.ts` 只有五行再导出；这里做同一件事，只导出已搬的项。

pub mod agent;
pub mod agent_loop;
pub mod proxy;
pub mod stream_fn;
pub mod types;

pub use agent::{Agent, AgentOptions, PrepareNextTurnSignal, PromptInput, Subscription};
pub use agent_loop::{
    agent_loop, agent_loop_continue, create_error_tool_result, run_agent_loop,
    run_agent_loop_continue, run_tool_call, AgentEventSink, RunToolCallOptions, ToolCallHooks,
    ToolUpdateSink, DEFAULT_AGENT_IDENTITY,
};
pub use proxy::{
    stream_proxy, ProxyAssistantMessageEvent, ProxySerializableStreamOptions, ProxyStreamOptions,
};
pub use stream_fn::{get_default_stream_fn, set_default_stream_fn};
pub use types::{
    empty_assistant_message, empty_usage, now_ms, reasoning_from_thinking_level,
    thinking_level_from_reasoning, AfterToolCall, AfterToolCallContext, AfterToolCallResult,
    AgentContext, AgentError, AgentEvent, AgentInitialState, AgentLoopConfig, AgentLoopTurnUpdate,
    AgentMessage, AgentRequestUpdate, AgentState, AgentTool, AgentToolCall, AgentToolCallOutcome,
    AgentToolResult, AgentToolUpdateCallback, AgentTurnContext, AgentTurnDecision, BeforeToolCall,
    BeforeToolCallContext, BeforeToolCallResult, ConvertToLlm, CustomAgentMessage, FinishTurn,
    GetApiKey, GetFollowUpMessages, GetSteeringMessages, PrepareNextTurn, PrepareNextTurnContext,
    PrepareRequest, PrepareRequestContext, QueueMode, ReasoningLevel, ReplayMode, StreamFn,
    StreamFnOptions, ThinkingLevel, ToolError, ToolExecutionMode, TransformContext,
};
