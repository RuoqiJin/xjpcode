//! 对应上游 `src/core/agent-session.ts` 的这一里程碑用到的部分：接着聊、自动压缩、自动重试、
//! 中途插话。扩展、模型路由、分支树、图片、`!` 命令、导出等块没接，清单见
//! `docs/port/milestone-5.md`。
//!
//! 与上游同形的部分：会话文件由 `SessionManager` 追加；每个请求前用会话投影替换上下文；
//! 一轮结束后先看重试、再看压缩、最后看队列；`steer` 在下一步之后投递、`followUp` 等它停下。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use xjpcode_agent::{
    now_ms, Agent, AgentContext, AgentEvent, AgentLoopTurnUpdate, AgentMessage, GetApiKey, StreamFn,
    ThinkingLevel,
};
use xjpcode_ai::utils::overflow::{is_context_overflow, is_recoverable_length};
use xjpcode_ai::utils::retry::{is_retryable_assistant_error, retry_delay_ms, RetryCallbacks};
use xjpcode_ai::{
    content_text, get_current_system_message, AbortController, AssistantContentBlock,
    AssistantMessage, BoxFuture, Message, StopReason, SystemContent, SystemMessage, SystemRole,
    TextContent, ToolResultMessage, UserContent, UserContentBlock, UserMessage, UserRole,
};

use crate::core::compaction::{
    calculate_context_tokens, compact, estimate_context_tokens, estimate_projected_context_tokens,
    estimate_tokens, prepare_compaction, should_compact, CompactionResult,
};
use crate::core::session_manager::{CompactionEntry, SessionEntry, SessionManager};
use crate::core::settings_manager::Settings;
use crate::core::system_prompt::{
    build_system_prompt_sections, diff_system_prompt_sections, BuildSystemPromptOptions,
};

/// pi: `"steer" | "followUp"`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamingBehavior {
    Steer,
    FollowUp,
}

/// pi: `AgentSessionEvent` 的这一里程碑部分。
#[derive(Debug, Clone, PartialEq)]
pub enum AgentSessionEvent {
    AgentStart,
    AgentEnd {
        messages: Vec<AgentMessage>,
        will_retry: bool,
    },
    AgentSettled {
        aborted: bool,
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
        assistant_message_event: xjpcode_ai::AssistantMessageEvent,
    },
    MessageEnd {
        message: AgentMessage,
    },
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: xjpcode_ai::JsonObject,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: xjpcode_ai::JsonObject,
        partial_result: xjpcode_agent::AgentToolResult,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: xjpcode_agent::AgentToolResult,
        is_error: bool,
        duration_ms: Option<u64>,
    },
    QueueUpdate {
        steering: Vec<String>,
        follow_up: Vec<String>,
    },
    AutoRetryStart {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        error_message: String,
    },
    AutoRetryEnd {
        success: bool,
        attempt: u32,
        final_error: Option<String>,
    },
    CompactionStart {
        reason: CompactionReason,
    },
    CompactionEnd {
        reason: CompactionReason,
        result: Option<CompactionResult>,
        aborted: bool,
        will_retry: bool,
        error_message: Option<String>,
    },
    SummarizationRetryScheduled {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        error_message: String,
    },
    SummarizationRetryAttemptStart,
    SummarizationRetryFinished,
}

/// pi: `"manual" | "threshold" | "overflow"`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionReason {
    Manual,
    Threshold,
    Overflow,
}

type Listener = Arc<dyn Fn(&AgentSessionEvent) + Send + Sync>;

struct ListenerEntry {
    id: u64,
    listener: Listener,
}

/// 订阅句柄：撤销或 `Drop` 时移除监听器。
pub struct AgentSessionSubscription {
    inner: Weak<SessionInner>,
    id: u64,
}

impl AgentSessionSubscription {
    pub fn unsubscribe(self) {}
}

impl Drop for AgentSessionSubscription {
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

/// `AgentSessionConfig` 的这一里程碑子集。
pub struct AgentSessionOptions {
    pub agent: Agent,
    pub session_manager: SessionManager,
    pub settings: Settings,
    pub cwd: PathBuf,
    pub stream_fn: StreamFn,
    pub get_api_key: Option<GetApiKey>,
    pub api_key: Option<String>,
    pub prompt_options: BuildSystemPromptOptions,
}

/// pi: `AgentSession`
#[derive(Clone)]
pub struct AgentSession {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    agent: Agent,
    session: Mutex<SessionManager>,
    settings: Settings,
    cwd: PathBuf,
    stream_fn: StreamFn,
    get_api_key: Option<GetApiKey>,
    api_key: Option<String>,
    prompt_options: BuildSystemPromptOptions,
    listeners: Mutex<Vec<ListenerEntry>>,
    next_listener_id: AtomicU64,
    run_active: AtomicBool,
    abort_requested: AtomicBool,
    steering_messages: Mutex<Vec<String>>,
    follow_up_messages: Mutex<Vec<String>>,
    retry_attempt: AtomicU32,
    retry_controller: Mutex<Option<AbortController>>,
    compaction_controller: Mutex<Option<AbortController>>,
    overflow_recovery_attempted: AtomicBool,
    last_assistant: Mutex<Option<AssistantMessage>>,
    entry_ids: Mutex<std::collections::HashMap<String, String>>,
    agent_subscription: Mutex<Option<xjpcode_agent::Subscription>>,
}

/// 消息与条目 id 的对应：Rust 侧没有对象身份，用序列化后的一份做键。
fn message_fingerprint(message: &AgentMessage) -> String {
    serde_json::to_string(message).unwrap_or_default()
}

fn iso_to_ms(timestamp: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|value| value.timestamp_millis().max(0) as u64)
        .unwrap_or(0)
}

