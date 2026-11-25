//! 对应上游 `packages/agent/src/agent.ts`。
//!
//! 上游的 `Agent` 是单线程里的活对象：`state` 是活字段、钩子是公开可改的字段、`activeRun`
//! 是一个 promise。Rust 侧落成可克隆的句柄（`Arc<AgentInner>`）+ 各自加锁的槽位，方法都收
//! `&self`；钩子与 `streamFunction` 仍是可读可写的槽位（`docs/port/agent-coding-plan`「对内核的
//! 七条接线要求」第 1、2 条）。锁一律不跨 `await`。

use std::collections::BTreeSet;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use futures::FutureExt;
use tokio::sync::watch;
use xjpcode_ai::providers::model_schema::{ModelCost, ModelInputModality};
use xjpcode_ai::{
    get_current_system_message, get_current_system_prompt, to_tool_declaration, AbortController,
    AbortSignal, Callback, ClientIdentity, ImageContent, Message, Model, OnPayloadFn,
    OnProviderStreamEventFn, OnResponseFn, StopReason, TextContent, ThinkingBudgets, Tool,
    ToolChoice, Transport, UserContent, UserContentBlock, UserMessage, UserRole,
};

use crate::agent_loop::{run_agent_loop, run_agent_loop_continue};
use crate::stream_fn::get_default_stream_fn;
use crate::types::{
    empty_assistant_message, empty_usage, now_ms, reasoning_from_thinking_level, AfterToolCall,
    AgentContext, AgentError, AgentEvent, AgentInitialState, AgentLoopConfig, AgentLoopTurnUpdate,
    AgentMessage, AgentState, AgentTool, AgentTurnContext, BeforeToolCall, ConvertToLlm,
    FinishTurn, GetApiKey, GetFollowUpMessages, GetSteeringMessages, PrepareNextTurn,
    PrepareNextTurnWithContext, PrepareRequest, QueueMode, StreamFn, ThinkingLevel,
    ToolExecutionMode, TransformContext,
};

/// pi: `DEFAULT_MODEL`——没给初始模型时用的那个。
fn default_model() -> Model {
    Model {
        id: "unknown".to_string(),
        name: "unknown".to_string(),
        api: "unknown".to_string(),
        provider: "unknown".to_string(),
        base_url: String::new(),
        input: Vec::<ModelInputModality>::new(),
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        r#type: None,
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 0,
        max_tokens: 0,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

impl Default for AgentLoopConfig {
    /// 上游没有这个构造函数：`AgentLoopConfig` 每次都整个写出来。Rust 侧测试要用
    /// `..Default::default()`，所以给一个起点：`model` 是 `DEFAULT_MODEL`，`convertToLlm`
    /// 是「只留四种 LLM 消息」（与 `agent.ts` 的 `defaultConvertToLlm` 同义，见 `agent.rs`）。
    fn default() -> Self {
        Self {
            api_key: None,
            env: None,
            on_payload: Callback::default(),
            on_response: Callback::default(),
            on_provider_stream_event: Callback::default(),
            headers: None,
            identity: None,
            timeout_ms: None,
            max_retries: None,
            max_retry_delay_ms: None,
            temperature: None,
            sampling_params: None,
            max_tokens: None,
            transport: None,
            cache_retention: None,
            session_id: None,
            disable_continuation: None,
            websocket_connect_timeout_ms: None,
            metadata: None,
            tool_choice: None,
            reasoning: None,
            deferred: None,
            thinking_budgets: None,
            model: default_model(),
            convert_to_llm: default_convert_to_llm(),
            transform_context: None,
            get_api_key: None,
            finish_turn: None,
            prepare_request: None,
            prepare_next_turn: None,
            get_steering_messages: None,
            get_follow_up_messages: None,
            tool_execution: ToolExecutionMode::Parallel,
            before_tool_call: None,
            after_tool_call: None,
        }
    }
}

/// pi: `defaultConvertToLlm`——只留 system/user/assistant/toolResult 四种消息。
fn default_convert_to_llm() -> ConvertToLlm {
    Arc::new(|messages: &[AgentMessage]| {
        let messages: Vec<Message> = messages
            .iter()
            .filter_map(|message| match message {
                AgentMessage::System(message) => Some(Message::System(message.clone())),
                AgentMessage::User(message) => Some(Message::User(message.clone())),
                AgentMessage::Assistant(message) => Some(Message::Assistant(message.clone())),
                AgentMessage::ToolResult(message) => Some(Message::ToolResult(message.clone())),
                AgentMessage::Custom(_) => None,
            })
            .collect();
        Box::pin(async move { messages })
    })
}

/// `MutableAgentState`：`AgentState` 里可变的那几个字段。
struct MutableAgentState {
    model: Model,
    thinking_level: ThinkingLevel,
    tools: Vec<Arc<dyn AgentTool>>,
    messages: Vec<AgentMessage>,
    is_streaming: bool,
    streaming_message: Option<AgentMessage>,
    pending_tool_calls: BTreeSet<String>,
    error_message: Option<String>,
}

fn create_mutable_agent_state(initial_state: Option<AgentInitialState>) -> MutableAgentState {
    let initial_state = initial_state.unwrap_or_default();
    let tools = initial_state.tools.unwrap_or_default();
    let mut messages = initial_state.messages.unwrap_or_default();
    let initial_message = xjpcode_ai::create_initial_system_message(
        initial_state.system_prompt.as_deref(),
        Some(
            &tools
                .iter()
                .map(|tool| to_tool_declaration(&tool.declaration()))
                .collect::<Vec<Tool>>(),
        ),
    );
    if messages.first().map(AgentMessage::role) != Some("system") {
        if let Some(initial_message) = initial_message {
            messages.insert(0, AgentMessage::System(initial_message));
        }
    }

    MutableAgentState {
        model: initial_state.model.unwrap_or_else(default_model),
        thinking_level: initial_state.thinking_level.unwrap_or(ThinkingLevel::Off),
        tools,
        messages,
        is_streaming: false,
        streaming_message: None,
        pending_tool_calls: BTreeSet::new(),
        error_message: None,
    }
}

/// pi: `PendingMessageQueue`
struct PendingMessageQueue {
    messages: Vec<AgentMessage>,
    mode: QueueMode,
}

impl PendingMessageQueue {
    fn new(mode: QueueMode) -> Self {
        Self {
            messages: Vec::new(),
            mode,
        }
    }

    fn enqueue(&mut self, message: AgentMessage) {
        self.messages.push(message);
    }

    fn has_items(&self) -> bool {
        !self.messages.is_empty()
    }

    fn peek(&self) -> Vec<AgentMessage> {
        match self.mode {
            QueueMode::All => self.messages.clone(),
            QueueMode::OneAtATime => self.messages.first().cloned().into_iter().collect(),
        }
    }

    fn drain(&mut self) -> Vec<AgentMessage> {
        let drained = self.peek();
        self.messages.drain(..drained.len());
        drained
    }

    fn clear(&mut self) {
        self.messages.clear();
    }
}

struct ActiveRun {
    signal: AbortSignal,
    controller: AbortController,
    done: Arc<watch::Sender<bool>>,
    done_reader: watch::Receiver<bool>,
}

/// 上游 `runWithLifecycle` 的 `finally { this.finishRun() }`。
///
/// Rust 的 future 可以被半路丢掉（超时、`select!`、任务被取消），监听器也可能在处理失败事件时
/// panic；两种情况都走不到函数末尾，所以收尾放在 `Drop` 里。
struct FinishRun<'a> {
    agent: &'a Agent,
}

impl Drop for FinishRun<'_> {
    /// pi: `finishRun`
    fn drop(&mut self) {
        // 可能正在 unwind：这里再 panic 进程就没了，所以锁中毒了也照样收尾。
        let inner = &self.agent.inner;
        {
            let mut state = inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.is_streaming = false;
            state.streaming_message = None;
            state.pending_tool_calls = BTreeSet::new();
        }
        let run = inner
            .active_run
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(run) = run {
            run.done.send_replace(true);
        }
    }
}

