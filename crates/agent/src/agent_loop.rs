//! 对应上游 `packages/agent/src/agent-loop.ts`。
//!
//! 循环里每个 `await` 都对上游的一个 `await`（`docs/port/agent-contract.md` 第十节那张表）。
//! 与上游形状不同的地方只有语言逼出来的那些：钩子是 boxed-future 闭包、取消是 `AbortSignal`、
//! 并行批在同任务里 `join_all`、工具只保留擦除的一层（见 `PORTING.md` 差异表 #75）。

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use futures::future::join_all;
use futures::FutureExt;
use xjpcode_ai::{
    get_current_tools, get_tool_state_changes, merge_client_identity, normalize_context,
    to_tool_declaration, validate_tool_arguments, AbortSignal, AssistantMessage,
    AssistantMessageEvent, AssistantMessageEventStream, ClientIdentity, Context, EventStream,
    JsonValue, Message, SystemContent, SystemMessage, SystemRole, TextContent, Tool, ToolCall,
    ToolResultContentBlock, ToolResultMessage, ToolResultRole, PI_ATTRIBUTION,
};

use crate::stream_fn::get_default_stream_fn;
use crate::types::{
    now_ms, reasoning_from_thinking_level, AfterToolCallContext, AgentContext, AgentError,
    AgentEvent, AgentLoopConfig, AgentMessage, AgentTool, AgentToolCallOutcome, AgentToolResult,
    AgentToolUpdateCallback, AgentTurnContext, BeforeToolCallContext, BeforeToolCallResult,
    PrepareRequestContext, StreamFn, ThinkingLevel, ToolError, ToolExecutionMode,
};

/// pi: `AgentEventSink`
pub type AgentEventSink =
    Arc<dyn Fn(AgentEvent) -> xjpcode_ai::BoxFuture<'static, ()> + Send + Sync>;

/// pi: `DEFAULT_AGENT_IDENTITY`——pi-ai 的缺省加上对 pi 的归属。
///
/// 上游是模块级常量对象；Rust 的 `String` 不能出现在 `const` 里，所以用 `LazyLock`（与
/// `PI_ATTRIBUTION` 同一套做法）。
pub static DEFAULT_AGENT_IDENTITY: LazyLock<ClientIdentity> = LazyLock::new(|| ClientIdentity {
    user_agent: None,
    attribution: Some(Some(PI_ATTRIBUTION.clone())),
    version: None,
});

/// pi: `agentLoop`——带一批提示消息开始一次循环。
///
/// 上游在同一个线程上 `void runAgentLoop(...)` 然后把结果交给 `stream.end`；Rust 侧用一个任务
/// 跑同一件事（`PORTING.md` 差异表 #75）。默认流函数在循环里解析（和上游一样），解析失败时流
/// 直接结束、`result()` 一直等下去——与上游那个不会 settle 的 Promise 同义。
pub fn agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> EventStream<AgentEvent, Vec<AgentMessage>> {
    let stream = create_agent_stream();
    let task_stream = stream.clone();
    let emit = event_sink(&stream);
    tokio::spawn(async move {
        if let Ok(messages) =
            run_agent_loop(prompts, context, config, emit, signal, stream_fn).await
        {
            task_stream.end(Some(messages));
        }
    });
    stream
}

/// pi: `agentLoopContinue`——从现有上下文接着跑，不加新消息。
///
/// 上游在校验失败时同步抛错；这里同步返回 `Err`。
pub fn agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Result<EventStream<AgentEvent, Vec<AgentMessage>>, AgentError> {
    validate_continuation(&context)?;
    let stream = create_agent_stream();
    let task_stream = stream.clone();
    let emit = event_sink(&stream);
    tokio::spawn(async move {
        if let Ok(messages) =
            run_agent_loop_continue(context, config, emit, signal, stream_fn).await
        {
            task_stream.end(Some(messages));
        }
    });
    Ok(stream)
}

/// pi: `runAgentLoop`
pub async fn run_agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Result<Vec<AgentMessage>, AgentError> {
    let initial_messages = declare_tool_changes(&context, prompts);
    let mut new_messages = initial_messages.clone();
    let mut current_context = AgentContext {
        messages: context
            .messages
            .iter()
            .chain(initial_messages.iter())
            .cloned()
            .collect(),
        tools: context.tools.clone(),
    };

    emit(AgentEvent::AgentStart).await;
    emit(AgentEvent::TurnStart).await;
    for message in &initial_messages {
        emit(AgentEvent::MessageStart {
            message: message.clone(),
        })
        .await;
        emit(AgentEvent::MessageEnd {
            message: message.clone(),
        })
        .await;
    }

    let stream_function = match stream_fn {
        Some(stream_function) => stream_function,
        None => get_default_stream_fn()?,
    };
    run_loop(
        &mut current_context,
        &mut new_messages,
        config,
        signal,
        &emit,
        &stream_function,
    )
    .await?;
    Ok(new_messages)
}

/// pi: `runAgentLoopContinue`
pub async fn run_agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    signal: Option<AbortSignal>,
    stream_fn: Option<StreamFn>,
) -> Result<Vec<AgentMessage>, AgentError> {
    validate_continuation(&context)?;
    let mut new_messages: Vec<AgentMessage> = Vec::new();
    let mut current_context = context;

    emit(AgentEvent::AgentStart).await;
    emit(AgentEvent::TurnStart).await;

    let stream_function = match stream_fn {
        Some(stream_function) => stream_function,
        None => get_default_stream_fn()?,
    };
    run_loop(
        &mut current_context,
        &mut new_messages,
        config,
        signal,
        &emit,
        &stream_function,
    )
    .await?;
    Ok(new_messages)
}

