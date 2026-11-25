//! 移植自上游 `test/agent-session-retry.test.ts`、`test/suite/agent-session-retry-events.test.ts`
//! 与 `test/agent-session-compaction.test.ts` 里不依赖真模型的用例。
//!
//! 假模型在 `test/utils/faux.rs`：一条响应队列、一个流函数，能记下每次请求的上下文。
//! 扩展、图片、`!` 命令、分支树相关的用例没搬（见 `docs/port/milestone-5.md`）。

#[path = "utils/faux.rs"]
mod faux;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;
use xjpcode_agent::{
    Agent, AgentInitialState, AgentMessage, AgentOptions, AgentTool, StreamFn,
};
use xjpcode_ai::{
    content_text, AssistantContentBlock, Message, StopReason, UserContent, UserMessage, UserRole,
};
use xjpcode_coding_agent::core::agent_session::{
    AgentSession, AgentSessionEvent, AgentSessionOptions, CompactionReason,
};
use xjpcode_coding_agent::core::compaction::CompactionSettings;
use xjpcode_coding_agent::core::session_manager::{
    load_entries_from_file, NewSessionOptions, SessionEntry, SessionManager,
};
use xjpcode_coding_agent::core::settings_manager::Settings;
use xjpcode_coding_agent::core::system_prompt::BuildSystemPromptOptions;

use faux::{FauxProvider, FauxResponse};

fn retry_settings(max_retries: u32, base_delay_ms: u64, max_agent_delay_ms: Option<u64>) -> Settings {
    let mut settings = Settings::in_memory();
    settings.retry.enabled = true;
    settings.retry.max_retries = max_retries;
    settings.retry.base_delay_ms = base_delay_ms;
    settings.retry.max_agent_delay_ms = max_agent_delay_ms;
    settings.compaction.enabled = false;
    settings
}

fn compaction_settings(keep_recent_tokens: u64, reserve_tokens: u64) -> Settings {
    let mut settings = Settings::in_memory();
    settings.retry.enabled = false;
    settings.compaction = CompactionSettings {
        enabled: true,
        reserve_tokens,
        keep_recent_tokens,
    };
    settings
}

fn create_agent(provider: &FauxProvider, tools: Vec<Arc<dyn AgentTool>>) -> Agent {
    Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            system_prompt: Some("Test".to_string()),
            model: Some(provider.model()),
            thinking_level: None,
            tools: Some(tools),
            messages: None,
        }),
        stream_fn: Some(provider.stream_fn()),
        get_api_key: Some(Arc::new(|_provider: &str| {
            Box::pin(async { Some("test-key".to_string()) }) as xjpcode_ai::BoxFuture<'_, _>
        })),
        ..AgentOptions::default()
    })
    .unwrap()
}

fn create_session_with(
    provider: &FauxProvider,
    settings: Settings,
    tools: Vec<Arc<dyn AgentTool>>,
    session_manager: SessionManager,
    cwd: &std::path::Path,
) -> Arc<AgentSession> {
    let stream_fn: StreamFn = provider.stream_fn();
    let agent = create_agent(provider, tools);
    AgentSession::new(AgentSessionOptions {
        agent,
        session_manager,
        settings,
        cwd: cwd.to_path_buf(),
        stream_fn,
        get_api_key: None,
        api_key: Some("test-key".to_string()),
        prompt_options: BuildSystemPromptOptions {
            selected_tools: Some(Vec::new()),
            cwd: cwd.to_string_lossy().into_owned(),
            ..BuildSystemPromptOptions::default()
        },
    })
}

fn create_session(provider: &FauxProvider, settings: Settings) -> Arc<AgentSession> {
    let cwd = PathBuf::from("/work");
    let session_manager =
        SessionManager::in_memory(&cwd, NewSessionOptions::default(), None).unwrap();
    create_session_with(provider, settings, Vec::new(), session_manager, &cwd)
}

fn create_session_in(
    provider: &FauxProvider,
    settings: Settings,
    dir: &std::path::Path,
) -> (Arc<AgentSession>, SessionManager) {
    let cwd = PathBuf::from("/work");
    let session_manager =
        SessionManager::create(&cwd, Some(dir.to_path_buf()), NewSessionOptions::default())
            .unwrap();
    let session = create_session_with(provider, settings, Vec::new(), session_manager, &cwd);
    let file = session.session_file().unwrap();
    (session, SessionManager::open(&file, None, None).unwrap())
}