/// 一个事件监听器。持有 `Weak` 句柄，免得与 agent 成环。
type Listener = Arc<
    dyn for<'a> Fn(&'a AgentEvent, &'a AbortSignal) -> xjpcode_ai::BoxFuture<'a, ()> + Send + Sync,
>;

struct ListenerEntry {
    id: u64,
    listener: Listener,
}

/// pi: `AgentOptions`
///
/// 上游的 `streamFn` 是必给参数（旧调用方省略时回落到默认值）。Rust 侧 `None` 就是那个回落。
pub struct AgentOptions {
    pub initial_state: Option<AgentInitialState>,
    pub convert_to_llm: Option<ConvertToLlm>,
    pub transform_context: Option<TransformContext>,
    pub stream_fn: Option<StreamFn>,
    pub get_api_key: Option<GetApiKey>,
    pub on_payload: Callback<OnPayloadFn>,
    pub on_response: Callback<OnResponseFn>,
    pub on_provider_stream_event: Callback<OnProviderStreamEventFn>,
    pub before_tool_call: Option<BeforeToolCall>,
    pub after_tool_call: Option<AfterToolCall>,
    pub finish_turn: Option<FinishTurn>,
    pub prepare_request: Option<PrepareRequest>,
    /// pi 旧形状的 `prepareNextTurn(signal?)`。
    pub prepare_next_turn: Option<PrepareNextTurnSignal>,
    /// pi 后来的 `prepareNextTurnWithContext(context, signal?)`。
    pub prepare_next_turn_with_context: Option<PrepareNextTurnWithContext>,
    pub steering_mode: Option<QueueMode>,
    pub follow_up_mode: Option<QueueMode>,
    pub session_id: Option<String>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub transport: Option<Transport>,
    pub max_retry_delay_ms: Option<u64>,
    pub tool_execution: Option<ToolExecutionMode>,
    pub identity: Option<ClientIdentity>,
}

impl Default for AgentOptions {
    fn default() -> Self {
        Self {
            initial_state: None,
            convert_to_llm: None,
            transform_context: None,
            stream_fn: None,
            get_api_key: None,
            on_payload: Callback::default(),
            on_response: Callback::default(),
            on_provider_stream_event: Callback::default(),
            before_tool_call: None,
            after_tool_call: None,
            finish_turn: None,
            prepare_request: None,
            prepare_next_turn: None,
            prepare_next_turn_with_context: None,
            steering_mode: None,
            follow_up_mode: None,
            session_id: None,
            thinking_budgets: None,
            transport: None,
            max_retry_delay_ms: None,
            tool_execution: None,
            identity: None,
        }
    }
}

/// pi 旧形状的 `prepareNextTurn(signal?)`。
pub type PrepareNextTurnSignal = Arc<
    dyn for<'a> Fn(
            Option<&'a AbortSignal>,
        ) -> xjpcode_ai::BoxFuture<'a, Option<AgentLoopTurnUpdate>>
        + Send
        + Sync,