fn validate_continuation(context: &AgentContext) -> Result<(), AgentError> {
    if context.messages.is_empty() {
        return Err(AgentError::new("Cannot continue: no messages in context"));
    }
    if context.messages[context.messages.len() - 1].role() == "assistant" {
        return Err(AgentError::new(
            "Cannot continue from message role: assistant",
        ));
    }
    Ok(())
}

fn create_agent_stream() -> EventStream<AgentEvent, Vec<AgentMessage>> {
    EventStream::new(
        |event| matches!(event, AgentEvent::AgentEnd { .. }),
        |event| match event {
            AgentEvent::AgentEnd { messages } => messages.clone(),
            _ => Vec::new(),
        },
    )
}

fn event_sink(stream: &EventStream<AgentEvent, Vec<AgentMessage>>) -> AgentEventSink {
    let stream = stream.clone();
    Arc::new(move |event| {
        stream.push(event);
        Box::pin(async {})
    })
}

/// 上一轮结束时的快照，`prepareNextTurn` 与 `finishTurn` 共用。
///
/// 上游把整个 `AgentTurnContext` 存下来（`context`/`newMessages` 是活的引用）；这里只留这一轮
/// 自己的两份消息，调用时再从循环当下的 `current_context` / `new_messages` 现取——两次调用之间
/// 循环不会改这两份东西（`docs/port/agent-contract.md` 第十节）。
struct CompletedTurn {
    message: AssistantMessage,
    tool_results: Vec<ToolResultMessage>,
}

impl CompletedTurn {
    fn context<'a>(
        &'a self,
        context: &'a AgentContext,
        new_messages: &'a [AgentMessage],
    ) -> AgentTurnContext<'a> {
        AgentTurnContext {
            message: &self.message,
            tool_results: &self.tool_results,
            context,
            new_messages,
        }
    }
}