fn thinking_level_name(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "off",
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

fn thinking_level_from_name(name: &str) -> ThinkingLevel {
    match name {
        "minimal" => ThinkingLevel::Minimal,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" => ThinkingLevel::High,
        "xhigh" => ThinkingLevel::Xhigh,
        "max" => ThinkingLevel::Max,
        _ => ThinkingLevel::Off,
    }
}

fn user_message_text(message: &AgentMessage) -> Option<String> {
    match message {
        AgentMessage::User(user) => Some(match &user.content {
            UserContent::Text(text) => text.clone(),
            UserContent::Blocks(blocks) => content_text(blocks.as_slice(), ""),
        }),
        _ => None,
    }
}

fn system_messages_of(messages: &[AgentMessage]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|message| match message {
            AgentMessage::System(system) => Some(Message::System(system.clone())),
            _ => None,
        })
        .collect()
}

impl AgentSession {
    /// 建一场会话。会话里已有的消息、思考档会被恢复进 `agent`；之后的消息接着写进同一份文件。
    pub fn new(options: AgentSessionOptions) -> Arc<Self> {
        let AgentSessionOptions {
            agent,
            session_manager,
            settings,
            cwd,
            stream_fn,
            get_api_key,
            api_key,
            prompt_options,
        } = options;
        let context = session_manager.build_session_context();
        let has_existing_session = !context.messages.is_empty();
        // pi 把会话 id 交给 agent（OpenAI 那条路拿它做 prompt_cache_key 与 session-id 头）。
        if agent.session_id().is_none() {
            agent.set_session_id(Some(session_manager.get_session_id().to_string()));
        }
        if has_existing_session {
            agent.set_messages(context.messages);
            agent.set_thinking_level(thinking_level_from_name(&context.thinking_level));
        }
        agent.set_convert_to_llm(Arc::new(|messages: &[AgentMessage]| {
            let converted = crate::core::messages::convert_to_llm(messages);
            Box::pin(async move { converted })
        }));
        let inner = Arc::new(SessionInner {
            agent,
            session: Mutex::new(session_manager),
            settings,
            cwd,
            stream_fn,
            get_api_key,
            api_key,
            prompt_options,
            listeners: Mutex::new(Vec::new()),
            next_listener_id: AtomicU64::new(0),
            run_active: AtomicBool::new(false),
            abort_requested: AtomicBool::new(false),
            steering_messages: Mutex::new(Vec::new()),
            follow_up_messages: Mutex::new(Vec::new()),
            retry_attempt: AtomicU32::new(0),
            retry_controller: Mutex::new(None),
            compaction_controller: Mutex::new(None),
            overflow_recovery_attempted: AtomicBool::new(false),
            last_assistant: Mutex::new(None),
            entry_ids: Mutex::new(std::collections::HashMap::new()),
            agent_subscription: Mutex::new(None),
        });
        // 每个请求前把会话投影换成当前上下文：压缩、重试的省略、上下文编辑都在这里生效。
        let weak = Arc::downgrade(&inner);
        inner.agent.set_prepare_request(Some(Arc::new(
            move |request, _signal| {
                let weak = weak.clone();
                let tools = request.context.tools.clone();
                Box::pin(async move {
                    let Some(inner) = weak.upgrade() else {
                        return None;
                    };
                    let messages = AgentSession { inner }.projection_messages();
                    Some(xjpcode_agent::AgentRequestUpdate {
                        context: Some(AgentContext { messages, tools }),
                        ..Default::default()
                    })
                })
            },
        )));

        // 一轮结束后、下一次请求前：必要时压缩，然后补上变了的系统提示片段。
        let weak = Arc::downgrade(&inner);
        inner
            .agent
            .set_prepare_next_turn_with_context(Some(Arc::new(move |_context, _signal| {
                let weak = weak.clone();
                Box::pin(async move {
                    let inner = weak.upgrade()?;
                    AgentSession { inner }.on_next_turn().await
                })
            })));

        // 内部监听：落盘、队列状态、重试计数、事件转发。句柄挂在会话上，Drop 时撤销。
        let weak = Arc::downgrade(&inner);
        let subscription = inner.agent.subscribe(move |event, _signal| {
            let weak = weak.clone();
            let event = event.clone();
            Box::pin(async move {
                if let Some(inner) = weak.upgrade() {
                    AgentSession { inner }.handle_agent_event(event);
                }
            })
        });
        *inner.agent_subscription.lock().expect("agent subscription") = Some(subscription);
        let session = Arc::new(Self { inner });
        session.seed_entry_ids();
        session
    }

    // =====================================================================
    // 订阅与事件
    // =====================================================================