>;

struct AgentInner {
    state: Mutex<MutableAgentState>,
    listeners: Mutex<Vec<ListenerEntry>>,
    next_listener_id: AtomicU64,
    steering_queue: Mutex<PendingMessageQueue>,
    follow_up_queue: Mutex<PendingMessageQueue>,
    active_run: Mutex<Option<ActiveRun>>,

    // 可读可写的槽位（上游是公开字段）。
    convert_to_llm: Mutex<ConvertToLlm>,
    transform_context: Mutex<Option<TransformContext>>,
    stream_function: Mutex<StreamFn>,
    get_api_key: Mutex<Option<GetApiKey>>,
    on_payload: Mutex<Callback<OnPayloadFn>>,
    on_response: Mutex<Callback<OnResponseFn>>,
    on_provider_stream_event: Mutex<Callback<OnProviderStreamEventFn>>,
    before_tool_call: Mutex<Option<BeforeToolCall>>,
    after_tool_call: Mutex<Option<AfterToolCall>>,
    finish_turn: Mutex<Option<FinishTurn>>,
    prepare_request: Mutex<Option<PrepareRequest>>,
    prepare_next_turn: Mutex<Option<PrepareNextTurnSignal>>,
    prepare_next_turn_with_context: Mutex<Option<PrepareNextTurnWithContext>>,
    session_id: Mutex<Option<String>>,
    thinking_budgets: Mutex<Option<ThinkingBudgets>>,
    transport: Mutex<Transport>,
    max_retry_delay_ms: Mutex<Option<u64>>,
    tool_execution: Mutex<ToolExecutionMode>,
    identity: Mutex<Option<ClientIdentity>>,
}

/// pi: `Agent`——低层循环外面的有状态壳。
///
/// 可克隆：每个克隆是同一个 agent 的句柄（上游的 `this` 只有一份，同步世界里不需要这个区分）。
#[derive(Clone)]
pub struct Agent {
    inner: Arc<AgentInner>,
}

impl Agent {
    /// pi: `new Agent({...})`——上游在构造函数里就解析默认流函数，所以这里也是 `Result`。
    pub fn new(options: AgentOptions) -> Result<Self, AgentError> {
        let stream_function = match options.stream_fn {
            Some(stream_function) => stream_function,
            None => get_default_stream_fn()?,
        };
        let agent = Self {
            inner: Arc::new(AgentInner {
                state: Mutex::new(create_mutable_agent_state(options.initial_state)),
                listeners: Mutex::new(Vec::new()),
                next_listener_id: AtomicU64::new(0),
                steering_queue: Mutex::new(PendingMessageQueue::new(
                    options.steering_mode.unwrap_or(QueueMode::OneAtATime),
                )),
                follow_up_queue: Mutex::new(PendingMessageQueue::new(
                    options.follow_up_mode.unwrap_or(QueueMode::OneAtATime),
                )),
                active_run: Mutex::new(None),
                convert_to_llm: Mutex::new(
                    options
                        .convert_to_llm
                        .unwrap_or_else(default_convert_to_llm),
                ),
                transform_context: Mutex::new(options.transform_context),
                stream_function: Mutex::new(stream_function),
                get_api_key: Mutex::new(options.get_api_key),
                on_payload: Mutex::new(options.on_payload),
                on_response: Mutex::new(options.on_response),
                on_provider_stream_event: Mutex::new(options.on_provider_stream_event),
                before_tool_call: Mutex::new(options.before_tool_call),
                after_tool_call: Mutex::new(options.after_tool_call),
                finish_turn: Mutex::new(options.finish_turn),
                prepare_request: Mutex::new(options.prepare_request),
                prepare_next_turn: Mutex::new(options.prepare_next_turn),
                prepare_next_turn_with_context: Mutex::new(options.prepare_next_turn_with_context),
                session_id: Mutex::new(options.session_id),
                thinking_budgets: Mutex::new(options.thinking_budgets),
                transport: Mutex::new(options.transport.unwrap_or(Transport::Auto)),
                max_retry_delay_ms: Mutex::new(options.max_retry_delay_ms),
                tool_execution: Mutex::new(
                    options
                        .tool_execution
                        .unwrap_or(ToolExecutionMode::Parallel),
                ),
                identity: Mutex::new(options.identity),
            }),
        };
        Ok(agent)
    }

    /// pi: `subscribe`——按订阅顺序 await；`agent_end` 的监听器 settle 之后才算 idle。
    pub fn subscribe<F>(&self, listener: F) -> Subscription
    where
        F: for<'a> Fn(&'a AgentEvent, &'a AbortSignal) -> xjpcode_ai::BoxFuture<'a, ()>
            + Send
            + Sync
            + 'static,
    {
        let id = self.inner.next_listener_id.fetch_add(1, Ordering::SeqCst);
        self.inner
            .listeners
            .lock()
            .expect("listeners")
            .push(ListenerEntry {
                id,
                listener: Arc::new(listener),
            });
        Subscription {
            inner: Arc::downgrade(&self.inner),
            id,
        }
    }