/// pi: `runLoop`
async fn run_loop(
    current_context: &mut AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    initial_config: AgentLoopConfig,
    signal: Option<AbortSignal>,
    emit: &AgentEventSink,
    stream_function: &StreamFn,
) -> Result<(), AgentError> {
    let mut config = initial_config;
    let mut last_completed_turn: Option<CompletedTurn> = None;
    let mut explicit_continuation = false;
    // 开始前先看一眼 steering：用户可能在等上一步的时候就打了字。
    let mut pending_messages = poll_messages(&config.get_steering_messages).await;

    loop {
        let mut has_more_tool_calls = true;

        while has_more_tool_calls || !pending_messages.is_empty() {
            let mut prepared_messages: Vec<AgentMessage> = Vec::new();
            if let Some(last_completed_turn) = &last_completed_turn {
                if let Some(prepare_next_turn) = config.prepare_next_turn.clone() {
                    let turn = last_completed_turn.context(current_context, new_messages);
                    if let Some(update) = prepare_next_turn(&turn).await {
                        if let Some(context) = update.context {
                            *current_context = context;
                        }
                        prepared_messages = update.messages.unwrap_or_default();
                        if let Some(model) = update.model {
                            config.model = model;
                        }
                        config.reasoning = match update.thinking_level {
                            None => config.reasoning,
                            Some(level) => reasoning_from_thinking_level(level),
                        };
                    }
                }
                // 准备可能很慢（例如压缩）。它跑的时候排进来的 steering 要在这时取走；
                // 只有前一次轮询什么都没拿到时才再轮询，否则 one-at-a-time 会一次投两条。
                if pending_messages.is_empty() {
                    pending_messages = poll_messages(&config.get_steering_messages).await;
                }
                emit(AgentEvent::TurnStart).await;
            }

            // 先把准备好的消息与队列里的消息发出去，再去要下一条 assistant 响应。
            let queued: Vec<AgentMessage> = prepared_messages
                .into_iter()
                .chain(std::mem::take(&mut pending_messages))
                .collect();
            for message in declare_tool_changes(current_context, queued) {
                emit(AgentEvent::MessageStart {
                    message: message.clone(),
                })
                .await;
                emit(AgentEvent::MessageEnd {
                    message: message.clone(),
                })
                .await;
                current_context.messages.push(message.clone());
                new_messages.push(message);
            }
            if let Some(prepare_request) = config.prepare_request.clone() {
                let update = {
                    let request = PrepareRequestContext {
                        context: current_context,
                        model: &config.model,
                        thinking_level: thinking_level(&config),
                    };
                    prepare_request(&request, signal.as_ref()).await
                };
                if let Some(update) = update {
                    if let Some(context) = update.context {
                        *current_context = context;
                    }
                    if let Some(model) = update.model {
                        config.model = model;
                    }
                    config.reasoning = match update.thinking_level {
                        None => config.reasoning,
                        Some(level) => reasoning_from_thinking_level(level),
                    };
                }
            }

            let message = stream_assistant_response(
                current_context,
                &config,
                signal.as_ref(),
                emit,
                stream_function,
            )
            .await?;
            new_messages.push(AgentMessage::Assistant(message.clone()));

            if message.stop_reason == xjpcode_ai::StopReason::Error
                || message.stop_reason == xjpcode_ai::StopReason::Aborted
            {
                let completed = CompletedTurn {
                    message: message.clone(),
                    tool_results: Vec::new(),
                };
                if let Some(finish_turn) = config.finish_turn.clone() {
                    let turn = completed.context(current_context, new_messages);
                    let _ = finish_turn(&turn, signal.as_ref()).await;
                }
                emit(AgentEvent::TurnEnd {
                    message: AgentMessage::Assistant(message),
                    tool_results: Vec::new(),
                })
                .await;
                emit(AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                })
                .await;
                return Ok(());
            }

            let tool_calls: Vec<ToolCall> = message
                .content
                .iter()
                .filter_map(|content| match content {
                    xjpcode_ai::AssistantContentBlock::ToolCall(tool_call) => {
                        Some(tool_call.clone())
                    }
                    _ => None,
                })
                .collect();

            let mut tool_results: Vec<ToolResultMessage> = Vec::new();
            has_more_tool_calls = false;
            if !tool_calls.is_empty() {
                // `length` 表示输出被 token 上限截断：这条消息里的每个工具调用都可能带着截断的
                // 参数，全部报错，别去执行可能坏掉的调用。
                let executed = if message.stop_reason == xjpcode_ai::StopReason::Length {
                    fail_tool_calls_from_truncated_message(&tool_calls, emit).await
                } else {
                    execute_tool_calls(
                        current_context,
                        &message,
                        &tool_calls,
                        &config,
                        signal.as_ref(),
                        emit,
                    )
                    .await?
                };
                tool_results.extend(executed.messages);
                has_more_tool_calls = !executed.terminate;

                for result in &tool_results {
                    current_context
                        .messages
                        .push(AgentMessage::ToolResult(result.clone()));
                    new_messages.push(AgentMessage::ToolResult(result.clone()));
                }
            }

            let completed = CompletedTurn {
                message: message.clone(),
                tool_results: tool_results.clone(),
            };
            let decision = if let Some(finish_turn) = config.finish_turn.clone() {
                let turn = completed.context(current_context, new_messages);
                finish_turn(&turn, signal.as_ref()).await
            } else {
                None
            };
            last_completed_turn = Some(completed);
            emit(AgentEvent::TurnEnd {
                message: AgentMessage::Assistant(message),
                tool_results,
            })
            .await;

            if decision == Some(crate::types::AgentTurnDecision::End) {
                emit(AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                })
                .await;
                return Ok(());
            }

            explicit_continuation = decision == Some(crate::types::AgentTurnDecision::Continue);
            pending_messages = poll_messages(&config.get_steering_messages).await;
            if has_more_tool_calls || !pending_messages.is_empty() {
                explicit_continuation = false;
            }
        }

        // 这里本来该停了，看看有没有 follow-up 消息。
        let follow_up_messages = poll_messages(&config.get_follow_up_messages).await;
        if !follow_up_messages.is_empty() {
            explicit_continuation = false;
            pending_messages = follow_up_messages;
            continue;
        }

        // 没有天然的请求能顶替这个决定：用一次只有上下文的请求把它兑现。
        if explicit_continuation {
            explicit_continuation = false;
            continue;
        }

        break;
    }

    emit(AgentEvent::AgentEnd {
        messages: new_messages.clone(),
    })
    .await;
    Ok(())
}
/// 轮询一个消息队列；没配就是空的（上游的 `(await config.getX?.()) || []`）。
async fn poll_messages(poll: &Option<crate::types::GetSteeringMessages>) -> Vec<AgentMessage> {
    match poll {
        Some(poll) => poll().await,
        None => Vec::new(),
    }
}

/// 这次请求的思考等级（上游的 `config.reasoning ?? "off"`）。
fn thinking_level(config: &AgentLoopConfig) -> ThinkingLevel {
    config
        .reasoning
        .map(crate::types::thinking_level_from_reasoning)
        .unwrap_or(ThinkingLevel::Off)
}
/// pi: `declareToolChanges`
///
/// `context.tools` 是运行时能执行的工具；记录里的系统消息声明的是模型能调用的工具。请求之前把两者
/// 的差写成一条系统消息的 `toolsAdded`/`toolsRemoved`。已经有待发的系统消息时，它的工具字段当作
/// 意图，换成「已提交的记录」与「可执行集合」之间的差，于是重放总得到恰好 `context.tools`。
fn declare_tool_changes(
    context: &AgentContext,
    pending_messages: Vec<AgentMessage>,
) -> Vec<AgentMessage> {
    // 待发消息里最后一条系统消息：它的工具字段只是意图，下面换成真实的差。
    let pending = pending_messages
        .iter()
        .rposition(|message| message.role() == "system")
        .and_then(|index| match &pending_messages[index] {
            AgentMessage::System(message) => Some((index, message.clone())),
            _ => None,
        });
    let mut baseline = pending_messages.clone();
    if let Some((index, message)) = &pending {
        baseline[*index] = AgentMessage::System(with_tool_changes(message, &NO_CHANGES));
    }

    let declared_messages: Vec<Message> = context
        .messages
        .iter()
        .chain(baseline.iter())
        .filter_map(system_message_of)
        .collect();
    let current_tools: Vec<Tool> = context
        .tools
        .iter()
        .flatten()
        .map(|tool| to_tool_declaration(&tool.declaration()))
        .collect();
    let changes = get_tool_state_changes(&get_current_tools(&declared_messages), &current_tools);
    let unchanged = changes.tools_added.is_empty() && changes.tools_removed.is_empty();

    if let Some((index, message)) = pending {
        let declares_nothing = message.tools_added.as_ref().is_none_or(Vec::is_empty)
            && message.tools_removed.as_ref().is_none_or(Vec::is_empty);
        if unchanged && declares_nothing {
            return pending_messages;
        }
        baseline[index] = AgentMessage::System(with_tool_changes(&message, &changes));
        return baseline;
    }
    if unchanged {
        return pending_messages;
    }

    let update = with_tool_changes(
        &SystemMessage {
            role: SystemRole::System,
            content: SystemContent::Text(String::new()),
            sections: None,
            tools_added: None,
            tools_removed: None,
            timestamp: now_ms(),
        },
        &changes,
    );
    let insert_index = pending_messages
        .iter()
        .position(|message| message.role() != "system")
        .unwrap_or(pending_messages.len());
    let mut result = pending_messages;
    result.insert(insert_index, AgentMessage::System(update));
    result
}