fn text_of(message: &AgentMessage) -> String {
    faux::message_text(message)
}

fn session_entries(path: &std::path::Path) -> Vec<SessionEntry> {
    load_entries_from_file(path)
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect()
}

// ============================================================================
// 自动重试
// ============================================================================

#[tokio::test]
async fn retries_after_a_transient_error_and_succeeds() {
    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![
        FauxResponse::error("overloaded_error"),
        FauxResponse::text("recovered"),
    ]);
    let session = create_session(&provider, retry_settings(3, 1, None));
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&events);
    let _subscription = session.subscribe(move |event| match event {
        AgentSessionEvent::AutoRetryStart { attempt, .. } => {
            sink.lock().unwrap().push(format!("start:{attempt}"));
        }
        AgentSessionEvent::AutoRetryEnd { success, .. } => {
            sink.lock().unwrap().push(format!("end:{success}"));
        }
        _ => {}
    });

    session.prompt("Test", None).await.unwrap();

    assert_eq!(provider.call_count(), 2);
    assert_eq!(*events.lock().unwrap(), vec!["start:1", "end:true"]);
    assert!(!session.is_retrying());
    // 重试的请求里不带那次失败的 assistant 消息。
    let contexts = provider.contexts();
    assert_eq!(contexts.len(), 2);
    assert!(!contexts[1].iter().any(|message| matches!(
        message,
        Message::Assistant(assistant) if assistant.stop_reason == StopReason::Error
    )));
}

#[tokio::test]
async fn retries_multiple_transient_failures_and_succeeds_on_the_final_attempt() {
    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![
        FauxResponse::error("overloaded_error"),
        FauxResponse::error("overloaded_error"),
        FauxResponse::text("success"),
    ]);
    let session = create_session(&provider, retry_settings(3, 1, None));
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&events);
    let _subscription = session.subscribe(move |event| match event {
        AgentSessionEvent::AutoRetryStart { attempt, .. } => {
            sink.lock().unwrap().push(format!("start:{attempt}"));
        }
        AgentSessionEvent::AutoRetryEnd { success, .. } => {
            sink.lock().unwrap().push(format!("end:{success}"));
        }
        _ => {}
    });
    session.prompt("Test", None).await.unwrap();
    assert_eq!(provider.call_count(), 3);
    assert_eq!(
        *events.lock().unwrap(),
        vec!["start:1", "start:2", "end:true"]
    );
}

#[tokio::test]
async fn exhausts_max_retries_and_emits_a_failure_event() {
    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![
        FauxResponse::error("overloaded_error"),
        FauxResponse::error("overloaded_error"),
        FauxResponse::error("overloaded_error"),
    ]);
    let session = create_session(&provider, retry_settings(2, 1, None));
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let will_retry = Arc::new(Mutex::new(Vec::<bool>::new()));
    let sink = Arc::clone(&events);
    let retries = Arc::clone(&will_retry);
    let _subscription = session.subscribe(move |event| match event {
        AgentSessionEvent::AutoRetryStart { attempt, .. } => {
            sink.lock().unwrap().push(format!("start:{attempt}"));
        }
        AgentSessionEvent::AutoRetryEnd { success, .. } => {
            sink.lock().unwrap().push(format!("end:{success}"));
        }
        AgentSessionEvent::AgentEnd { will_retry: retry, .. } => {
            retries.lock().unwrap().push(*retry);
        }
        _ => {}
    });
    session.prompt("Test", None).await.unwrap();
    assert_eq!(provider.call_count(), 3);
    assert_eq!(
        *events.lock().unwrap(),
        vec!["start:1", "start:2", "end:false"]
    );
    assert_eq!(*will_retry.lock().unwrap(), vec![true, true, false]);
}