    /// 当下的状态快照（上游的 `state` 是活对象，见差异 #75）。
    pub fn state(&self) -> AgentState {
        let state = self.inner.state.lock().expect("agent state");
        let system_messages: Vec<Message> = state
            .messages
            .iter()
            .filter_map(|message| match message {
                AgentMessage::System(message) => Some(Message::System(message.clone())),
                _ => None,
            })
            .collect();
        AgentState {
            system_prompt: get_current_system_prompt(&system_messages),
            model: state.model.clone(),
            thinking_level: state.thinking_level,
            tools: state.tools.clone(),
            messages: state.messages.clone(),
            is_streaming: state.is_streaming,
            streaming_message: state.streaming_message.clone(),
            pending_tool_calls: state.pending_tool_calls.clone(),
            error_message: state.error_message.clone(),
        }
    }

    /// 上游的 `state.systemPrompt`（只读、从记录重放出来）。
    pub fn system_prompt(&self) -> String {
        self.state().system_prompt
    }

    pub fn set_tools(&self, tools: Vec<Arc<dyn AgentTool>>) {
        self.inner.state.lock().expect("agent state").tools = tools;
    }

    pub fn set_messages(&self, messages: Vec<AgentMessage>) {
        self.inner.state.lock().expect("agent state").messages = messages;
    }

    pub fn set_model(&self, model: Model) {
        self.inner.state.lock().expect("agent state").model = model;
    }

    pub fn set_thinking_level(&self, thinking_level: ThinkingLevel) {
        self.inner.state.lock().expect("agent state").thinking_level = thinking_level;
    }

    /// 上游 `state.messages.push(...)`：Rust 侧状态是快照，追加要走这个方法。
    pub fn append_message(&self, message: AgentMessage) {
        self.inner
            .state
            .lock()
            .expect("agent state")
            .messages
            .push(message);
    }

    /// pi: `steeringMode`（可读可写）
    pub fn steering_mode(&self) -> QueueMode {
        self.inner
            .steering_queue
            .lock()
            .expect("steering queue")
            .mode
    }

    pub fn set_steering_mode(&self, mode: QueueMode) {
        self.inner
            .steering_queue
            .lock()
            .expect("steering queue")
            .mode = mode;
    }