static NO_CHANGES: LazyLock<xjpcode_ai::ToolStateChanges> =
    LazyLock::new(xjpcode_ai::ToolStateChanges::default);

/// 复制一条系统消息，把工具字段换成 `changes`；空的列表不写这个字段。
fn with_tool_changes(
    message: &SystemMessage,
    changes: &xjpcode_ai::ToolStateChanges,
) -> SystemMessage {
    let mut next = message.clone();
    next.tools_added = (!changes.tools_added.is_empty()).then(|| changes.tools_added.clone());
    next.tools_removed = (!changes.tools_removed.is_empty()).then(|| changes.tools_removed.clone());
    next
}

fn system_message_of(message: &AgentMessage) -> Option<Message> {
    match message {
        AgentMessage::System(message) => Some(Message::System(message.clone())),
        _ => None,
    }
}

/// pi: `streamAssistantResponse`——把 `AgentMessage[]` 变成模型要的 `Message[]`，读回来一个响应。
async fn stream_assistant_response(
    context: &mut AgentContext,
    config: &AgentLoopConfig,
    signal: Option<&AbortSignal>,
    emit: &AgentEventSink,
    stream_function: &StreamFn,
) -> Result<AssistantMessage, AgentError> {
    // 有 transformContext 就先用它（AgentMessage[] → AgentMessage[]）。
    let messages: Vec<AgentMessage> = match &config.transform_context {
        Some(transform) => transform(&context.messages, signal).await,
        None => context.messages.clone(),
    };

    // 换成模型认识的消息（AgentMessage[] → Message[]）。
    let llm_messages = (config.convert_to_llm)(&messages).await;
    let llm_context = normalize_context(Context {
        system_prompt: None,
        messages: llm_messages,
        tools: None,
    });

    // 现取钥匙（OAuth 令牌可能在很长的工具阶段里过期）。
    let resolved_api_key = match &config.get_api_key {
        Some(get_api_key) => get_api_key(&config.model.provider).await,
        None => None,
    }
    .filter(|api_key| !api_key.is_empty())
    .or_else(|| config.api_key.clone());

    let identity =
        merge_client_identity(&[Some(&DEFAULT_AGENT_IDENTITY), config.identity.as_ref()]);
    let options = config.to_stream_options(resolved_api_key, identity, signal.cloned());

    // 上游的契约是「不得抛错」；真抛了（Rust 里是 panic）就在这里接住，交给 run 级处置。
    let response: AssistantMessageEventStream = std::panic::catch_unwind(AssertUnwindSafe(|| {
        stream_function(config.model.clone(), llm_context, options)
    }))
    .map_err(|payload| AgentError::new(panic_message(payload, "stream function panicked")))?;

    let mut added_partial = false;
    let mut consumer = response.consumer();

    while let Some(event) = consumer.next().await {
        match &event {
            AssistantMessageEvent::Start { partial } => {
                let message = AgentMessage::Assistant(partial.clone());
                context.messages.push(message.clone());
                added_partial = true;
                emit(AgentEvent::MessageStart { message }).await;
            }
            AssistantMessageEvent::TextStart { partial, .. }
            | AssistantMessageEvent::TextDelta { partial, .. }
            | AssistantMessageEvent::TextEnd { partial, .. }
            | AssistantMessageEvent::ThinkingStart { partial, .. }
            | AssistantMessageEvent::ThinkingDelta { partial, .. }
            | AssistantMessageEvent::ThinkingEnd { partial, .. }
            | AssistantMessageEvent::ToolcallStart { partial, .. }
            | AssistantMessageEvent::ToolcallDelta { partial, .. }
            | AssistantMessageEvent::ToolcallEnd { partial, .. } => {
                if added_partial {
                    let message = AgentMessage::Assistant(partial.clone());
                    if let Some(last) = context.messages.last_mut() {
                        *last = message.clone();
                    }
                    emit(AgentEvent::MessageUpdate {
                        message,
                        assistant_message_event: event,
                    })
                    .await;
                }
            }
            AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. } => {
                let final_message = response.result().await;
                return finish_response(context, config, emit, final_message, added_partial).await;
            }
        }
    }

    // 流没有给终局事件就结束了：上游在这里同样按最终结果收尾。
    let final_message = response.result().await;
    finish_response(context, config, emit, final_message, added_partial).await
}