#[tokio::test]
async fn caps_the_agent_retry_delay() {
    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![
        FauxResponse::error("overloaded_error"),
        FauxResponse::error("overloaded_error"),
        FauxResponse::error("overloaded_error"),
        FauxResponse::error("overloaded_error"),
        FauxResponse::text("ok"),
    ]);
    let session = create_session(&provider, retry_settings(5, 1, Some(5)));
    let delays = Arc::new(Mutex::new(Vec::<u64>::new()));
    let sink = Arc::clone(&delays);
    let _subscription = session.subscribe(move |event| {
        if let AgentSessionEvent::AutoRetryStart { delay_ms, .. } = event {
            sink.lock().unwrap().push(*delay_ms);
        }
    });
    session.prompt("Test", None).await.unwrap();
    assert_eq!(*delays.lock().unwrap(), vec![1, 2, 4, 5]);
}

#[tokio::test]
async fn retries_provider_network_errors() {
    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![
        FauxResponse::error("Provider finish_reason: network_error"),
        FauxResponse::text("Recovered after retry"),
    ]);
    let session = create_session(&provider, retry_settings(3, 1, None));
    session.prompt("Test", None).await.unwrap();
    assert_eq!(provider.call_count(), 2);
}

#[tokio::test]
async fn does_not_retry_when_retry_is_disabled_or_the_error_is_final() {
    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![FauxResponse::error("invalid_api_key")]);
    let session = create_session(&provider, retry_settings(3, 1, None));
    session.prompt("Test", None).await.unwrap();
    assert_eq!(provider.call_count(), 1);

    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![FauxResponse::error("overloaded_error")]);
    let mut settings = retry_settings(3, 1, None);
    settings.retry.enabled = false;
    let session = create_session(&provider, settings);
    session.prompt("Test", None).await.unwrap();
    assert_eq!(provider.call_count(), 1);
}

#[tokio::test]
async fn cancels_the_retry_sleep_when_abort_retry_is_called() {
    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![FauxResponse::error("overloaded_error")]);
    let session = create_session(&provider, retry_settings(3, 100, None));
    let ended = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let sink = Arc::clone(&ended);
    let _subscription = session.subscribe(move |event| {
        if let AgentSessionEvent::AutoRetryEnd { final_error, .. } = event {
            sink.lock().unwrap().push(final_error.clone());
        }
    });

    let prompt_session = Arc::clone(&session);
    let prompt = tokio::spawn(async move { prompt_session.prompt("Test", None).await });
    while !session.is_retrying() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    session.abort_retry();
    prompt.await.unwrap().unwrap();

    assert!(!session.is_retrying());
    assert_eq!(provider.call_count(), 1);
    assert!(ended
        .lock()
        .unwrap()
        .iter()
        .any(|error| error.as_deref() == Some("Retry cancelled")));
}

#[tokio::test]
async fn finalizes_retry_state_when_abort_is_requested_after_a_retry_attempt_fails() {
    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![
        FauxResponse::error("overloaded_error"),
        FauxResponse::error("overloaded_error"),
    ]);
    let session = create_session(&provider, retry_settings(3, 0, None));
    let error_count = Arc::new(AtomicU32::new(0));
    let last_will_retry = Arc::new(Mutex::new(Vec::<bool>::new()));
    let last_retry_end = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let weak = Arc::downgrade(&session);
    let errors = Arc::clone(&error_count);
    let will_retry = Arc::clone(&last_will_retry);
    let retry_end = Arc::clone(&last_retry_end);
    let _subscription = session.subscribe(move |event| match event {
        AgentSessionEvent::MessageEnd { message } => {
            if let AgentMessage::Assistant(assistant) = message {
                if assistant.stop_reason == StopReason::Error
                    && errors.fetch_add(1, Ordering::SeqCst) + 1 == 2
                {
                    if let Some(session) = weak.upgrade() {
                        session.request_abort();
                    }
                }
            }
        }
        AgentSessionEvent::AgentEnd {
            will_retry: retry, ..
        } => will_retry.lock().unwrap().push(*retry),
        AgentSessionEvent::AutoRetryEnd { final_error, .. } => {
            retry_end.lock().unwrap().push(final_error.clone())
        }
        _ => {}
    });

    session.prompt("Test", None).await.unwrap();

    assert_eq!(session.retry_attempt(), 0);
    assert_eq!(last_will_retry.lock().unwrap().last(), Some(&false));
    assert_eq!(
        last_retry_end.lock().unwrap().last().cloned().flatten().as_deref(),
        Some("Retry cancelled")
    );
}