    /// pi: `followUpMode`（可读可写）
    pub fn follow_up_mode(&self) -> QueueMode {
        self.inner
            .follow_up_queue
            .lock()
            .expect("follow-up queue")
            .mode
    }

    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        self.inner
            .follow_up_queue
            .lock()
            .expect("follow-up queue")
            .mode = mode;
    }

    /// pi: `steer`
    pub fn steer(&self, message: AgentMessage) {
        self.inner
            .steering_queue
            .lock()
            .expect("steering queue")
            .enqueue(message);
    }

    /// pi: `followUp`
    pub fn follow_up(&self, message: AgentMessage) {
        self.inner
            .follow_up_queue
            .lock()
            .expect("follow-up queue")
            .enqueue(message);
    }

    pub fn clear_steering_queue(&self) {
        self.inner
            .steering_queue
            .lock()
            .expect("steering queue")
            .clear();
    }

    pub fn clear_follow_up_queue(&self) {
        self.inner
            .follow_up_queue
            .lock()
            .expect("follow-up queue")
            .clear();
    }

    pub fn clear_all_queues(&self) {
        self.clear_steering_queue();
        self.clear_follow_up_queue();
    }

    pub fn has_queued_messages(&self) -> bool {
        self.inner
            .steering_queue
            .lock()
            .expect("steering queue")
            .has_items()
            || self
                .inner
                .follow_up_queue
                .lock()
                .expect("follow-up queue")
                .has_items()
    }

    /// pi: `peekQueuedMessages`——看一眼下一轮会投哪些，不消费。
    pub fn peek_queued_messages(&self) -> Vec<AgentMessage> {
        let steering = self
            .inner
            .steering_queue
            .lock()
            .expect("steering queue")
            .peek();
        if !steering.is_empty() {
            return steering;
        }
        self.inner
            .follow_up_queue
            .lock()
            .expect("follow-up queue")
            .peek()
    }

    /// pi: `signal`——当前 run 的取消信号，没有 run 时是 `None`。
    pub fn signal(&self) -> Option<AbortSignal> {
        self.inner
            .active_run
            .lock()
            .expect("active run")
            .as_ref()
            .map(|run| run.signal.clone())
    }

    /// pi: `abort`
    pub fn abort(&self) {
        if let Some(run) = self.inner.active_run.lock().expect("active run").as_ref() {
            run.controller.abort();
        }
    }

    /// pi: `waitForIdle`——当前 run 与它的监听器都结束之后返回。
    pub async fn wait_for_idle(&self) {
        let mut reader = match self.inner.active_run.lock().expect("active run").as_ref() {
            Some(run) => run.done_reader.clone(),
            None => return,
        };
        while !*reader.borrow_and_update() {
            if reader.changed().await.is_err() {
                break;
            }
        }
    }

    /// pi: `reset`——清掉对话与队列，留下重放出来的提示词与工具基线。
    pub fn reset(&self) -> Result<(), AgentError> {
        if self.inner.active_run.lock().expect("active run").is_some() {
            return Err(AgentError::new(
                "Agent is already processing. Wait for completion before resetting.",
            ));
        }
        let mut state = self.inner.state.lock().expect("agent state");
        let system_messages: Vec<Message> = state
            .messages
            .iter()
            .filter_map(|message| match message {
                AgentMessage::System(message) => Some(Message::System(message.clone())),
                _ => None,
            })
            .collect();
        let baseline = get_current_system_message(&system_messages);
        state.messages = baseline.into_iter().map(AgentMessage::System).collect();
        state.is_streaming = false;
        state.streaming_message = None;
        state.pending_tool_calls = BTreeSet::new();
        state.error_message = None;
        drop(state);
        self.clear_follow_up_queue();
        self.clear_steering_queue();
        Ok(())
    }

    /// pi: `prompt`——从文字、一条消息或一批消息开始一次。
    pub async fn prompt(&self, input: impl Into<PromptInput>) -> Result<(), AgentError> {
        if self.inner.active_run.lock().expect("active run").is_some() {
            return Err(AgentError::new(
                "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion.",
            ));
        }
        let messages = normalize_prompt_input(input.into());
        self.run_prompt_messages(messages, false).await
    }

    /// pi: `continue`——从现有记录接着跑；最后一条必须是 user 或 toolResult。
    pub async fn r#continue(&self) -> Result<(), AgentError> {
        if self.inner.active_run.lock().expect("active run").is_some() {
            return Err(AgentError::new(
                "Agent is already processing. Wait for completion before continuing.",
            ));
        }

        let (last_role, every_message_is_system) = {
            let state = self.inner.state.lock().expect("agent state");
            (
                state
                    .messages
                    .last()
                    .map(|message| message.role().to_string()),
                state
                    .messages
                    .iter()
                    .all(|message| message.role() == "system"),
            )
        };
        let Some(last_role) = last_role else {
            return Err(AgentError::new("No messages to continue from"));
        };
        if every_message_is_system {
            return Err(AgentError::new("No messages to continue from"));
        }

        if last_role == "assistant" {
            let queued_steering = self
                .inner
                .steering_queue
                .lock()
                .expect("steering queue")
                .drain();
            if !queued_steering.is_empty() {
                return self.run_prompt_messages(queued_steering, true).await;
            }
            let queued_follow_ups = self
                .inner
                .follow_up_queue
                .lock()
                .expect("follow-up queue")
                .drain();
            if !queued_follow_ups.is_empty() {
                return self.run_prompt_messages(queued_follow_ups, false).await;
            }
            return Err(AgentError::new(
                "Cannot continue from message role: assistant",
            ));
        }

        self.run_continuation().await
    }

    async fn run_prompt_messages(
        &self,
        messages: Vec<AgentMessage>,
        skip_initial_steering_poll: bool,
    ) -> Result<(), AgentError> {
        self.run_with_lifecycle(|signal| async move {
            let context = self.create_context_snapshot();
            let config = self.create_loop_config(skip_initial_steering_poll);
            // 上游：每次 run 开头取当下的 `streamFunction`（接线要求第 2 条）。
            let stream_function = self.stream_function();
            run_agent_loop(
                messages,
                context,
                config,
                self.event_sink(),
                Some(signal),
                Some(stream_function),
            )
            .await?;
            Ok(())
        })
        .await
    }

    async fn run_continuation(&self) -> Result<(), AgentError> {
        self.run_with_lifecycle(|signal| async move {
            let context = self.create_context_snapshot();
            let config = self.create_loop_config(false);
            let stream_function = self.stream_function();
            run_agent_loop_continue(
                context,
                config,
                self.event_sink(),
                Some(signal),
                Some(stream_function),
            )
            .await?;
            Ok(())
        })
        .await
    }

    fn create_context_snapshot(&self) -> AgentContext {
        let state = self.inner.state.lock().expect("agent state");
        AgentContext {
            messages: state.messages.clone(),
            tools: Some(state.tools.clone()),
        }
    }

    fn create_loop_config(&self, skip_initial_steering_poll: bool) -> AgentLoopConfig {
        let (model, thinking_level) = {
            let state = self.inner.state.lock().expect("agent state");
            (state.model.clone(), state.thinking_level)
        };
        let agent = self.clone();
        let skip_initial_steering_poll = Arc::new(AtomicBool::new(skip_initial_steering_poll));
        let steering_queue = Arc::downgrade(&self.inner);
        let follow_up_queue = Arc::downgrade(&self.inner);

        let prepare_next_turn = {
            let has_with_context = self
                .inner
                .prepare_next_turn_with_context
                .lock()
                .expect("prepare next turn")
                .is_some();
            let has_legacy = self
                .inner
                .prepare_next_turn
                .lock()
                .expect("prepare next turn")
                .is_some();
            if !has_with_context && !has_legacy {
                None
            } else {
                Some(prepare_next_turn_wrapper(agent.clone()))
            }
        };

        let get_steering_messages: GetSteeringMessages = Arc::new(move || {
            let skip = Arc::clone(&skip_initial_steering_poll);
            let steering_queue = steering_queue.clone();
            Box::pin(async move {
                if skip.swap(false, Ordering::SeqCst) {
                    return Vec::new();
                }
                match steering_queue.upgrade() {
                    Some(inner) => inner.steering_queue.lock().expect("steering queue").drain(),
                    None => Vec::new(),
                }
            })
        });
        let get_follow_up_messages: GetFollowUpMessages = Arc::new(move || {
            let follow_up_queue = follow_up_queue.clone();
            Box::pin(async move {
                match follow_up_queue.upgrade() {
                    Some(inner) => inner
                        .follow_up_queue
                        .lock()
                        .expect("follow-up queue")
                        .drain(),
                    None => Vec::new(),
                }
            })
        });

        let config = AgentLoopConfig {
            api_key: None,
            env: None,
            on_payload: self.inner.on_payload.lock().expect("on payload").clone(),
            on_response: self.inner.on_response.lock().expect("on response").clone(),
            on_provider_stream_event: self
                .inner
                .on_provider_stream_event
                .lock()
                .expect("on provider stream event")
                .clone(),
            headers: None,
            identity: self.inner.identity.lock().expect("identity").clone(),
            timeout_ms: None,
            max_retries: None,
            max_retry_delay_ms: *self
                .inner
                .max_retry_delay_ms
                .lock()
                .expect("max retry delay"),
            temperature: None,
            sampling_params: None,
            max_tokens: None,
            transport: Some(*self.inner.transport.lock().expect("transport")),
            cache_retention: None,
            session_id: self.inner.session_id.lock().expect("session id").clone(),
            disable_continuation: None,
            websocket_connect_timeout_ms: None,
            metadata: None,
            tool_choice: None::<ToolChoice>,
            reasoning: reasoning_from_thinking_level(thinking_level),
            deferred: None,
            thinking_budgets: self
                .inner
                .thinking_budgets
                .lock()
                .expect("thinking budgets")
                .clone(),
            model,
            convert_to_llm: self
                .inner
                .convert_to_llm
                .lock()
                .expect("convert to llm")
                .clone(),
            transform_context: self
                .inner
                .transform_context
                .lock()
                .expect("transform context")
                .clone(),
            get_api_key: self.inner.get_api_key.lock().expect("get api key").clone(),
            finish_turn: self.inner.finish_turn.lock().expect("finish turn").clone(),
            prepare_request: self
                .inner
                .prepare_request
                .lock()
                .expect("prepare request")
                .clone(),
            prepare_next_turn,
            get_steering_messages: Some(get_steering_messages),
            get_follow_up_messages: Some(get_follow_up_messages),
            tool_execution: *self.inner.tool_execution.lock().expect("tool execution"),
            before_tool_call: self
                .inner
                .before_tool_call
                .lock()
                .expect("before tool call")
                .clone(),
            after_tool_call: self
                .inner
                .after_tool_call
                .lock()
                .expect("after tool call")
                .clone(),
        };
        config
    }

    async fn run_with_lifecycle<F, Fut>(&self, executor: F) -> Result<(), AgentError>
    where
        F: FnOnce(AbortSignal) -> Fut,
        Fut: std::future::Future<Output = Result<(), AgentError>>,
    {
        let controller = AbortController::new();
        let signal = controller.signal();
        let (done, done_reader) = watch::channel(false);
        {
            // 「没有在跑」的检查和登记在同一把锁里做完。上游是单线程，这两步之间插不进别的调用；
            // 这里几个线程可以同时进来，分两次取锁的话都能通过检查。
            let mut active_run = self.inner.active_run.lock().expect("active run");
            if active_run.is_some() {
                return Err(AgentError::new("Agent is already processing."));
            }
            *active_run = Some(ActiveRun {
                signal: signal.clone(),
                controller: controller.clone(),
                done: Arc::new(done),
                done_reader,
            });
        }
        // 上游的 `finally { this.finishRun() }`：从这里起不管怎么离开这个函数，都会收尾。
        let _finish_run = FinishRun { agent: self };

        {
            let mut state = self.inner.state.lock().expect("agent state");
            state.is_streaming = true;
            state.streaming_message = None;
            state.error_message = None;
        }

        let aborted = || controller.signal().aborted();
        let outcome = AssertUnwindSafe(executor(signal)).catch_unwind().await;
        let failure = match outcome {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error.0),
            Err(payload) => Some(panic_message(payload)),
        };
        if let Some(message) = failure {
            self.handle_run_failure(&message, aborted()).await?;
        }
        Ok(())
    }

    async fn handle_run_failure(
        &self,
        error_message: &str,
        aborted: bool,
    ) -> Result<(), AgentError> {
        let model = self.inner.state.lock().expect("agent state").model.clone();
        let mut failure_message =
            empty_assistant_message(model.api.clone(), model.provider.clone(), model.id.clone());
        failure_message.content = vec![xjpcode_ai::AssistantContentBlock::Text(TextContent::new(
            "",
        ))];
        failure_message.usage = empty_usage();
        failure_message.stop_reason = if aborted {
            StopReason::Aborted
        } else {
            StopReason::Error
        };
        failure_message.error_message = Some(error_message.to_string());
        let failure_message = AgentMessage::Assistant(failure_message);

        self.process_events(AgentEvent::MessageStart {
            message: failure_message.clone(),
        })
        .await?;
        self.process_events(AgentEvent::MessageEnd {
            message: failure_message.clone(),
        })
        .await?;
        self.process_events(AgentEvent::TurnEnd {
            message: failure_message.clone(),
            tool_results: Vec::new(),
        })
        .await?;
        self.process_events(AgentEvent::AgentEnd {
            messages: vec![failure_message],
        })
        .await?;
        Ok(())
    }

    fn event_sink(&self) -> crate::agent_loop::AgentEventSink {
        let agent = self.clone();
        Arc::new(move |event| {
            let agent = agent.clone();
            Box::pin(async move {
                // 监听器只可能在 run 里被调用；出错说明状态已经不对，交给 run 级处置。
                let _ = agent.process_events(event).await;
            })
        })
    }

    /// pi: `processEvents`——先改状态（放锁），再按订阅序逐个 await 监听器。
    async fn process_events(&self, event: AgentEvent) -> Result<(), AgentError> {
        {
            let mut state = self.inner.state.lock().expect("agent state");
            match &event {
                AgentEvent::MessageStart { message } => {
                    state.streaming_message = Some(message.clone());
                }
                AgentEvent::MessageUpdate { message, .. } => {
                    state.streaming_message = Some(message.clone());
                }
                AgentEvent::MessageEnd { message } => {
                    state.streaming_message = None;
                    state.messages.push(message.clone());
                }
                AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                    state.pending_tool_calls.insert(tool_call_id.clone());
                }
                AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                    state.pending_tool_calls.remove(tool_call_id);
                }
                AgentEvent::TurnEnd { message, .. } => {
                    if message.role() == "assistant" {
                        if let AgentMessage::Assistant(message) = message {
                            // 上游按真假值判断：空字符串不算有错误文字（`agent.ts:605`）。
                            if let Some(error_message) = message
                                .error_message
                                .as_ref()
                                .filter(|text| !text.is_empty())
                            {
                                state.error_message = Some(error_message.clone());
                            }
                        }
                    }
                }
                AgentEvent::AgentEnd { .. } => {
                    state.streaming_message = None;
                }
                AgentEvent::AgentStart
                | AgentEvent::TurnStart
                | AgentEvent::ToolExecutionUpdate { .. } => {}
            }
        }

        let signal = self
            .inner
            .active_run
            .lock()
            .expect("active run")
            .as_ref()
            .map(|run| run.signal.clone())
            .ok_or_else(|| AgentError::new("Agent listener invoked outside active run"))?;
        let listeners: Vec<Listener> = self
            .inner
            .listeners
            .lock()
            .expect("listeners")
            .iter()
            .map(|entry| Arc::clone(&entry.listener))
            .collect();
        for listener in listeners {
            listener(&event, &signal).await;
        }
        Ok(())
    }

    // ---- 可读可写的槽位（上游是公开字段） ----

    pub fn convert_to_llm(&self) -> ConvertToLlm {
        self.inner
            .convert_to_llm
            .lock()
            .expect("convert to llm")
            .clone()
    }

    pub fn set_convert_to_llm(&self, convert_to_llm: ConvertToLlm) {
        *self.inner.convert_to_llm.lock().expect("convert to llm") = convert_to_llm;
    }

    pub fn transform_context(&self) -> Option<TransformContext> {
        self.inner
            .transform_context
            .lock()
            .expect("transform context")
            .clone()
    }

    pub fn set_transform_context(&self, transform_context: Option<TransformContext>) {
        *self
            .inner
            .transform_context
            .lock()
            .expect("transform context") = transform_context;
    }

    /// pi: `streamFunction`
    pub fn stream_function(&self) -> StreamFn {
        self.inner
            .stream_function
            .lock()
            .expect("stream function")
            .clone()
    }

    pub fn set_stream_function(&self, stream_function: StreamFn) {
        *self.inner.stream_function.lock().expect("stream function") = stream_function;
    }

    pub fn get_api_key(&self) -> Option<GetApiKey> {
        self.inner.get_api_key.lock().expect("get api key").clone()
    }

    pub fn set_get_api_key(&self, get_api_key: Option<GetApiKey>) {
        *self.inner.get_api_key.lock().expect("get api key") = get_api_key;
    }

    pub fn on_payload(&self) -> Callback<OnPayloadFn> {
        self.inner.on_payload.lock().expect("on payload").clone()
    }

    pub fn set_on_payload(&self, on_payload: Callback<OnPayloadFn>) {
        *self.inner.on_payload.lock().expect("on payload") = on_payload;
    }

    pub fn on_response(&self) -> Callback<OnResponseFn> {
        self.inner.on_response.lock().expect("on response").clone()
    }

    pub fn set_on_response(&self, on_response: Callback<OnResponseFn>) {
        *self.inner.on_response.lock().expect("on response") = on_response;
    }

    pub fn on_provider_stream_event(&self) -> Callback<OnProviderStreamEventFn> {
        self.inner
            .on_provider_stream_event
            .lock()
            .expect("on provider stream event")
            .clone()
    }

    pub fn set_on_provider_stream_event(&self, callback: Callback<OnProviderStreamEventFn>) {
        *self
            .inner
            .on_provider_stream_event
            .lock()
            .expect("on provider stream event") = callback;
    }

    pub fn before_tool_call(&self) -> Option<BeforeToolCall> {
        self.inner
            .before_tool_call
            .lock()
            .expect("before tool call")
            .clone()
    }

    pub fn set_before_tool_call(&self, hook: Option<BeforeToolCall>) {
        *self
            .inner
            .before_tool_call
            .lock()
            .expect("before tool call") = hook;
    }

    pub fn after_tool_call(&self) -> Option<AfterToolCall> {
        self.inner
            .after_tool_call
            .lock()
            .expect("after tool call")
            .clone()
    }

    pub fn set_after_tool_call(&self, hook: Option<AfterToolCall>) {
        *self.inner.after_tool_call.lock().expect("after tool call") = hook;
    }

    pub fn finish_turn(&self) -> Option<FinishTurn> {
        self.inner.finish_turn.lock().expect("finish turn").clone()
    }

    pub fn set_finish_turn(&self, hook: Option<FinishTurn>) {
        *self.inner.finish_turn.lock().expect("finish turn") = hook;
    }

    pub fn prepare_request(&self) -> Option<PrepareRequest> {
        self.inner
            .prepare_request
            .lock()
            .expect("prepare request")
            .clone()
    }

    pub fn set_prepare_request(&self, hook: Option<PrepareRequest>) {
        *self.inner.prepare_request.lock().expect("prepare request") = hook;
    }

    pub fn prepare_next_turn(&self) -> Option<PrepareNextTurnSignal> {
        self.inner
            .prepare_next_turn
            .lock()
            .expect("prepare next turn")
            .clone()
    }

    pub fn set_prepare_next_turn(&self, hook: Option<PrepareNextTurnSignal>) {
        *self
            .inner
            .prepare_next_turn
            .lock()
            .expect("prepare next turn") = hook;
    }

    pub fn prepare_next_turn_with_context(&self) -> Option<PrepareNextTurnWithContext> {
        self.inner
            .prepare_next_turn_with_context
            .lock()
            .expect("prepare next turn with context")
            .clone()
    }

    pub fn set_prepare_next_turn_with_context(&self, hook: Option<PrepareNextTurnWithContext>) {
        *self
            .inner
            .prepare_next_turn_with_context
            .lock()
            .expect("prepare next turn with context") = hook;
    }

    pub fn session_id(&self) -> Option<String> {
        self.inner.session_id.lock().expect("session id").clone()
    }

    pub fn set_session_id(&self, session_id: Option<String>) {
        *self.inner.session_id.lock().expect("session id") = session_id;
    }

    pub fn thinking_budgets(&self) -> Option<ThinkingBudgets> {
        self.inner
            .thinking_budgets
            .lock()
            .expect("thinking budgets")
            .clone()
    }

    pub fn set_thinking_budgets(&self, thinking_budgets: Option<ThinkingBudgets>) {
        *self
            .inner
            .thinking_budgets
            .lock()
            .expect("thinking budgets") = thinking_budgets;
    }

    pub fn transport(&self) -> Transport {
        *self.inner.transport.lock().expect("transport")
    }

    pub fn set_transport(&self, transport: Transport) {
        *self.inner.transport.lock().expect("transport") = transport;
    }

    pub fn max_retry_delay_ms(&self) -> Option<u64> {
        *self
            .inner
            .max_retry_delay_ms
            .lock()
            .expect("max retry delay")
    }

    pub fn set_max_retry_delay_ms(&self, max_retry_delay_ms: Option<u64>) {
        *self
            .inner
            .max_retry_delay_ms
            .lock()
            .expect("max retry delay") = max_retry_delay_ms;
    }

    pub fn tool_execution(&self) -> ToolExecutionMode {
        *self.inner.tool_execution.lock().expect("tool execution")
    }

    pub fn set_tool_execution(&self, tool_execution: ToolExecutionMode) {
        *self.inner.tool_execution.lock().expect("tool execution") = tool_execution;
    }

    pub fn identity(&self) -> Option<ClientIdentity> {
        self.inner.identity.lock().expect("identity").clone()
    }

    pub fn set_identity(&self, identity: Option<ClientIdentity>) {
        *self.inner.identity.lock().expect("identity") = identity;
    }
}