async fn finish_response(
    context: &mut AgentContext,
    config: &AgentLoopConfig,
    emit: &AgentEventSink,
    mut final_message: AssistantMessage,
    added_partial: bool,
) -> Result<AssistantMessage, AgentError> {
    // 记下这次请求的等级，不管流是谁答的。
    final_message.thinking_level = Some(thinking_level(config));
    if added_partial {
        if let Some(last) = context.messages.last_mut() {
            *last = AgentMessage::Assistant(final_message.clone());
        }
    } else {
        context
            .messages
            .push(AgentMessage::Assistant(final_message.clone()));
        emit(AgentEvent::MessageStart {
            message: AgentMessage::Assistant(final_message.clone()),
        })
        .await;
    }
    emit(AgentEvent::MessageEnd {
        message: AgentMessage::Assistant(final_message.clone()),
    })
    .await;
    Ok(final_message)
}

/// pi: `failToolCallsFromTruncatedMessage`
async fn fail_tool_calls_from_truncated_message(
    tool_calls: &[ToolCall],
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut messages: Vec<ToolResultMessage> = Vec::new();
    for tool_call in tool_calls {
        emit_tool_execution_start(tool_call, emit).await;
        let finalized = unexecuted(
            tool_call,
            create_error_tool_result(format!(
                "Tool call \"{}\" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.",
                tool_call.name
            )),
            true,
        );
        emit_tool_execution_end(&finalized, emit).await;
        let tool_result_message = create_tool_result_message(&finalized);
        emit_tool_result_message(&tool_result_message, emit).await;
        messages.push(tool_result_message);
    }
    ExecutedToolCallBatch {
        messages,
        terminate: false,
    }
}

/// pi: `ExecutedToolCallBatch`
struct ExecutedToolCallBatch {
    messages: Vec<ToolResultMessage>,
    terminate: bool,
}

/// pi: `executeToolCalls`
async fn execute_tool_calls(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[ToolCall],
    config: &AgentLoopConfig,
    signal: Option<&AbortSignal>,
    emit: &AgentEventSink,
) -> Result<ExecutedToolCallBatch, AgentError> {
    let hooks = ToolCallHooks::from_config(config);
    let has_sequential_tool_call = tool_calls.iter().any(|tool_call| {
        current_context
            .tools
            .as_ref()
            .and_then(|tools| tools.iter().find(|tool| tool.name() == tool_call.name))
            .and_then(|tool| tool.execution_mode())
            == Some(ToolExecutionMode::Sequential)
    });
    if config.tool_execution == ToolExecutionMode::Sequential || has_sequential_tool_call {
        return execute_tool_calls_sequential(
            current_context,
            assistant_message,
            tool_calls,
            &hooks,
            signal,
            emit,
        )
        .await;
    }
    execute_tool_calls_parallel(
        current_context,
        assistant_message,
        tool_calls,
        &hooks,
        signal,
        emit,
    )
    .await
}

async fn execute_tool_calls_sequential(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[ToolCall],
    hooks: &ToolCallHooks,
    signal: Option<&AbortSignal>,
    emit: &AgentEventSink,
) -> Result<ExecutedToolCallBatch, AgentError> {
    let mut finalized_calls: Vec<AgentToolCallOutcome> = Vec::new();
    let mut messages: Vec<ToolResultMessage> = Vec::new();

    for tool_call in tool_calls {
        emit_tool_execution_start(tool_call, emit).await;

        let preparation = prepare_tool_call(
            current_context,
            assistant_message,
            tool_call,
            hooks,
            signal,
            current_context.tools.as_deref(),
        )
        .await;
        let finalized = match preparation {
            PreparedToolCall::Immediate { result, is_error } => {
                unexecuted(tool_call, result, is_error)
            }
            PreparedToolCall::Prepared(prepared) => {
                let executed = execute_prepared_tool_call(
                    &prepared,
                    signal,
                    emit_tool_execution_update(tool_call, emit),
                )
                .await;
                finalize_executed_tool_call(
                    current_context,
                    assistant_message,
                    &prepared,
                    executed,
                    hooks,
                    signal,
                )
                .await
            }
        };

        emit_tool_execution_end(&finalized, emit).await;
        let tool_result_message = create_tool_result_message(&finalized);
        emit_tool_result_message(&tool_result_message, emit).await;
        finalized_calls.push(finalized);
        messages.push(tool_result_message);
    }

    Ok(ExecutedToolCallBatch {
        messages,
        terminate: should_terminate_tool_batch(&finalized_calls),
    })
}