#[tokio::test]
async fn waits_for_the_full_loop_when_retry_recovery_produces_tool_calls() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("hello.txt"), "hello from the tool").unwrap();
    let read = Arc::new(xjpcode_coding_agent::core::tools::read::create_read_tool(
        dir.path(),
        xjpcode_coding_agent::core::tools::read::ReadToolOptions::default(),
    )) as Arc<dyn AgentTool>;
    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![
        FauxResponse::error("overloaded_error"),
        FauxResponse::tool_call(
            "call_1",
            "read",
            json!({"path": "hello.txt"}).as_object().unwrap().clone(),
        ),
        FauxResponse::text("Final answer."),
    ]);
    let cwd = dir.path().to_path_buf();
    let session_manager =
        SessionManager::in_memory(&cwd, NewSessionOptions::default(), None).unwrap();
    let session = create_session_with(
        &provider,
        retry_settings(3, 1, None),
        vec![read],
        session_manager,
        &cwd,
    );

    session.prompt("Test", None).await.unwrap();
    assert_eq!(provider.call_count(), 3);
    let tool_output: Vec<String> = session
        .messages()
        .iter()
        .filter(|message| matches!(message, AgentMessage::ToolResult(_)))
        .map(text_of)
        .collect();
    assert!(tool_output
        .iter()
        .any(|text| text.contains("hello from the tool")));
    assert!(!session.is_streaming());
    provider.append_response(FauxResponse::text("follow-up answer"));
    session.prompt("Follow-up", None).await.unwrap();
    assert_eq!(provider.call_count(), 4);
}

// ============================================================================
// 自动压缩
// ============================================================================

#[tokio::test]
async fn compacts_after_a_threshold_is_reached_and_keeps_going() {
    let provider = FauxProvider::new(faux::model(200, 1_000));
    provider.set_responses(vec![
        FauxResponse::text_with_usage("first answer", faux::usage(150, 10)),
        FauxResponse::text("the summary"),
        FauxResponse::text_with_usage("second answer", faux::usage(10, 10)),
    ]);
    let dir = TempDir::new().unwrap();
    let (session, _manager) = create_session_in(
        &provider,
        compaction_settings(1, 100),
        dir.path(),
    );

    session.prompt("hello", None).await.unwrap();
    assert_eq!(provider.call_count(), 2);
    let file = session.session_file().unwrap();
    let entries = session_entries(&file);
    assert!(entries
        .iter()
        .any(|entry| matches!(entry, SessionEntry::Compaction(_))));

    let projection = session.projection_messages();
    let roles: Vec<String> = projection
        .iter()
        .map(|message| message.role().to_string())
        .collect();
    assert!(roles.iter().any(|role| role == "compactionSummary"), "{roles:?}");

    // 压缩后的下一句照常发；旧回答的用量不再触发第二次压缩。
    session.prompt("next", None).await.unwrap();
    assert_eq!(provider.call_count(), 3);
}

#[tokio::test]
async fn compacts_before_sending_when_the_restored_session_is_over_the_threshold() {
    let cwd = PathBuf::from("/work");
    let mut manager =
        SessionManager::in_memory(&cwd, NewSessionOptions::default(), None).unwrap();
    manager
        .append_message(AgentMessage::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("old question".to_string()),
            timestamp: 1,
        }))
        .unwrap();
    let mut assistant = xjpcode_agent::empty_assistant_message("faux", "faux", "faux-model");
    assistant.content = vec![AssistantContentBlock::Text(xjpcode_ai::TextContent::new(
        "old answer",
    ))];
    assistant.stop_reason = StopReason::Stop;
    assistant.usage = faux::usage(150, 10);
    manager
        .append_message(AgentMessage::Assistant(assistant))
        .unwrap();

    let provider = FauxProvider::new(faux::model(200, 1_000));
    provider.set_responses(vec![
        FauxResponse::text("the summary"),
        FauxResponse::text_with_usage("fresh answer", faux::usage(10, 10)),
    ]);
    let session =
        create_session_with(&provider, compaction_settings(1, 100), Vec::new(), manager, &cwd);

    session.prompt("next question", None).await.unwrap();
    assert_eq!(provider.call_count(), 2);
    let projection = session.projection_messages();
    let roles: Vec<String> = projection
        .iter()
        .map(|message| message.role().to_string())
        .collect();
    assert!(roles.iter().any(|role| role == "compactionSummary"), "{roles:?}");
    assert!(roles.iter().any(|role| role == "user"));
}