/// pi: `createLoopConfig` 里那段 `prepareNextTurn` 包装——先看带上下文的那个槽位，再看旧形状。
fn prepare_next_turn_wrapper(agent: Agent) -> PrepareNextTurn {
    Arc::new(move |context: &AgentTurnContext<'_>| {
        let agent = agent.clone();
        Box::pin(async move {
            if let Some(with_context) = agent.prepare_next_turn_with_context() {
                let signal = agent.signal();
                return with_context(context, signal.as_ref()).await;
            }
            if let Some(legacy) = agent.prepare_next_turn() {
                let signal = agent.signal();
                return legacy(signal.as_ref()).await;
            }
            None
        })
    })
}

/// `subscribe` 交出的撤销句柄；丢掉它就不再收到事件（上游是一个函数）。
pub struct Subscription {
    inner: Weak<AgentInner>,
    id: u64,
}

impl Subscription {
    /// 上游返回的那个撤销函数：调用即撤销（`Drop` 做同一件事）。
    pub fn unsubscribe(self) {}
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            inner
                .listeners
                .lock()
                .expect("listeners")
                .retain(|entry| entry.id != self.id);
        }
    }
}

/// pi: `normalizePromptInput` 的三种输入。
pub enum PromptInput {
    Text {
        text: String,
        images: Vec<ImageContent>,
    },
    Message(AgentMessage),
    Messages(Vec<AgentMessage>),
}

impl From<&str> for PromptInput {
    fn from(text: &str) -> Self {
        Self::Text {
            text: text.to_string(),
            images: Vec::new(),
        }
    }
}

impl From<String> for PromptInput {
    fn from(text: String) -> Self {
        Self::Text {
            text,
            images: Vec::new(),
        }
    }
}

impl From<AgentMessage> for PromptInput {
    fn from(message: AgentMessage) -> Self {
        Self::Message(message)
    }
}

impl From<Vec<AgentMessage>> for PromptInput {
    fn from(messages: Vec<AgentMessage>) -> Self {
        Self::Messages(messages)
    }
}

fn normalize_prompt_input(input: PromptInput) -> Vec<AgentMessage> {
    match input {
        PromptInput::Messages(messages) => messages,
        PromptInput::Message(message) => vec![message],
        PromptInput::Text { text, images } => {
            let mut content: Vec<UserContentBlock> =
                vec![UserContentBlock::Text(TextContent::new(text))];
            content.extend(images.into_iter().map(UserContentBlock::Image));
            vec![AgentMessage::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Blocks(content),
                timestamp: now_ms(),
            })]
        }
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "run panicked".to_string()
}