async fn execute_tool_calls_parallel(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[ToolCall],
    hooks: &ToolCallHooks,
    signal: Option<&AbortSignal>,
    emit: &AgentEventSink,
) -> Result<ExecutedToolCallBatch, AgentError> {
    enum Entry {
        Finalized(AgentToolCallOutcome),
        Pending(Box<PreparedInvocation>),
    }

    let mut entries: Vec<Entry> = Vec::new();
    for tool_call in tool_calls {
        emit_tool_execution_start(tool_call, emit).await;

        match prepare_tool_call(
            current_context,
            assistant_message,
            tool_call,
            hooks,
            signal,
            current_context.tools.as_deref(),
        )
        .await
        {
            PreparedToolCall::Immediate { result, is_error } => {
                let finalized = unexecuted(tool_call, result, is_error);
                emit_tool_execution_end(&finalized, emit).await;
                entries.push(Entry::Finalized(finalized));
            }
            PreparedToolCall::Prepared(prepared) => {
                entries.push(Entry::Pending(Box::new(prepared)));
            }
        }
    }

    let ordered_finalized_calls: Vec<AgentToolCallOutcome> =
        join_all(entries.into_iter().map(|entry| async move {
            let prepared = match entry {
                Entry::Finalized(finalized) => return finalized,
                Entry::Pending(prepared) => prepared,
            };
            let finalized = if signal.is_some_and(AbortSignal::aborted) {
                unexecuted(
                    &prepared.tool_call,
                    create_error_tool_result("Operation aborted"),
                    true,
                )
            } else {
                let executed = execute_prepared_tool_call(
                    &prepared,
                    signal,
                    emit_tool_execution_update(&prepared.tool_call, emit),
                )
                .await;
                finalize_executed_tool_call(
                    current_context,
                    assistant_message,
                    &prepared,
                    executed,
                    hooks,
                    signal,
                )
                .await
            };
            emit_tool_execution_end(&finalized, emit).await;
            finalized
        }))
        .await;

    let mut messages: Vec<ToolResultMessage> = Vec::new();
    for finalized in &ordered_finalized_calls {
        let tool_result_message = create_tool_result_message(finalized);
        emit_tool_result_message(&tool_result_message, emit).await;
        messages.push(tool_result_message);
    }

    Ok(ExecutedToolCallBatch {
        messages,
        terminate: should_terminate_tool_batch(&ordered_finalized_calls),
    })
}

/// pi: `PreparedToolCall` / `ImmediateToolCallOutcome`
enum PreparedToolCall {
    Prepared(PreparedInvocation),
    Immediate {
        result: AgentToolResult,
        is_error: bool,
    },
}

impl PreparedToolCall {
    /// 没去执行就已经有了结果：一条错误。
    fn failed(message: impl Into<String>) -> Self {
        Self::Immediate {
            result: create_error_tool_result(message),
            is_error: true,
        }
    }
}

struct PreparedInvocation {
    tool_call: ToolCall,
    tool: Arc<dyn AgentTool>,
    args: JsonValue,
}

/// pi: `ExecutedToolCallOutcome`
struct ExecutedToolCallOutcome {
    result: AgentToolResult,
    is_error: bool,
    duration_ms: u64,
}

/// pi: `ToolCallHooks`——`beforeToolCall` 与 `afterToolCall` 两个槽位。
#[derive(Clone, Default)]
pub struct ToolCallHooks {
    pub before_tool_call: Option<crate::types::BeforeToolCall>,
    pub after_tool_call: Option<crate::types::AfterToolCall>,
}

impl ToolCallHooks {
    pub fn from_config(config: &AgentLoopConfig) -> Self {
        Self {
            before_tool_call: config.before_tool_call.clone(),
            after_tool_call: config.after_tool_call.clone(),
        }
    }
}

fn should_terminate_tool_batch(finalized_calls: &[AgentToolCallOutcome]) -> bool {
    !finalized_calls.is_empty()
        && finalized_calls
            .iter()
            .all(|finalized| finalized.result.terminate == Some(true))
}

/// pi: `prepareToolCallArguments`
fn prepare_tool_call_arguments(
    tool: &dyn AgentTool,
    tool_call: &ToolCall,
) -> Result<ToolCall, ToolError> {
    let mut prepared = tool_call.clone();
    prepared.arguments = tool.prepare_arguments(&tool_call.arguments)?;
    Ok(prepared)
}

/// pi: `prepareToolCall`——校验、`beforeToolCall`。失败与拦截都落成一条立即的错误结果。
async fn prepare_tool_call(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_call: &ToolCall,
    hooks: &ToolCallHooks,
    signal: Option<&AbortSignal>,
    tools: Option<&[Arc<dyn AgentTool>]>,
) -> PreparedToolCall {
    // 已经 abort：不进钩子，但调用方仍要给这条调用一个结果（#9986）。
    if signal.is_some_and(AbortSignal::aborted) {
        return PreparedToolCall::failed("Operation aborted");
    }
    let tools = tools.unwrap_or(&[]);
    let Some(tool) = tools
        .iter()
        .find(|tool| tool.name() == tool_call.name)
        .cloned()
    else {
        return PreparedToolCall::failed(format!("Tool {} not found", tool_call.name));
    };

    let outcome = AssertUnwindSafe(prepare_tool_call_inner(
        current_context,
        assistant_message,
        tool_call,
        &tool,
        hooks,
        signal,
    ))
    .catch_unwind()
    .await;

    match outcome {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(error)) => PreparedToolCall::failed(error.message()),
        Err(payload) => PreparedToolCall::failed(panic_message(payload, "tool panicked")),
    }
}

async fn prepare_tool_call_inner(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_call: &ToolCall,
    tool: &Arc<dyn AgentTool>,
    hooks: &ToolCallHooks,
    signal: Option<&AbortSignal>,
) -> Result<PreparedToolCall, ToolError> {
    let prepared_tool_call = prepare_tool_call_arguments(tool.as_ref(), tool_call)?;
    let mut validated_args = validate_tool_arguments(&tool.declaration(), &prepared_tool_call)
        .map_err(ToolError::from)?;

    if let Some(before_tool_call) = &hooks.before_tool_call {
        let before_result: Option<BeforeToolCallResult> = {
            let context = BeforeToolCallContext {
                assistant_message,
                tool_call,
                args: &mut validated_args,
                context: current_context,
            };
            before_tool_call(context, signal).await
        };
        if signal.is_some_and(AbortSignal::aborted) {
            return Ok(PreparedToolCall::failed("Operation aborted"));
        }
        if let Some(before_result) = before_result {
            if before_result.block == Some(true) {
                let mut result = create_error_tool_result(
                    before_result
                        .reason
                        .filter(|reason| !reason.is_empty())
                        .unwrap_or_else(|| "Tool execution was blocked".to_string()),
                );
                if before_result.terminate == Some(true) {
                    result.terminate = Some(true);
                }
                return Ok(PreparedToolCall::Immediate {
                    result,
                    is_error: true,
                });
            }
        }
    }

    Ok(PreparedToolCall::Prepared(PreparedInvocation {
        tool_call: tool_call.clone(),
        tool: tool.clone(),
        args: validated_args,
    }))
}