#[tokio::test]
async fn manually_compacts_and_reports_it_when_the_session_is_too_small() {
    let cwd = PathBuf::from("/work");
    let mut manager =
        SessionManager::in_memory(&cwd, NewSessionOptions::default(), None).unwrap();
    manager
        .append_message(AgentMessage::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("old question".to_string()),
            timestamp: 1,
        }))
        .unwrap();
    let mut assistant = xjpcode_agent::empty_assistant_message("faux", "faux", "faux-model");
    assistant.content = vec![AssistantContentBlock::Text(xjpcode_ai::TextContent::new(
        "old answer",
    ))];
    assistant.stop_reason = StopReason::Stop;
    manager
        .append_message(AgentMessage::Assistant(assistant))
        .unwrap();

    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![FauxResponse::text("S")]);
    let session =
        create_session_with(&provider, compaction_settings(1, 16_384), Vec::new(), manager, &cwd);

    let result = session.compact(None).await.unwrap();
    assert!(result.summary.contains('S'));
    assert!(result.tokens_before > 0);
    let projection = session.projection_messages();
    assert_eq!(projection[0].role(), "compactionSummary");

    let error = session.compact(None).await.unwrap_err();
    assert_eq!(error, "Already compacted");
}

#[tokio::test]
async fn emits_compaction_events_around_auto_compaction() {
    let provider = FauxProvider::new(faux::model(200, 1_000));
    provider.set_responses(vec![
        FauxResponse::text_with_usage("first answer", faux::usage(150, 10)),
        FauxResponse::text("the summary"),
    ]);
    let session = create_session(&provider, compaction_settings(1, 100));
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&events);
    let _subscription = session.subscribe(move |event| match event {
        AgentSessionEvent::CompactionStart { reason } => {
            sink.lock().unwrap().push(format!("start:{reason:?}"));
        }
        AgentSessionEvent::CompactionEnd { reason, .. } => {
            sink.lock().unwrap().push(format!("end:{reason:?}"));
        }
        _ => {}
    });
    session.prompt("hello", None).await.unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            format!("start:{:?}", CompactionReason::Threshold),
            format!("end:{:?}", CompactionReason::Threshold)
        ]
    );
}

// ============================================================================
// 插话与排队
// ============================================================================

#[tokio::test]
async fn steers_a_message_into_the_next_step_of_a_running_turn() {
    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![
        FauxResponse::text("first answer").delayed(150),
        FauxResponse::text("second answer"),
    ]);
    let session = create_session(&provider, retry_settings(0, 1, None));
    let prompt_session = Arc::clone(&session);
    let prompt = tokio::spawn(async move { prompt_session.prompt("first", None).await });
    while !session.is_streaming() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    session.steer("change of plan").unwrap();
    assert_eq!(session.steering_messages(), vec!["change of plan"]);
    prompt.await.unwrap().unwrap();

    assert_eq!(provider.call_count(), 2);
    let texts: Vec<String> = session.messages().iter().map(text_of).collect();
    assert!(texts.iter().any(|text| text == "change of plan"));
    assert!(session.steering_messages().is_empty());
}

#[tokio::test]
async fn queues_a_follow_up_until_the_agent_stops() {
    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![
        FauxResponse::text("first answer").delayed(150),
        FauxResponse::text("second answer"),
    ]);
    let session = create_session(&provider, retry_settings(0, 1, None));
    let prompt_session = Arc::clone(&session);
    let prompt = tokio::spawn(async move { prompt_session.prompt("first", None).await });
    while !session.is_streaming() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    session.follow_up("after you finish").unwrap();
    assert_eq!(session.follow_up_messages(), vec!["after you finish"]);
    prompt.await.unwrap().unwrap();

    assert_eq!(provider.call_count(), 2);
    let texts: Vec<String> = session.messages().iter().map(text_of).collect();
    assert!(texts.iter().any(|text| text == "after you finish"));
    assert!(session.follow_up_messages().is_empty());
}