    pub fn subscribe<F>(&self, listener: F) -> AgentSessionSubscription
    where
        F: Fn(&AgentSessionEvent) + Send + Sync + 'static,
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
        AgentSessionSubscription {
            inner: Arc::downgrade(&self.inner),
            id,
        }
    }

    pub(crate) fn emit(&self, event: &AgentSessionEvent) {
        let listeners: Vec<Listener> = self
            .inner
            .listeners
            .lock()
            .expect("listeners")
            .iter()
            .map(|entry| Arc::clone(&entry.listener))
            .collect();
        for listener in listeners {
            listener(event);
        }
    }

    fn emit_queue_update(&self) {
        self.emit(&AgentSessionEvent::QueueUpdate {
            steering: self
                .inner
                .steering_messages
                .lock()
                .expect("steering")
                .clone(),
            follow_up: self
                .inner
                .follow_up_messages
                .lock()
                .expect("follow-up")
                .clone(),
        });
    }

    fn handle_agent_event(&self, event: AgentEvent) {
        if let AgentEvent::MessageStart { message } = &event {
            if let Some(text) = user_message_text(message) {
                self.inner
                    .overflow_recovery_attempted
                    .store(false, Ordering::SeqCst);
                let removed = {
                    let mut steering = self.inner.steering_messages.lock().expect("steering");
                    match steering.iter().position(|candidate| candidate == &text) {
                        Some(index) => {
                            steering.remove(index);
                            true
                        }
                        None => false,
                    }
                };
                if !removed {
                    let mut follow_up = self
                        .inner
                        .follow_up_messages
                        .lock()
                        .expect("follow-up");
                    if let Some(index) = follow_up.iter().position(|candidate| candidate == &text) {
                        follow_up.remove(index);
                    }
                }
                self.emit_queue_update();
            }
        }

        if let AgentEvent::MessageEnd { message } = &event {
            match message {
                AgentMessage::Custom(custom) => {
                    let custom_type = custom
                        .payload
                        .get("customType")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("custom")
                        .to_string();
                    let content = custom
                        .payload
                        .get("content")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    let display = custom
                        .payload
                        .get("display")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(true);
                    let details = custom.payload.get("details").cloned();
                    let _ = self
                        .inner
                        .session
                        .lock()
                        .expect("session")
                        .append_custom_message_entry(&custom_type, content, display, details);
                }
                AgentMessage::System(_)
                | AgentMessage::User(_)
                | AgentMessage::Assistant(_)
                | AgentMessage::ToolResult(_) => {
                    let appended = self
                        .inner
                        .session
                        .lock()
                        .expect("session")
                        .append_message(message.clone());
                    if let Ok(entry_id) = appended {
                        self.inner
                            .entry_ids
                            .lock()
                            .expect("entry ids")
                            .insert(message_fingerprint(message), entry_id);
                    }
                }
            }

            if let AgentMessage::Assistant(assistant) = message {
                *self.inner.last_assistant.lock().expect("last assistant") =
                    Some(assistant.clone());
                if !matches!(assistant.stop_reason, StopReason::Error | StopReason::Length)
                    && !matches!(assistant.stop_reason, StopReason::Aborted)
                {
                    self.inner
                        .overflow_recovery_attempted
                        .store(false, Ordering::SeqCst);
                }
                let attempt = self.inner.retry_attempt.load(Ordering::SeqCst);
                if !matches!(assistant.stop_reason, StopReason::Error | StopReason::Length)
                    && attempt > 0
                {
                    self.inner.retry_attempt.store(0, Ordering::SeqCst);
                    self.emit(&AgentSessionEvent::AutoRetryEnd {
                        success: true,
                        attempt,
                        final_error: None,
                    });
                }
            }
        }

        let forwarded = match &event {
            AgentEvent::AgentStart => AgentSessionEvent::AgentStart,
            AgentEvent::AgentEnd { messages } => AgentSessionEvent::AgentEnd {
                messages: messages.clone(),
                will_retry: self.would_retry(messages),
            },
            AgentEvent::TurnStart => AgentSessionEvent::TurnStart,
            AgentEvent::TurnEnd {
                message,
                tool_results,
            } => AgentSessionEvent::TurnEnd {
                message: message.clone(),
                tool_results: tool_results.clone(),
            },
            AgentEvent::MessageStart { message } => AgentSessionEvent::MessageStart {
                message: message.clone(),
            },
            AgentEvent::MessageUpdate {
                message,
                assistant_message_event,
            } => AgentSessionEvent::MessageUpdate {
                message: message.clone(),
                assistant_message_event: assistant_message_event.clone(),
            },
            AgentEvent::MessageEnd { message } => AgentSessionEvent::MessageEnd {
                message: message.clone(),
            },
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => AgentSessionEvent::ToolExecutionStart {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                args: args.clone(),
            },
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                args,
                partial_result,
            } => AgentSessionEvent::ToolExecutionUpdate {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                args: args.clone(),
                partial_result: partial_result.clone(),
            },
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
                duration_ms,
            } => AgentSessionEvent::ToolExecutionEnd {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                result: result.clone(),
                is_error: *is_error,
                duration_ms: *duration_ms,
            },
        };
        self.emit(&forwarded);
    }

    fn emit_settled(&self) {
        self.emit(&AgentSessionEvent::AgentSettled {
            aborted: self.inner.abort_requested.load(Ordering::SeqCst),
        });
    }

    // =====================================================================
    // 读状态
    // =====================================================================

    pub fn agent(&self) -> &Agent {
        &self.inner.agent
    }

    pub fn messages(&self) -> Vec<AgentMessage> {
        self.inner.agent.state().messages
    }

    pub fn model(&self) -> xjpcode_ai::Model {
        self.inner.agent.state().model
    }

    pub fn thinking_level(&self) -> ThinkingLevel {
        self.inner.agent.state().thinking_level
    }

    pub fn cwd(&self) -> &Path {
        &self.inner.cwd
    }

    pub fn session_id(&self) -> String {
        self.inner
            .session
            .lock()
            .expect("session")
            .get_session_id()
            .to_string()
    }

    pub fn session_file(&self) -> Option<PathBuf> {
        self.inner
            .session
            .lock()
            .expect("session")
            .get_session_file()
            .map(Path::to_path_buf)
    }

    pub fn session_name(&self) -> Option<String> {
        self.inner.session.lock().expect("session").get_session_name()
    }

    /// pi: `setModel`——换模型，并把这次更换记进会话（界面上的 `/model` 用）。
    ///
    /// 思考档照 pi 的 `_getThinkingLevelForModelSwitch`：模型单独记过的优先，其次是设置里的
    /// 缺省档，最后沿用当前这一档；再按新模型支持的档位夹一次。
    pub fn set_model(&self, model: xjpcode_ai::Model) -> Result<(), String> {
        let level = self
            .inner
            .settings
            .model_thinking_level(&model.provider, &model.id)
            .map(thinking_level_from_name)
            .or_else(|| {
                self.inner
                    .settings
                    .default_thinking_level
                    .as_deref()
                    .map(thinking_level_from_name)
            })
            .unwrap_or_else(|| self.thinking_level());
        let effective = xjpcode_ai::models::clamp_thinking_level(&model, level);
        let previous = self.thinking_level();
        self.inner.agent.set_model(model.clone());
        self.inner
            .session
            .lock()
            .expect("session")
            .append_model_change(&model.provider, &model.id)?;
        self.apply_thinking_level(effective, previous)?;
        Ok(())
    }

    /// 当前模型支持的思考档（pi: `getAvailableThinkingLevels`）。
    pub fn available_thinking_levels(&self) -> Vec<ThinkingLevel> {
        xjpcode_ai::models::get_supported_thinking_levels(&self.model())
    }

    /// pi: `setThinkingLevel`——夹到模型支持的档位，真变了才记进会话。
    pub fn set_thinking_level(&self, level: ThinkingLevel) -> Result<Option<ThinkingLevel>, String> {
        let effective = xjpcode_ai::models::clamp_thinking_level(&self.model(), level);
        let previous = self.thinking_level();
        self.inner.agent.set_thinking_level(effective);
        self.apply_thinking_level(effective, previous)
    }

    fn apply_thinking_level(
        &self,
        effective: ThinkingLevel,
        previous: ThinkingLevel,
    ) -> Result<Option<ThinkingLevel>, String> {
        self.inner.agent.set_thinking_level(effective);
        if effective != previous {
            self.inner
                .session
                .lock()
                .expect("session")
                .append_thinking_level_change(thinking_level_name(effective))?;
            return Ok(Some(effective));
        }
        Ok(None)
    }

    pub fn is_streaming(&self) -> bool {
        self.inner.run_active.load(Ordering::SeqCst)
            || self.inner.agent.state().is_streaming
    }

    pub fn is_compacting(&self) -> bool {
        self.inner
            .compaction_controller
            .lock()
            .expect("compaction")
            .is_some()
    }

    pub fn is_idle(&self) -> bool {
        !self.is_streaming() && !self.is_compacting()
    }

    pub fn is_retrying(&self) -> bool {
        self.inner
            .retry_controller
            .lock()
            .expect("retry")
            .is_some()
    }

    pub fn retry_attempt(&self) -> u32 {
        self.inner.retry_attempt.load(Ordering::SeqCst)
    }

    pub fn pending_message_count(&self) -> usize {
        self.inner.steering_messages.lock().expect("steering").len()
            + self
                .inner
                .follow_up_messages
                .lock()
                .expect("follow-up")
                .len()
    }

    pub fn steering_messages(&self) -> Vec<String> {
        self.inner
            .steering_messages
            .lock()
            .expect("steering")
            .clone()
    }

    pub fn follow_up_messages(&self) -> Vec<String> {
        self.inner
            .follow_up_messages
            .lock()
            .expect("follow-up")
            .clone()
    }

    /// pi: `clearQueue`
    pub fn clear_queue(&self) -> (Vec<String>, Vec<String>) {
        let steering = std::mem::take(
            &mut *self
                .inner
                .steering_messages
                .lock()
                .expect("steering"),
        );
        let follow_up = std::mem::take(
            &mut *self
                .inner
                .follow_up_messages
                .lock()
                .expect("follow-up"),
        );
        self.inner.agent.clear_all_queues();
        self.emit_queue_update();
        (steering, follow_up)
    }

    pub fn projection_messages(&self) -> Vec<AgentMessage> {
        let projection = self
            .inner
            .session
            .lock()
            .expect("session")
            .build_session_projection();
        projection.messages
    }

    fn find_last_assistant(&self) -> Option<AssistantMessage> {
        self.inner
            .agent
            .state()
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                AgentMessage::Assistant(assistant) => Some(assistant.clone()),
                _ => None,
            })
    }

    // =====================================================================
    // 收一句话
    // =====================================================================

    /// pi: `prompt`——空闲时开跑；跑着时按 `behavior` 排队。
    pub async fn prompt(
        &self,
        text: &str,
        behavior: Option<StreamingBehavior>,
    ) -> Result<(), String> {
        if self.is_compacting() {
            return Err(
                "Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry."
                    .to_string(),
            );
        }
        if self.is_streaming() {
            let Some(behavior) = behavior else {
                return Err(
                    "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message."
                        .to_string(),
                );
            };
            return match behavior {
                StreamingBehavior::Steer => self.steer(text),
                StreamingBehavior::FollowUp => self.follow_up(text),
            };
        }
        if self
            .inner
            .run_active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            let Some(behavior) = behavior else {
                return Err(
                    "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message."
                        .to_string(),
                );
            };
            return match behavior {
                StreamingBehavior::Steer => self.steer(text),
                StreamingBehavior::FollowUp => self.follow_up(text),
            };
        }
        self.inner.abort_requested.store(false, Ordering::SeqCst);
        let result = self.run_prompt(text).await;
        self.inner.run_active.store(false, Ordering::SeqCst);
        if self.inner.abort_requested.load(Ordering::SeqCst) {
            self.finish_cancelled_retry();
        }
        self.emit_settled();
        result
    }

    async fn run_prompt(&self, text: &str) -> Result<(), String> {
        // 发之前先看一眼上一条回答：上下文已经顶到上限时先压一次（上游 `_prepareRun`）。
        if let Some(last_assistant) = self.find_last_assistant() {
            self.check_compaction(&last_assistant, false).await;
        }
        let messages = self.prepare_run_messages(text)?;
        self.inner
            .agent
            .prompt(messages)
            .await
            .map_err(|error| error.to_string())?;
        while !self.inner.abort_requested.load(Ordering::SeqCst) {
            if self.handle_post_agent_run().await? {
                if self.inner.abort_requested.load(Ordering::SeqCst) {
                    break;
                }
                self.inner
                    .agent
                    .r#continue()
                    .await
                    .map_err(|error| error.to_string())?;
                continue;
            }
            break;
        }
        Ok(())
    }

    fn prepare_run_messages(&self, text: &str) -> Result<Vec<AgentMessage>, String> {
        let mut messages = Vec::new();
        if let Some(patch) = self.system_prompt_patch()? {
            messages.push(AgentMessage::System(patch));
        }
        messages.push(AgentMessage::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(
                text,
            ))]),
            timestamp: now_ms(),
        }));
        Ok(messages)
    }

    /// 和记录里已有的系统提示逐段比，只发变了的片段。
    fn system_prompt_patch(&self) -> Result<Option<SystemMessage>, String> {
        let messages = self.inner.agent.state().messages;
        let system_messages = system_messages_of(&messages);
        let previous = get_current_system_message(&system_messages)
            .and_then(|message| message.sections)
            .unwrap_or_default();
        let current = build_system_prompt_sections(self.inner.prompt_options.clone())?;
        Ok(diff_system_prompt_sections(&previous, &current).map(|sections| SystemMessage {
            role: SystemRole::System,
            content: SystemContent::Text(String::new()),
            sections: Some(sections),
            tools_added: None,
            tools_removed: None,
            timestamp: now_ms(),
        }))
    }

    async fn on_next_turn(&self) -> Option<AgentLoopTurnUpdate> {
        let model = self.model();
        let settings = self.inner.settings.compaction_settings();
        let projection = self
            .inner
            .session
            .lock()
            .expect("session")
            .build_session_projection();
        let branch = self.inner.session.lock().expect("session").get_branch();
        if settings.enabled
            && model.context_window > 0
            && should_compact(
                estimate_projected_context_tokens(&projection, &branch).tokens,
                model.context_window,
                &settings,
            )
        {
            self.run_auto_compaction(CompactionReason::Threshold, false)
                .await;
        }
        let mut messages = self.projection_messages();
        if let Ok(Some(patch)) = self.system_prompt_patch() {
            messages.push(AgentMessage::System(patch));
        }
        Some(AgentLoopTurnUpdate {
            context: Some(AgentContext {
                messages,
                tools: Some(self.inner.agent.state().tools),
            }),
            ..Default::default()
        })
    }

    async fn handle_post_agent_run(&self) -> Result<bool, String> {
        let message = self
            .inner
            .last_assistant
            .lock()
            .expect("last assistant")
            .take();
        if self.inner.abort_requested.load(Ordering::SeqCst) {
            self.finish_cancelled_retry();
            return Ok(false);
        }
        let Some(message) = message else {
            return Ok(self.inner.agent.has_queued_messages());
        };

        if self.is_retryable_error(&message) && self.prepare_retry(&message).await {
            return Ok(!self.inner.abort_requested.load(Ordering::SeqCst));
        }
        if self.inner.abort_requested.load(Ordering::SeqCst) {
            self.finish_cancelled_retry();
            return Ok(false);
        }

        let attempt = self.inner.retry_attempt.load(Ordering::SeqCst);
        if message.stop_reason == StopReason::Error && attempt > 0 {
            self.inner.retry_attempt.store(0, Ordering::SeqCst);
            self.emit(&AgentSessionEvent::AutoRetryEnd {
                success: false,
                attempt,
                final_error: message.error_message.clone(),
            });
        }

        if self.check_compaction(&message, true).await {
            return Ok(!self.inner.abort_requested.load(Ordering::SeqCst));
        }

        Ok(!self.inner.abort_requested.load(Ordering::SeqCst)
            && self.inner.agent.has_queued_messages())
    }

    // =====================================================================
    // 插话与排队
    // =====================================================================

    /// pi: `steer`——当前这一步做完之后插进去。
    pub fn steer(&self, text: &str) -> Result<(), String> {
        self.inner
            .steering_messages
            .lock()
            .expect("steering")
            .push(text.to_string());
        self.emit_queue_update();
        self.inner
            .agent
            .steer(AgentMessage::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(
                    text,
                ))]),
                timestamp: now_ms(),
            }));
        Ok(())
    }

    /// pi: `followUp`——等它停下再发。
    pub fn follow_up(&self, text: &str) -> Result<(), String> {
        self.inner
            .follow_up_messages
            .lock()
            .expect("follow-up")
            .push(text.to_string());
        self.emit_queue_update();
        self.inner
            .agent
            .follow_up(AgentMessage::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent::new(
                    text,
                ))]),
                timestamp: now_ms(),
            }));
        Ok(())
    }

    // =====================================================================
    // 打断
    // =====================================================================

    /// 只发信号，不等空闲（给事件监听器里用；在监听器里等会自己锁自己）。
    pub fn request_abort(&self) {
        self.inner.abort_requested.store(true, Ordering::SeqCst);
        self.abort_retry();
        self.abort_compaction();
        self.inner.agent.abort();
    }

    /// pi: `abort`——打断当前这一轮并等它真正停下。
    pub async fn abort(&self) {
        self.request_abort();
        self.inner.agent.wait_for_idle().await;
    }

    // =====================================================================
    // 自动重试
    // =====================================================================

    fn is_retryable_error(&self, message: &AssistantMessage) -> bool {
        if is_context_overflow(message, Some(self.model().context_window)) {
            return false;
        }
        is_retryable_assistant_error(message)
    }

    fn would_retry(&self, messages: &[AgentMessage]) -> bool {
        if self.inner.abort_requested.load(Ordering::SeqCst) {
            return false;
        }
        let settings = self.inner.settings.retry_policy();
        if !settings.enabled
            || self.inner.retry_attempt.load(Ordering::SeqCst) >= settings.max_retries
        {
            return false;
        }
        for message in messages.iter().rev() {
            if let AgentMessage::Assistant(assistant) = message {
                return self.is_retryable_error(assistant);
            }
        }
        false
    }

    async fn prepare_retry(&self, message: &AssistantMessage) -> bool {
        let policy = self.inner.settings.retry_policy();
        if !policy.enabled {
            return false;
        }
        let attempt = self.inner.retry_attempt.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt > policy.max_retries {
            self.inner
                .retry_attempt
                .store(attempt - 1, Ordering::SeqCst);
            return false;
        }
        let delay_ms = retry_delay_ms(&policy, attempt);
        self.emit(&AgentSessionEvent::AutoRetryStart {
            attempt,
            max_attempts: policy.max_retries,
            delay_ms,
            error_message: message
                .error_message
                .clone()
                .unwrap_or_else(|| "Unknown error".to_string()),
        });

        self.omit_recovery_attempt(message);

        let controller = AbortController::new();
        let signal = controller.signal();
        *self.inner.retry_controller.lock().expect("retry") = Some(controller);
        let cancelled = tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => false,
            _ = signal.cancelled() => true,
        };
        *self.inner.retry_controller.lock().expect("retry") = None;
        if cancelled {
            self.finish_cancelled_retry();
            return false;
        }
        true
    }

    fn finish_cancelled_retry(&self) {
        let attempt = self.inner.retry_attempt.load(Ordering::SeqCst);
        if attempt == 0 {
            return;
        }
        self.inner.retry_attempt.store(0, Ordering::SeqCst);
        self.emit(&AgentSessionEvent::AutoRetryEnd {
            success: false,
            attempt,
            final_error: Some("Retry cancelled".to_string()),
        });
    }

    pub fn abort_retry(&self) {
        if let Some(controller) = self.inner.retry_controller.lock().expect("retry").as_ref() {
            controller.abort();
        }
    }

    /// 把这次失败的尝试从模型上下文里去掉（原始记录留着）。
    fn omit_recovery_attempt(&self, message: &AssistantMessage) {
        let tool_call_ids: HashSet<String> = message
            .content
            .iter()
            .filter_map(|block| match block {
                AssistantContentBlock::ToolCall(call) => Some(call.id.clone()),
                _ => None,
            })
            .collect();
        let messages = self.inner.agent.state().messages;
        let Some(index) = messages
            .iter()
            .rposition(|candidate| candidate == &AgentMessage::Assistant(message.clone()))
        else {
            return;
        };
        let mut targets: Vec<AgentMessage> = vec![AgentMessage::Assistant(message.clone())];
        for candidate in &messages[index + 1..] {
            if matches!(candidate, AgentMessage::Assistant(_)) {
                break;
            }
            if let AgentMessage::ToolResult(result) = candidate {
                if tool_call_ids.contains(&result.tool_call_id) {
                    targets.push(candidate.clone());
                }
            }
        }
        let ids: Vec<Option<String>> = targets
            .iter()
            .map(|target| self.find_persisted_entry_id(target))
            .collect();
        for id in ids.into_iter().flatten() {
            let _ = self
                .inner
                .session
                .lock()
                .expect("session")
                .append_context_edit(&id, None);
        }
        self.refresh_finalized_context();
    }

    fn find_persisted_entry_id(&self, target: &AgentMessage) -> Option<String> {
        self.inner
            .entry_ids
            .lock()
            .expect("entry ids")
            .get(&message_fingerprint(target))
            .cloned()
    }

    fn seed_entry_ids(&self) {
        let projection = self
            .inner
            .session
            .lock()
            .expect("session")
            .build_session_projection();
        let mut ids = self.inner.entry_ids.lock().expect("entry ids");
        ids.clear();
        for entry in &projection.entries {
            for message in &entry.messages {
                ids.insert(
                    message_fingerprint(message),
                    entry.source_entry.id().to_string(),
                );
            }
        }
    }

    fn refresh_finalized_context(&self) {
        let messages = self.projection_messages();
        self.inner.agent.set_messages(messages);
        self.seed_entry_ids();
    }

    // =====================================================================
    // 自动压缩
    // =====================================================================

    fn latest_compaction(branch: &[SessionEntry]) -> Option<&CompactionEntry> {
        branch.iter().rev().find_map(|entry| match entry {
            SessionEntry::Compaction(compaction) => Some(compaction),
            _ => None,
        })
    }

    async fn check_compaction(&self, message: &AssistantMessage, skip_aborted: bool) -> bool {
        let settings = self.inner.settings.compaction_settings();
        if !settings.enabled {
            return false;
        }
        if skip_aborted && message.stop_reason == StopReason::Aborted {
            return false;
        }

        let branch = self.inner.session.lock().expect("session").get_branch();
        if let Some(compaction) = Self::latest_compaction(&branch) {
            if message.timestamp <= iso_to_ms(&compaction.base.timestamp) {
                return false;
            }
        }

        let model = self.model();
        let context_window = model.context_window;
        let explicit_overflow = is_context_overflow(message, Some(context_window));
        let recoverable_length = is_recoverable_length(message, model.max_tokens);
        if explicit_overflow || recoverable_length {
            let will_retry = message.stop_reason != StopReason::Stop;
            if !will_retry {
                return self
                    .run_auto_compaction(CompactionReason::Overflow, false)
                    .await;
            }
            if self
                .inner
                .overflow_recovery_attempted
                .swap(true, Ordering::SeqCst)
            {
                let error_message = if explicit_overflow {
                    "Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model.".to_string()
                } else {
                    "Truncated response recovery failed after one compact-and-retry attempt."
                        .to_string()
                };
                self.emit(&AgentSessionEvent::CompactionEnd {
                    reason: CompactionReason::Overflow,
                    result: None,
                    aborted: false,
                    will_retry: false,
                    error_message: Some(error_message),
                });
                return false;
            }
            self.omit_recovery_attempt(message);
            return self
                .run_auto_compaction(CompactionReason::Overflow, will_retry)
                .await;
        }

        let has_context_edits = branch
            .iter()
            .any(|entry| matches!(entry, SessionEntry::ContextEdit(_)));
        let direct_context_tokens = calculate_context_tokens(&message.usage);
        let context_tokens = if has_context_edits {
            let projection = self
                .inner
                .session
                .lock()
                .expect("session")
                .build_session_projection();
            estimate_projected_context_tokens(&projection, &branch).tokens
        } else if message.stop_reason == StopReason::Error || direct_context_tokens == 0 {
            estimate_context_tokens(&self.inner.agent.state().messages).tokens
        } else {
            direct_context_tokens
        };
        if should_compact(context_tokens, context_window, &settings) {
            return self
                .run_auto_compaction(CompactionReason::Threshold, false)
                .await;
        }
        false
    }

    async fn run_auto_compaction(&self, reason: CompactionReason, will_retry: bool) -> bool {
        if self.is_compacting() {
            return false;
        }
        let settings = self.inner.settings.compaction_settings();
        let branch = self.inner.session.lock().expect("session").get_branch();
        let Some(preparation) = prepare_compaction(&branch, settings) else {
            return false;
        };

        let controller = AbortController::new();
        let signal = controller.signal();
        *self.inner.compaction_controller.lock().expect("compaction") = Some(controller);
        self.emit(&AgentSessionEvent::CompactionStart { reason });

        let result = self
            .run_default_compaction(&preparation, None, Some(signal.clone()), reason)
            .await;
        match result {
            Ok(result) => {
                {
                    let mut session = self.inner.session.lock().expect("session");
                    if session
                        .append_compaction(
                            &result.summary,
                            Some(&result.first_kept_entry_id),
                            result.tokens_before,
                            result.details.clone(),
                            Some(false),
                            result.usage.clone(),
                        )
                        .is_err()
                    {
                        self.clear_compaction_controller();
                        return false;
                    }
                }
                self.refresh_finalized_context();
                let estimated_tokens_after = self
                    .projection_messages()
                    .iter()
                    .map(estimate_tokens)
                    .sum::<u64>();
                let mut result = result;
                result.estimated_tokens_after = Some(estimated_tokens_after);
                self.emit(&AgentSessionEvent::CompactionEnd {
                    reason,
                    result: Some(result),
                    aborted: false,
                    will_retry,
                    error_message: None,
                });
                self.clear_compaction_controller();
                if will_retry {
                    true
                } else {
                    self.inner.agent.has_queued_messages()
                }
            }
            Err(error) => {
                let aborted = signal.aborted();
                self.emit(&AgentSessionEvent::CompactionEnd {
                    reason,
                    result: None,
                    aborted,
                    will_retry: false,
                    error_message: if aborted {
                        None
                    } else if reason == CompactionReason::Overflow {
                        Some(format!("Context overflow recovery failed: {error}"))
                    } else {
                        Some(format!("Auto-compaction failed: {error}"))
                    },
                });
                self.clear_compaction_controller();
                false
            }
        }
    }

    fn clear_compaction_controller(&self) {
        *self.inner.compaction_controller.lock().expect("compaction") = None;
    }

    pub fn abort_compaction(&self) {
        if let Some(controller) = self
            .inner
            .compaction_controller
            .lock()
            .expect("compaction")
            .as_ref()
        {
            controller.abort();
        }
    }

    async fn run_default_compaction(
        &self,
        preparation: &crate::core::compaction::CompactionPreparation,
        custom_instructions: Option<&str>,
        signal: Option<xjpcode_ai::AbortSignal>,
        reason: CompactionReason,
    ) -> Result<CompactionResult, String> {
        let model = self.model();
        let api_key = match &self.inner.get_api_key {
            Some(get_api_key) => get_api_key(&model.provider).await,
            None => self.inner.api_key.clone(),
        };
        let policy = self.inner.settings.retry_policy();
        let callbacks = self.summarization_callbacks(reason);
        compact(
            preparation,
            model,
            api_key,
            None,
            custom_instructions,
            signal,
            Some(self.thinking_level()),
            &self.inner.stream_fn,
            None,
            Some(&policy),
            &callbacks,
            Some(self.session_id()),
        )
        .await
    }

    fn summarization_callbacks(&self, _reason: CompactionReason) -> RetryCallbacks {
        let weak = Arc::downgrade(&self.inner);
        let scheduled = weak.clone();
        let started = weak.clone();
        let finished = weak.clone();
        RetryCallbacks {
            on_retry_scheduled: Some(Arc::new(move |attempt, max_attempts, delay_ms, error| {
                if let Some(inner) = scheduled.upgrade() {
                    AgentSession { inner }.emit(
                        &AgentSessionEvent::SummarizationRetryScheduled {
                            attempt,
                            max_attempts,
                            delay_ms,
                            error_message: error,
                        },
                    );
                }
                Box::pin(async {}) as BoxFuture<'static, ()>
            })),
            on_retry_attempt_start: Some(Arc::new(move || {
                if let Some(inner) = started.upgrade() {
                    AgentSession { inner }
                        .emit(&AgentSessionEvent::SummarizationRetryAttemptStart);
                }
                Box::pin(async {}) as BoxFuture<'static, ()>
            })),
            on_retry_finished: Some(Arc::new(move |_success, _attempt, _final_error| {
                if let Some(inner) = finished.upgrade() {
                    AgentSession { inner }
                        .emit(&AgentSessionEvent::SummarizationRetryFinished);
                }
                Box::pin(async {}) as BoxFuture<'static, ()>
            })),
        }
    }

    /// pi: `compact`——手动压缩（`/compact`）。先打断当前这一轮，再生成摘要并写进记录。
    pub async fn compact(&self, custom_instructions: Option<&str>) -> Result<CompactionResult, String> {
        self.abort().await;
        let controller = AbortController::new();
        let signal = controller.signal();
        *self.inner.compaction_controller.lock().expect("compaction") = Some(controller);
        self.emit(&AgentSessionEvent::CompactionStart {
            reason: CompactionReason::Manual,
        });

        let settings = self.inner.settings.compaction_settings();
        let branch = self.inner.session.lock().expect("session").get_branch();
        let preparation = match prepare_compaction(&branch, settings) {
            Some(preparation) => preparation,
            None => {
                self.clear_compaction_controller();
                let error = if matches!(branch.last(), Some(SessionEntry::Compaction(_))) {
                    "Already compacted"
                } else {
                    "Nothing to compact (session too small)"
                };
                self.emit(&AgentSessionEvent::CompactionEnd {
                    reason: CompactionReason::Manual,
                    result: None,
                    aborted: false,
                    will_retry: false,
                    error_message: Some(error.to_string()),
                });
                return Err(error.to_string());
            }
        };

        let result = self
            .run_default_compaction(
                &preparation,
                custom_instructions,
                Some(signal.clone()),
                CompactionReason::Manual,
            )
            .await;
        match result {
            Ok(result) => {
                self.inner
                    .session
                    .lock()
                    .expect("session")
                    .append_compaction(
                        &result.summary,
                        Some(&result.first_kept_entry_id),
                        result.tokens_before,
                        result.details.clone(),
                        Some(false),
                        result.usage.clone(),
                    )?;
                self.refresh_finalized_context();
                let estimated_tokens_after = self
                    .projection_messages()
                    .iter()
                    .map(estimate_tokens)
                    .sum::<u64>();
                let mut result = result;
                result.estimated_tokens_after = Some(estimated_tokens_after);
                self.emit(&AgentSessionEvent::CompactionEnd {
                    reason: CompactionReason::Manual,
                    result: Some(result.clone()),
                    aborted: false,
                    will_retry: false,
                    error_message: None,
                });
                self.clear_compaction_controller();
                Ok(result)
            }
            Err(error) => {
                let aborted = signal.aborted();
                self.emit(&AgentSessionEvent::CompactionEnd {
                    reason: CompactionReason::Manual,
                    result: None,
                    aborted,
                    will_retry: false,
                    error_message: if aborted {
                        None
                    } else {
                        Some(format!("Compaction failed: {error}"))
                    },
                });
                self.clear_compaction_controller();
                Err(if aborted {
                    "Compaction cancelled".to_string()
                } else {
                    error
                })
            }
        }
    }

    /// pi: `getContextUsage`——只在这一里程碑的界面/测试里用。
    pub fn context_usage(&self) -> Option<(u64, u64)> {
        let model = self.model();
        if model.context_window == 0 {
            return None;
        }
        let branch = self.inner.session.lock().expect("session").get_branch();
        let projection = self
            .inner
            .session
            .lock()
            .expect("session")
            .build_session_projection();
        Some((
            estimate_projected_context_tokens(&projection, &branch).tokens,
            model.context_window,
        ))
    }

}