/// pi: `RunToolCallOptions`
pub struct RunToolCallOptions {
    pub tools: Vec<Arc<dyn AgentTool>>,
    pub assistant_message: AssistantMessage,
    pub context: AgentContext,
    pub signal: Option<AbortSignal>,
    pub on_update: Option<ToolUpdateSink>,
    pub before_tool_call: Option<crate::types::BeforeToolCall>,
    pub after_tool_call: Option<crate::types::AfterToolCall>,
}

/// pi: `runToolCall`——用同一套钩子跑一条调用（工具内部再发起的调用走这里）。
pub async fn run_tool_call(
    tool_call: &ToolCall,
    options: RunToolCallOptions,
) -> AgentToolCallOutcome {
    let hooks = ToolCallHooks {
        before_tool_call: options.before_tool_call.clone(),
        after_tool_call: options.after_tool_call.clone(),
    };
    let preparation = prepare_tool_call(
        &options.context,
        &options.assistant_message,
        tool_call,
        &hooks,
        options.signal.as_ref(),
        Some(&options.tools),
    )
    .await;
    let prepared = match preparation {
        PreparedToolCall::Immediate { result, is_error } => {
            return unexecuted(tool_call, result, is_error);
        }
        PreparedToolCall::Prepared(prepared) => prepared,
    };
    let on_update = options
        .on_update
        .clone()
        .unwrap_or_else(|| Arc::new(|_| Box::pin(async {})));
    let executed = execute_prepared_tool_call(&prepared, options.signal.as_ref(), on_update).await;
    finalize_executed_tool_call(
        &options.context,
        &options.assistant_message,
        &prepared,
        executed,
        &hooks,
        options.signal.as_ref(),
    )
    .await
}

/// 一次工具执行要发出去的 update 事件。
pub type ToolUpdateSink =
    Arc<dyn Fn(AgentToolResult) -> xjpcode_ai::BoxFuture<'static, ()> + Send + Sync>;

fn emit_tool_execution_update(tool_call: &ToolCall, emit: &AgentEventSink) -> ToolUpdateSink {
    let emit = emit.clone();
    let tool_call_id = tool_call.id.clone();
    let tool_name = tool_call.name.clone();
    let args = tool_call.arguments.clone();
    Arc::new(move |partial_result| {
        emit(AgentEvent::ToolExecutionUpdate {
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
            args: args.clone(),
            partial_result,
        })
    })
}

/// pi: `executePreparedToolCall`
async fn execute_prepared_tool_call(
    prepared: &PreparedInvocation,
    signal: Option<&AbortSignal>,
    on_update: ToolUpdateSink,
) -> ExecutedToolCallOutcome {
    let params = match &prepared.args {
        JsonValue::Object(params) => params.clone(),
        // 校验之后参数一定是对象；`beforeToolCall` 换成别的就报这一条
        // （上游会把任何值直接交给工具，见 PORTING.md 差异表 #75）。
        other => {
            return ExecutedToolCallOutcome {
                result: create_error_tool_result(format!(
                    "Tool arguments must be a JSON object, got {}",
                    js_type_name(other)
                )),
                is_error: true,
                duration_ms: 0,
            };
        }
    };

    // 工具报进度时，事件要在工具还在跑的时候就发出去（上游的 `onUpdate` 一调用就开始发）：
    // 回调把事件的 future 交过来，下面一边等工具、一边按到达顺序把它们发完。
    let (queue, mut queued) =
        tokio::sync::mpsc::unbounded_channel::<xjpcode_ai::BoxFuture<'static, ()>>();
    let accepting_updates = Arc::new(AtomicBool::new(true));
    let callback: AgentToolUpdateCallback = {
        let accepting_updates = Arc::clone(&accepting_updates);
        let on_update = Arc::clone(&on_update);
        Arc::new(move |partial_result| {
            if accepting_updates.load(Ordering::SeqCst) {
                let _ = queue.send(on_update(partial_result));
            }
            Box::pin(async {})
        })
    };

    let started_at = Instant::now();
    let execution = AssertUnwindSafe(prepared.tool.execute(
        &prepared.tool_call.id,
        &params,
        signal,
        Some(&callback),
    ))
    .catch_unwind();
    tokio::pin!(execution);
    let mut sending: Option<xjpcode_ai::BoxFuture<'static, ()>> = None;
    let result = loop {
        tokio::select! {
            biased;
            result = &mut execution => break result,
            () = async { if let Some(update) = sending.as_mut() { update.await } }, if sending.is_some() => sending = None,
            Some(update) = queued.recv(), if sending.is_none() => sending = Some(update),
        }
    };
    let duration_ms = (started_at.elapsed().as_secs_f64() * 1000.0).round() as u64;
    accepting_updates.store(false, Ordering::SeqCst);
    // 与上游一样：`execute()` 返回之后到达的 update 丢掉；已经收到的按顺序发完。
    if let Some(update) = sending {
        update.await;
    }
    while let Ok(update) = queued.try_recv() {
        update.await;
    }

    match result {
        Ok(Ok(result)) => ExecutedToolCallOutcome {
            is_error: result.is_error == Some(true),
            result,
            duration_ms,
        },
        Ok(Err(error)) => ExecutedToolCallOutcome {
            result: create_error_tool_result(error.message().to_string()),
            is_error: true,
            duration_ms,
        },
        Err(payload) => ExecutedToolCallOutcome {
            result: create_error_tool_result(panic_message(payload, "tool panicked")),
            is_error: true,
            duration_ms,
        },
    }
}