#[tokio::test]
async fn refuses_a_prompt_without_a_streaming_behavior_while_busy() {
    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![
        FauxResponse::text("slow answer").delayed(200),
    ]);
    let session = create_session(&provider, retry_settings(0, 1, None));
    let prompt_session = Arc::clone(&session);
    let prompt = tokio::spawn(async move { prompt_session.prompt("first", None).await });
    while !session.is_streaming() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let error = session.prompt("second", None).await.unwrap_err();
    assert_eq!(
        error,
        "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message."
    );
    session.request_abort();
    prompt.await.unwrap().unwrap();
}

// ============================================================================
// 接着聊
// ============================================================================

#[tokio::test]
async fn continues_a_session_file_and_appends_to_it() {
    let dir = TempDir::new().unwrap();
    let cwd = PathBuf::from("/work");
    let mut manager =
        SessionManager::create(&cwd, Some(dir.path().to_path_buf()), NewSessionOptions::default())
            .unwrap();
    manager
        .append_message(AgentMessage::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("old question".to_string()),
            timestamp: 1,
        }))
        .unwrap();
    let mut assistant = xjpcode_agent::empty_assistant_message("faux", "faux", "faux-model");
    assistant.content = vec![AssistantContentBlock::Text(xjpcode_ai::TextContent::new(
        "old answer",
    ))];
    assistant.stop_reason = StopReason::Stop;
    manager
        .append_message(AgentMessage::Assistant(assistant))
        .unwrap();
    manager.append_thinking_level_change("high").unwrap();
    let file = manager.get_session_file().unwrap().to_path_buf();
    let session_id = manager.get_session_id().to_string();

    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![FauxResponse::text("new answer")]);
    let reopened = SessionManager::open(&file, None, None).unwrap();
    let session = create_session_with(
        &provider,
        retry_settings(0, 1, None),
        Vec::new(),
        reopened,
        &cwd,
    );

    // 恢复出旧历史与思考档。
    let restored: Vec<String> = session.messages().iter().map(text_of).collect();
    assert!(restored.iter().any(|text| text == "old question"));
    assert!(restored.iter().any(|text| text == "old answer"));
    assert_eq!(session.thinking_level(), xjpcode_agent::ThinkingLevel::High);
    assert_eq!(session.session_id(), session_id);

    session.prompt("new question", None).await.unwrap();
    assert_eq!(provider.call_count(), 1);
    let first_request = &provider.contexts()[0];
    let request_text: Vec<String> = first_request
        .iter()
        .map(|message| match message {
            Message::User(user) => match &user.content {
                UserContent::Text(text) => text.clone(),
                UserContent::Blocks(blocks) => content_text(blocks.as_slice(), ""),
            },
            _ => String::new(),
        })
        .collect();
    assert!(request_text.iter().any(|text| text.contains("old question")));
    assert!(request_text.iter().any(|text| text.contains("new question")));

    // 新消息写进同一份文件。
    let entries = session_entries(&file);
    let user_texts: Vec<String> = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::Message(message) => match &message.message {
                AgentMessage::User(user) => Some(match &user.content {
                    UserContent::Text(text) => text.clone(),
                    UserContent::Blocks(blocks) => content_text(blocks.as_slice(), ""),
                }),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(user_texts, vec!["old question", "new question"]);
}

#[tokio::test]
async fn defaults_to_keeping_the_retry_attempt_visible_in_the_session_file() {
    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![
        FauxResponse::error("overloaded_error"),
        FauxResponse::text("recovered"),
    ]);
    let dir = TempDir::new().unwrap();
    let (session, _manager) = create_session_in(
        &provider,
        retry_settings(3, 1, None),
        dir.path(),
    );
    session.prompt("Test", None).await.unwrap();
    let entries = session_entries(&session.session_file().unwrap());
    let assistants = entries
        .iter()
        .filter(|entry| match entry {
            SessionEntry::Message(message) => {
                matches!(message.message, AgentMessage::Assistant(_))
            }
            _ => false,
        })
        .count();
    // 失败那一次留在原始记录里（模型上下文里被上下文编辑去掉）。
    assert_eq!(assistants, 2);
    assert!(entries
        .iter()
        .any(|entry| matches!(entry, SessionEntry::ContextEdit(_))));
}

#[tokio::test]
async fn recovers_from_a_context_overflow_by_compacting_and_retrying_once() {
    let cwd = PathBuf::from("/work");
    let mut manager =
        SessionManager::in_memory(&cwd, NewSessionOptions::default(), None).unwrap();
    manager
        .append_message(AgentMessage::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("old question that should be summarized".to_string()),
            timestamp: 1,
        }))
        .unwrap();
    let mut old_answer = xjpcode_agent::empty_assistant_message("faux", "faux", "faux-model");
    old_answer.content = vec![AssistantContentBlock::Text(xjpcode_ai::TextContent::new(
        "old answer that should be summarized",
    ))];
    old_answer.stop_reason = StopReason::Stop;
    old_answer.usage = faux::usage(500, 50);
    manager
        .append_message(AgentMessage::Assistant(old_answer))
        .unwrap();

    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![
        FauxResponse::error("prompt is too long: 1200 tokens > 1000 maximum"),
        FauxResponse::text("the overflow summary"),
        FauxResponse::text("recovered answer"),
    ]);
    let session = create_session_with(
        &provider,
        compaction_settings(1, 100),
        Vec::new(),
        manager,
        &cwd,
    );
    let compaction_ends = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&compaction_ends);
    let _subscription = session.subscribe(move |event| {
        if let AgentSessionEvent::CompactionEnd {
            reason,
            will_retry,
            result,
            ..
        } = event
        {
            sink.lock().unwrap().push(format!(
                "{reason:?}:retry={will_retry}:ok={}",
                result.is_some()
            ));
        }
    });

    session.prompt("new question", None).await.unwrap();

    // 溢出恢复是「切在省略掉的失败回答那里」：上一段历史一份摘要、被切开的那一轮前缀一份摘要，
    // 然后才是重试的那次回答（与 pi 的 split turn 行为相同）。
    assert_eq!(provider.call_count(), 4);
    assert_eq!(
        *compaction_ends.lock().unwrap(),
        vec!["Overflow:retry=true:ok=true".to_string()]
    );
    let roles: Vec<String> = session
        .projection_messages()
        .iter()
        .map(|message| message.role().to_string())
        .collect();
    assert!(roles.iter().any(|role| role == "compactionSummary"), "{roles:?}");
}

#[tokio::test]
async fn delivers_a_follow_up_queued_during_auto_compaction() {
    let provider = FauxProvider::new(faux::model(200, 1_000));
    provider.set_responses(vec![
        FauxResponse::text_with_usage("first answer", faux::usage(150, 10)),
        FauxResponse::text("the summary").delayed(200),
        FauxResponse::text_with_usage("queued answer", faux::usage(10, 10)),
    ]);
    let session = create_session(&provider, compaction_settings(1, 100));
    let prompt_session = Arc::clone(&session);
    let prompt = tokio::spawn(async move { prompt_session.prompt("first", None).await });
    // 等自动压缩开始，再排一条 follow-up：压缩结束后它应该被发出去。
    let mut waited = false;
    for _ in 0..200 {
        if session.is_compacting() {
            waited = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(waited, "auto compaction did not start");
    session.follow_up("queued while compacting").unwrap();
    prompt.await.unwrap().unwrap();

    assert_eq!(provider.call_count(), 3);
    let texts: Vec<String> = session.messages().iter().map(text_of).collect();
    assert!(texts.iter().any(|text| text == "queued while compacting"));
    assert!(session.follow_up_messages().is_empty());
}

#[tokio::test]
async fn set_model_switches_the_agent_and_records_the_change() {
    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![FauxResponse::text("before switching")]);
    let dir = TempDir::new().unwrap();
    let (session, _) = create_session_in(&provider, retry_settings(0, 1, None), dir.path());
    session.prompt("hello", None).await.unwrap();

    let mut switched = provider.model();
    switched.id = "autre-model".to_string();
    switched.name = "Autre".to_string();
    session.set_model(switched).unwrap();

    assert_eq!(session.model().id, "autre-model");
    let file = session.session_file().unwrap();
    let entries = session_entries(&file);
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionEntry::ModelChange(change) if change.model_id == "autre-model"
    )));
    let restored = SessionManager::open(&file, None, None).unwrap();
    assert_eq!(
        restored.build_session_context().model,
        Some(("faux".to_string(), "autre-model".to_string()))
    );
}