/// pi: `finalizeExecutedToolCall`
async fn finalize_executed_tool_call(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    prepared: &PreparedInvocation,
    executed: ExecutedToolCallOutcome,
    hooks: &ToolCallHooks,
    signal: Option<&AbortSignal>,
) -> AgentToolCallOutcome {
    let mut result = executed.result;
    let mut is_error = executed.is_error;

    if let Some(after_tool_call) = &hooks.after_tool_call {
        let outcome = AssertUnwindSafe(async {
            let context = AfterToolCallContext {
                assistant_message,
                tool_call: &prepared.tool_call,
                args: &prepared.args,
                result: &result,
                is_error,
                context: current_context,
            };
            after_tool_call(context, signal).await
        })
        .catch_unwind()
        .await;

        match outcome {
            Ok(None) => {}
            Ok(Some(after_result)) => {
                // 只换了 content 的 structuredContent 可能已经对不上，除非一起给。
                let structured_content = match non_null(after_result.structured_content) {
                    Some(structured_content) => Some(structured_content),
                    None if after_result.content.is_some() => None,
                    None => result.structured_content.clone(),
                };
                let mut next = result.clone();
                if let Some(content) = after_result.content {
                    next.content = content;
                }
                if let Some(details) = non_null(after_result.details) {
                    next.details = Some(details);
                }
                if let Some(usage) = after_result.usage {
                    next.usage = Some(usage);
                }
                if let Some(terminate) = after_result.terminate {
                    next.terminate = Some(terminate);
                }
                next.structured_content = structured_content;
                result = next;
                if let Some(after_is_error) = after_result.is_error {
                    is_error = after_is_error;
                }
            }
            Err(payload) => {
                result = create_error_tool_result(panic_message(payload, "tool panicked"));
                is_error = true;
            }
        }
    }

    AgentToolCallOutcome {
        tool_call: prepared.tool_call.clone(),
        result,
        is_error,
        duration_ms: Some(executed.duration_ms),
    }
}

fn non_null(value: Option<JsonValue>) -> Option<JsonValue> {
    match value {
        Some(JsonValue::Null) | None => None,
        Some(value) => Some(value),
    }
}

/// pi: `createErrorToolResult`
pub fn create_error_tool_result(message: impl Into<String>) -> AgentToolResult {
    AgentToolResult {
        content: vec![ToolResultContentBlock::Text(TextContent::new(message))],
        details: Some(serde_json::json!({})),
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

/// 没有执行过的调用的结局：没有 `duration_ms`。
fn unexecuted(
    tool_call: &ToolCall,
    result: AgentToolResult,
    is_error: bool,
) -> AgentToolCallOutcome {
    AgentToolCallOutcome {
        tool_call: tool_call.clone(),
        result,
        is_error,
        duration_ms: None,
    }
}

async fn emit_tool_execution_start(tool_call: &ToolCall, emit: &AgentEventSink) {
    emit(AgentEvent::ToolExecutionStart {
        tool_call_id: tool_call.id.clone(),
        tool_name: tool_call.name.clone(),
        args: tool_call.arguments.clone(),
    })
    .await;
}
async fn emit_tool_execution_end(finalized: &AgentToolCallOutcome, emit: &AgentEventSink) {
    emit(AgentEvent::ToolExecutionEnd {
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        result: finalized.result.clone(),
        is_error: finalized.is_error,
        duration_ms: finalized.duration_ms,
    })
    .await;
}

fn create_tool_result_message(finalized: &AgentToolCallOutcome) -> ToolResultMessage {
    ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        // 不带类型的工具可能不给 content；归一成空列表，别让 null 进会话记录或请求体。
        content: finalized.result.content.clone(),
        details: finalized.result.details.clone(),
        usage: finalized.result.usage.clone(),
        nested_calls: None,
        is_error: finalized.is_error,
        timestamp: now_ms(),
        duration_ms: finalized.duration_ms,
    }
}

async fn emit_tool_result_message(tool_result_message: &ToolResultMessage, emit: &AgentEventSink) {
    emit(AgentEvent::MessageStart {
        message: AgentMessage::ToolResult(tool_result_message.clone()),
    })
    .await;
    emit(AgentEvent::MessageEnd {
        message: AgentMessage::ToolResult(tool_result_message.clone()),
    })
    .await;
}

/// panic 的载荷变成一句话：`panic!("…")` 的字符串，或者调用方的兜底文字。
fn panic_message(payload: Box<dyn std::any::Any + Send>, fallback: &str) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    fallback.to_string()
}

/// JSON 值的类型名，只用在上面那句错误里。
fn js_type_name(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "boolean",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}