fn reasoning_model() -> xjpcode_ai::Model {
    use xjpcode_ai::providers::model_schema::{ModelThinkingLevel, ThinkingLevelMap};
    let mut model = faux::model(100_000, 1_000);
    model.id = "faux-reasoning".to_string();
    model.name = "Faux Reasoning".to_string();
    model.reasoning = true;
    model.thinking_level_map = Some(ThinkingLevelMap::from([
        (ModelThinkingLevel::Minimal, None),
        (ModelThinkingLevel::Low, Some("low".to_string())),
        (ModelThinkingLevel::Medium, None),
        (ModelThinkingLevel::High, Some("high".to_string())),
        (ModelThinkingLevel::Max, Some("max".to_string())),
    ]));
    model
}

#[tokio::test]
async fn set_thinking_level_clamps_to_the_model_and_records_only_changes() {
    use xjpcode_agent::ThinkingLevel;
    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    provider.set_responses(vec![FauxResponse::text("before switching")]);
    let dir = TempDir::new().unwrap();
    let (session, _) = create_session_in(&provider, retry_settings(0, 1, None), dir.path());
    // 会话文件在有第一条消息之后才落盘（与 pi 一样），所以先聊一轮。
    session.prompt("hello", None).await.unwrap();

    // 非推理模型只有 off：设 high 被夹回 off，什么也不写。
    assert_eq!(session.available_thinking_levels(), vec![ThinkingLevel::Off]);
    assert_eq!(session.set_thinking_level(ThinkingLevel::High).unwrap(), None);
    assert_eq!(session.thinking_level(), ThinkingLevel::Off);

    session.set_model(reasoning_model()).unwrap();
    // medium 在这个模型上映射为 null，往后找到 high。
    assert_eq!(session.set_thinking_level(ThinkingLevel::Medium).unwrap(), Some(ThinkingLevel::High));
    assert_eq!(session.thinking_level(), ThinkingLevel::High);
    // 再设到同一档不写。
    assert_eq!(session.set_thinking_level(ThinkingLevel::High).unwrap(), None);
    assert_eq!(session.set_thinking_level(ThinkingLevel::Max).unwrap(), Some(ThinkingLevel::Max));
    assert_eq!(session.set_thinking_level(ThinkingLevel::Max).unwrap(), None);

    let file = session.session_file().unwrap();
    let levels: Vec<String> = session_entries(&file)
        .into_iter()
        .filter_map(|entry| match entry {
            SessionEntry::ThinkingLevelChange(change) => Some(change.thinking_level.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(levels, vec!["high".to_string(), "max".to_string()]);
}

#[tokio::test]
async fn set_model_applies_per_model_and_default_thinking_levels() {
    use xjpcode_agent::ThinkingLevel;
    let provider = FauxProvider::new(faux::model(100_000, 1_000));
    let dir = TempDir::new().unwrap();
    let mut settings = retry_settings(0, 1, None);
    settings.default_thinking_level = Some("low".to_string());
    settings
        .model_thinking_levels
        .insert("faux/faux-reasoning".to_string(), "max".to_string());
    let (session, _) = create_session_in(&provider, settings, dir.path());

    session.set_model(reasoning_model()).unwrap();
    assert_eq!(session.thinking_level(), ThinkingLevel::Max);

    let mut other = reasoning_model();
    other.id = "faux-reasoning-2".to_string();
    session.set_model(other).unwrap();
    assert_eq!(session.thinking_level(), ThinkingLevel::Low);
}
