//! 移植自上游 `test/compaction.test.ts` 与 `test/compaction-serialization.test.ts`。
//!
//! 大型会话样本（`test/fixtures/large-session.jsonl`）是 v1 格式；上游测试先 `migrateSessionEntries`
//! 补 id/parentId。这里在测试里做同一件事，生产代码不读 v1（见 `docs/port/milestone-5.md`）。
//! 需要真实模型的摘要用例（`describe.skipIf(!ANTHROPIC_OAUTH_TOKEN)`）没搬。

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;
use xjpcode_agent::{empty_assistant_message, AgentMessage};
use xjpcode_ai::{
    AssistantContentBlock, Message, StopReason, TextContent, ToolCall, ToolResultContentBlock,
    ToolResultMessage, ToolResultRole, Usage, UsageCost, UserContent, UserMessage, UserRole,
};
use xjpcode_coding_agent::core::compaction::{
    calculate_context_tokens, compute_file_lists, create_file_ops, estimate_context_tokens,
    extract_file_ops_from_message, find_cut_point, get_last_assistant_usage, prepare_compaction,
    serialize_conversation, should_compact, CompactionSettings, DEFAULT_COMPACTION_SETTINGS,
};
use xjpcode_coding_agent::core::session_manager::{
    build_session_context, build_session_context_for_path, parse_session_entries, SessionEntry,
    SessionEntryBase, SessionMessageEntry,
};

fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Usage {
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        total_tokens: input + output + cache_read + cache_write,
        cost: UsageCost::default(),
        ..Usage::default()
    }
}

fn user_message(text: &str) -> AgentMessage {
    AgentMessage::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_string()),
        timestamp: 0,
    })
}

fn assistant_message(text: &str, reported_usage: Option<Usage>) -> AgentMessage {
    let mut message = empty_assistant_message("anthropic-messages", "anthropic", "claude-sonnet-4-5");
    message.content = vec![AssistantContentBlock::Text(TextContent::new(text))];
    message.usage = reported_usage.unwrap_or_else(|| usage(100, 50, 0, 0));
    message.stop_reason = StopReason::Stop;
    AgentMessage::Assistant(message)
}

struct Entries {
    list: Vec<SessionEntry>,
    last_id: Option<String>,
    counter: usize,
}

impl Entries {
    fn new() -> Self {
        Self {
            list: Vec::new(),
            last_id: None,
            counter: 0,
        }
    }

    fn base(&mut self) -> SessionEntryBase {
        let id = format!("test-id-{}", self.counter);
        self.counter += 1;
        let base = SessionEntryBase {
            id: id.clone(),
            parent_id: self.last_id.take(),
            timestamp: format!("2025-01-01T00:00:{:02}.000Z", self.counter % 60),
        };
        self.last_id = Some(id);
        base
    }

    fn message(&mut self, message: AgentMessage) -> String {
        let base = self.base();
        let id = base.id.clone();
        self.list
            .push(SessionEntry::Message(SessionMessageEntry { base, message }));
        id
    }

    fn compaction(&mut self, summary: &str, first_kept_entry_id: &str) -> String {
        let base = self.base();
        let id = base.id.clone();
        self.list.push(SessionEntry::Compaction(
            xjpcode_coding_agent::core::session_manager::CompactionEntry {
                base,
                summary: summary.to_string(),
                first_kept_entry_id: first_kept_entry_id.to_string(),
                tokens_before: 10_000,
                details: None,
                usage: None,
                from_hook: None,
                system_message: None,
            },
        ));
        id
    }

    fn model_change(&mut self, provider: &str, model_id: &str) {
        let base = self.base();
        self.list.push(SessionEntry::ModelChange(
            xjpcode_coding_agent::core::session_manager::ModelChangeEntry {
                base,
                provider: provider.to_string(),
                model_id: model_id.to_string(),
            },
        ));
    }

    fn thinking_level(&mut self, level: &str) {
        let base = self.base();
        self.list.push(SessionEntry::ThinkingLevelChange(
            xjpcode_coding_agent::core::session_manager::ThinkingLevelChangeEntry {
                base,
                thinking_level: level.to_string(),
            },
        ));
    }

    fn custom_message(&mut self, content: &str) -> String {
        let base = self.base();
        let id = base.id.clone();
        self.list.push(SessionEntry::CustomMessage(
            xjpcode_coding_agent::core::session_manager::CustomMessageEntry {
                base,
                custom_type: "test".to_string(),
                content: Value::String(content.to_string()),
                display: true,
                details: None,
            },
        ));
        id
    }
}

fn build_context(entries: &[SessionEntry]) -> xjpcode_coding_agent::core::session_manager::SessionContext {
    let by_id: HashMap<String, SessionEntry> = entries
        .iter()
        .map(|entry| (entry.id().to_string(), entry.clone()))
        .collect();
    let leaf = entries.last().map(|entry| entry.id().to_string());
    build_session_context(entries, leaf.as_deref(), &by_id)
}

fn message_role(entry: &SessionEntry) -> Option<String> {
    match entry {
        SessionEntry::Message(message) => Some(message.message.role().to_string()),
        _ => None,
    }
}

// ============================================================================
// Token calculation
// ============================================================================

#[test]
fn calculates_total_context_tokens_from_usage() {
    assert_eq!(calculate_context_tokens(&usage(1000, 500, 200, 100)), 1800);
    assert_eq!(calculate_context_tokens(&usage(0, 0, 0, 0)), 0);
}

#[test]
fn finds_the_last_non_aborted_nonzero_assistant_usage() {
    let mut entries = Entries::new();
    entries.message(user_message("Hello"));
    entries.message(assistant_message("Hi", Some(usage(100, 50, 0, 0))));
    entries.message(user_message("How are you?"));
    entries.message(assistant_message("Good", Some(usage(200, 100, 0, 0))));
    assert_eq!(get_last_assistant_usage(&entries.list).unwrap().input, 200);

    let mut aborted = assistant_message("Aborted", Some(usage(300, 150, 0, 0)));
    if let AgentMessage::Assistant(assistant) = &mut aborted {
        assistant.stop_reason = StopReason::Aborted;
    }
    entries.message(aborted);
    assert_eq!(get_last_assistant_usage(&entries.list).unwrap().input, 200);

    entries.message(assistant_message("Partial", Some(usage(0, 0, 0, 0))));
    assert_eq!(get_last_assistant_usage(&entries.list).unwrap().input, 200);
}

#[test]
fn returns_no_usage_when_there_are_no_assistant_messages() {
    let mut entries = Entries::new();
    entries.message(user_message("Hello"));
    assert_eq!(get_last_assistant_usage(&entries.list), None);
}

#[test]
fn estimates_context_tokens_from_the_last_nonzero_assistant_usage() {
    let messages = vec![
        user_message("Hello"),
        assistant_message("Hi", Some(usage(100, 50, 0, 0))),
        user_message("continue"),
        assistant_message("Partial thinking", Some(usage(0, 0, 0, 0))),
    ];
    let estimate = estimate_context_tokens(&messages);
    assert_eq!(estimate.usage_tokens, 150);
    assert_eq!(estimate.last_usage_index, Some(1));
    assert!(estimate.trailing_tokens > 0);
    assert_eq!(estimate.tokens, 150 + estimate.trailing_tokens);
}

#[test]
fn compacts_when_the_context_exceeds_the_threshold() {
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 10_000,
        keep_recent_tokens: 20_000,
    };
    assert!(should_compact(95_000, 100_000, &settings));
    assert!(!should_compact(89_000, 100_000, &settings));
    assert!(!should_compact(
        95_000,
        100_000,
        &CompactionSettings {
            enabled: false,
            ..settings
        }
    ));
}

// ============================================================================
// findCutPoint
// ============================================================================

#[test]
fn cuts_at_a_valid_cut_point() {
    let mut entries = Entries::new();
    for index in 0..10 {
        entries.message(user_message(&format!("User {index}")));
        entries.message(assistant_message(
            &format!("Assistant {index}"),
            Some(usage(0, 100, (index + 1) * 1000, 0)),
        ));
    }
    let result = find_cut_point(&entries.list, 0, entries.list.len(), 2500);
    let role = message_role(&entries.list[result.first_kept_entry_index]).unwrap();
    assert!(role == "user" || role == "assistant");
}

#[test]
fn returns_the_start_when_there_are_no_valid_cut_points() {
    let mut entries = Entries::new();
    entries.message(assistant_message("a", None));
    let result = find_cut_point(&entries.list, 0, entries.list.len(), 1000);
    assert_eq!(result.first_kept_entry_index, 0);
}

#[test]
fn keeps_everything_when_all_messages_fit() {
    let mut entries = Entries::new();
    entries.message(user_message("1"));
    entries.message(assistant_message("a", Some(usage(0, 50, 500, 0))));
    entries.message(user_message("2"));
    entries.message(assistant_message("b", Some(usage(0, 50, 1000, 0))));
    let result = find_cut_point(&entries.list, 0, entries.list.len(), 50_000);
    assert_eq!(result.first_kept_entry_index, 0);
}

#[test]
fn marks_a_split_turn_when_cutting_at_an_assistant_message() {
    let mut entries = Entries::new();
    entries.message(user_message("Turn 1"));
    entries.message(assistant_message("A1", Some(usage(0, 100, 1000, 0))));
    entries.message(user_message("Turn 2"));
    entries.message(assistant_message("A2-1", Some(usage(0, 100, 5000, 0))));
    entries.message(assistant_message("A2-2", Some(usage(0, 100, 8000, 0))));
    entries.message(assistant_message("A2-3", Some(usage(0, 100, 10000, 0))));
    let result = find_cut_point(&entries.list, 0, entries.list.len(), 3000);
    if message_role(&entries.list[result.first_kept_entry_index]).as_deref() == Some("assistant") {
        assert!(result.is_split_turn);
        assert_eq!(result.turn_start_index, Some(2));
    }
}

#[test]
fn budgets_context_visible_custom_message_entries() {
    let mut entries = Entries::new();
    entries.message(user_message("hi"));
    entries.message(assistant_message("hello", None));
    entries.custom_message(&"x".repeat(4000));
    entries.message(assistant_message("ok", None));

    let tiny_budget = find_cut_point(&entries.list, 0, entries.list.len(), 1);
    assert_eq!(tiny_budget.first_kept_entry_index, 3);
    assert!(tiny_budget.is_split_turn);
    assert_eq!(tiny_budget.turn_start_index, Some(2));

    let custom_fits_budget = find_cut_point(&entries.list, 0, entries.list.len(), 2);
    assert_eq!(custom_fits_budget.first_kept_entry_index, 2);
    assert!(!custom_fits_budget.is_split_turn);
    assert_eq!(custom_fits_budget.turn_start_index, None);
}

#[test]
fn falls_back_to_the_latest_valid_cut_point_before_oversized_trailing_tool_results() {
    let mut entries = Entries::new();
    entries.message(user_message("old history"));
    entries.message(assistant_message("old answer", None));
    entries.message(user_message("read the large file"));
    let mut tool_call = empty_assistant_message("anthropic-messages", "anthropic", "claude-sonnet-4-5");
    tool_call.content = vec![AssistantContentBlock::ToolCall(ToolCall::new(
        "call-1",
        "read",
        serde_json::json!({"path": "big.txt"})
            .as_object()
            .unwrap()
            .clone(),
    ))];
    tool_call.stop_reason = StopReason::ToolUse;
    let tool_call_id = entries.message(AgentMessage::Assistant(tool_call));
    entries.message(AgentMessage::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "call-1".to_string(),
        tool_name: "read".to_string(),
        content: vec![ToolResultContentBlock::Text(TextContent::new("x".repeat(8000)))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 0,
        duration_ms: None,
    }));

    let result = find_cut_point(&entries.list, 0, entries.list.len(), 1000);
    assert_eq!(
        result,
        xjpcode_coding_agent::core::compaction::CutPointResult {
            first_kept_entry_index: 3,
            turn_start_index: Some(2),
            is_split_turn: true,
        }
    );
    let preparation = prepare_compaction(
        &entries.list,
        CompactionSettings {
            keep_recent_tokens: 1000,
            ..DEFAULT_COMPACTION_SETTINGS
        },
    )
    .unwrap();
    assert_eq!(preparation.first_kept_entry_id, tool_call_id);
    let summarized: Vec<String> = preparation
        .messages_to_summarize
        .iter()
        .map(|message| match message {
            AgentMessage::User(user) => match &user.content {
                UserContent::Text(text) => text.clone(),
                UserContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|block| match block {
                        xjpcode_ai::UserContentBlock::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            },
            AgentMessage::Assistant(assistant) => assistant
                .content
                .iter()
                .filter_map(|block| match block {
                    AssistantContentBlock::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        })
        .collect();
    assert_eq!(summarized, vec!["old history", "old answer"]);
    assert_eq!(
        preparation.turn_prefix_messages,
        vec![user_message("read the large file")]
    );
}

// ============================================================================
// buildSessionContext
// ============================================================================

#[test]
fn loads_all_messages_without_compaction() {
    let mut entries = Entries::new();
    entries.message(user_message("1"));
    entries.message(assistant_message("a", None));
    entries.message(user_message("2"));
    entries.message(assistant_message("b", None));
    let loaded = build_context(&entries.list);
    assert_eq!(loaded.messages.len(), 4);
    assert_eq!(loaded.thinking_level, "off");
    assert_eq!(
        loaded.model,
        Some(("anthropic".to_string(), "claude-sonnet-4-5".to_string()))
    );
}

#[test]
fn handles_a_single_compaction() {
    let mut entries = Entries::new();
    entries.message(user_message("1"));
    entries.message(assistant_message("a", None));
    let kept = entries.message(user_message("2"));
    entries.message(assistant_message("b", None));
    entries.compaction("Summary of 1,a,2,b", &kept);
    entries.message(user_message("3"));
    entries.message(assistant_message("c", None));

    let loaded = build_context(&entries.list);
    assert_eq!(loaded.messages.len(), 5);
    assert_eq!(loaded.messages[0].role(), "compactionSummary");
}

#[test]
fn only_the_latest_of_multiple_compactions_matters() {
    let mut entries = Entries::new();
    let first = entries.message(user_message("1"));
    entries.message(assistant_message("a", None));
    entries.compaction("First summary", &first);
    entries.message(user_message("2"));
    entries.message(assistant_message("b", None));
    let kept = entries.message(user_message("3"));
    entries.message(assistant_message("c", None));
    entries.compaction("Second summary", &kept);
    entries.message(user_message("4"));
    entries.message(assistant_message("d", None));

    let loaded = build_context(&entries.list);
    assert_eq!(loaded.messages.len(), 5);
    let summary = match &loaded.messages[0] {
        AgentMessage::Custom(custom) => custom.payload.get("summary").unwrap().as_str().unwrap(),
        _ => panic!("expected a compaction summary"),
    };
    assert!(summary.contains("Second summary"));
}

#[test]
fn keeps_all_messages_when_the_first_kept_entry_is_the_first_entry() {
    let mut entries = Entries::new();
    let first = entries.message(user_message("1"));
    entries.message(assistant_message("a", None));
    entries.compaction("First summary", &first);
    entries.message(user_message("2"));
    entries.message(assistant_message("b", None));
    let loaded = build_context(&entries.list);
    assert_eq!(loaded.messages.len(), 5);
}

#[test]
fn tracks_model_and_thinking_level_changes() {
    let mut entries = Entries::new();
    entries.message(user_message("1"));
    entries.model_change("openai", "gpt-4");
    entries.message(assistant_message("a", None));
    entries.thinking_level("high");
    let loaded = build_context(&entries.list);
    // model_change 被后一条 assistant 消息里的模型信息盖掉。
    assert_eq!(
        loaded.model,
        Some(("anthropic".to_string(), "claude-sonnet-4-5".to_string()))
    );
    assert_eq!(loaded.thinking_level, "high");
}

// ============================================================================
// prepareCompaction
// ============================================================================

#[test]
fn does_not_treat_system_messages_as_conversation_history() {
    let mut entries = Entries::new();
    let system = xjpcode_ai::SystemMessage {
        role: xjpcode_ai::SystemRole::System,
        content: xjpcode_ai::SystemContent::Text(String::new()),
        sections: Some(
            [("preamble".to_string(), Some("current prompt".to_string()))]
                .into_iter()
                .collect(),
        ),
        tools_added: None,
        tools_removed: None,
        timestamp: 0,
    };
    entries.message(AgentMessage::System(system));
    entries.message(user_message("one long turn"));
    let assistant = entries.message(assistant_message("assistant suffix", None));

    let preparation = prepare_compaction(
        &entries.list,
        CompactionSettings {
            keep_recent_tokens: 1,
            ..DEFAULT_COMPACTION_SETTINGS
        },
    )
    .unwrap();
    assert_eq!(preparation.first_kept_entry_id, assistant);
    assert!(preparation.is_split_turn);
    assert!(preparation.messages_to_summarize.is_empty());
    assert_eq!(
        preparation.turn_prefix_messages,
        vec![user_message("one long turn")]
    );
}

#[test]
fn skips_repeated_compactions_when_kept_messages_still_fit() {
    let mut entries = Entries::new();
    entries.message(user_message("user msg 1 (summarized by compaction1)"));
    entries.message(assistant_message("assistant msg 1", None));
    let kept = entries.message(user_message("user msg 2 - kept by compaction1"));
    entries.message(assistant_message("assistant msg 2", None));
    entries.message(user_message("user msg 3 - kept by compaction1"));
    entries.message(assistant_message(
        "assistant msg 3",
        Some(usage(5000, 1000, 0, 0)),
    ));
    entries.compaction("First summary", &kept);
    entries.message(user_message("user msg 4 (new after compaction1)"));
    entries.message(assistant_message(
        "assistant msg 4",
        Some(usage(8000, 2000, 0, 0)),
    ));
    assert!(prepare_compaction(&entries.list, DEFAULT_COMPACTION_SETTINGS).is_none());
}

#[test]
fn re_summarizes_previously_kept_messages_when_the_window_moves_past_them() {
    let mut entries = Entries::new();
    entries.message(user_message(&"user msg 1 (summarized by compaction1)".repeat(4)));
    entries.message(assistant_message(&"assistant msg 1".repeat(4), None));
    let kept = entries.message(user_message(&"user msg 2 - kept by compaction1 ".repeat(12)));
    entries.message(assistant_message(&"assistant msg 2 ".repeat(12), None));
    entries.message(user_message(&"user msg 3 - kept by compaction1 ".repeat(12)));
    entries.message(assistant_message(
        &"assistant msg 3 ".repeat(12),
        Some(usage(5000, 1000, 0, 0)),
    ));
    entries.compaction("First summary", &kept);
    entries.message(user_message(&"user msg 4 (new after compaction1) ".repeat(12)));
    entries.message(assistant_message(
        &"assistant msg 4 ".repeat(12),
        Some(usage(8000, 2000, 0, 0)),
    ));

    let preparation = prepare_compaction(
        &entries.list,
        CompactionSettings {
            keep_recent_tokens: 100,
            ..DEFAULT_COMPACTION_SETTINGS
        },
    )
    .unwrap();
    let text: String = preparation
        .messages_to_summarize
        .iter()
        .map(|message| match message {
            AgentMessage::User(user) => match &user.content {
                UserContent::Text(text) => text.clone(),
                UserContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|block| match block {
                        xjpcode_ai::UserContentBlock::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            },
            AgentMessage::Assistant(assistant) => assistant
                .content
                .iter()
                .filter_map(|block| match block {
                    AssistantContentBlock::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("user msg 2 - kept by compaction1"));
    assert!(text.contains("user msg 3 - kept by compaction1"));
    assert!(!text.contains("First summary"));
    assert_eq!(preparation.previous_summary.as_deref(), Some("First summary"));
}

// ============================================================================
// Large session fixture
// ============================================================================

fn load_large_session_entries() -> Vec<SessionEntry> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test/fixtures/large-session.jsonl");
    let content = std::fs::read_to_string(path).unwrap();
    let mut values = parse_session_entries(&content);
    values.retain(|value| value.get("type").and_then(Value::as_str) != Some("session"));
    let mut parent: Option<String> = None;
    for (index, value) in values.iter_mut().enumerate() {
        let object = value.as_object_mut().unwrap();
        let id = format!("migrated-{index}");
        object.insert("id".to_string(), Value::String(id.clone()));
        object.insert(
            "parentId".to_string(),
            parent.clone().map(Value::String).unwrap_or(Value::Null),
        );
        parent = Some(id);
    }
    values
        .iter()
        .filter_map(|value| serde_json::from_value(value.clone()).ok())
        .collect()
}

#[test]
fn parses_the_large_session() {
    let entries = load_large_session_entries();
    assert!(entries.len() > 100);
    let messages = entries
        .iter()
        .filter(|entry| matches!(entry, SessionEntry::Message(_)))
        .count();
    assert!(messages > 100);
}

#[test]
fn finds_a_cut_point_in_the_large_session() {
    let entries = load_large_session_entries();
    let result = find_cut_point(
        &entries,
        0,
        entries.len(),
        DEFAULT_COMPACTION_SETTINGS.keep_recent_tokens,
    );
    let role = message_role(&entries[result.first_kept_entry_index]).unwrap();
    assert!(role == "user" || role == "assistant");
}

#[test]
fn loads_the_large_session_context() {
    let entries = load_large_session_entries();
    let loaded = build_session_context_for_path(&entries);
    assert!(loaded.messages.len() > 100);
    assert!(loaded.model.is_some());
}

// ============================================================================
// serializeConversation
// ============================================================================

fn tool_result(text: &str) -> Message {
    Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "tc1".to_string(),
        tool_name: "read".to_string(),
        content: vec![ToolResultContentBlock::Text(TextContent::new(text))],
        details: None,
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 0,
        duration_ms: None,
    })
}

#[test]
fn truncates_long_tool_results() {
    let long = "x".repeat(5000);
    let result = serialize_conversation(&[tool_result(&long)]);
    assert!(result.contains("[Tool result]:"));
    assert!(result.contains("[... 3000 more characters truncated]"));
    assert!(result.contains(&"x".repeat(2000)));
    assert!(!result.contains(&"x".repeat(3000)));
}

#[test]
fn does_not_truncate_short_tool_results() {
    let short = "x".repeat(1500);
    assert_eq!(
        serialize_conversation(&[tool_result(&short)]),
        format!("[Tool result]: {short}")
    );
}

#[test]
fn does_not_truncate_assistant_or_user_messages() {
    let long = "y".repeat(5000);
    let messages = vec![
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![xjpcode_ai::UserContentBlock::Text(
                TextContent::new(long.clone()),
            )]),
            timestamp: 0,
        }),
        Message::Assistant(assistant_message(&long, None).into_assistant().unwrap()),
    ];
    let result = serialize_conversation(&messages);
    assert!(!result.contains("truncated"));
    assert!(result.contains(&long));
}

#[test]
fn omits_thinking() {
    let mut assistant = empty_assistant_message("anthropic-messages", "anthropic", "test");
    assistant.content = vec![
        AssistantContentBlock::Thinking(xjpcode_ai::ThinkingContent {
            r#type: xjpcode_ai::ThinkingContentType::Thinking,
            thinking: "private reasoning".to_string(),
            thinking_signature: Some("sig".to_string()),
            redacted: None,
        }),
        AssistantContentBlock::Text(TextContent::new("visible reply")),
        AssistantContentBlock::ToolCall(ToolCall::new(
            "tc1",
            "read",
            serde_json::json!({"path": "a.ts"})
                .as_object()
                .unwrap()
                .clone(),
        )),
    ];
    assistant.stop_reason = StopReason::ToolUse;
    let result = serialize_conversation(&[Message::Assistant(assistant)]);
    assert_eq!(
        result,
        "[Assistant]: visible reply\n\n[Assistant tool calls]: read(path=\"a.ts\")"
    );
}

#[test]
fn file_operations_include_files_touched_by_nested_calls() {
    // 移植自上游 `test/compaction-nested-calls.test.ts`。
    let mut read_arguments = serde_json::Map::new();
    read_arguments.insert("path".to_string(), serde_json::json!("a.ts"));
    let mut edit_arguments = serde_json::Map::new();
    edit_arguments.insert("path".to_string(), serde_json::json!("b.ts"));
    edit_arguments.insert("edits".to_string(), serde_json::json!([]));
    let message = AgentMessage::ToolResult(xjpcode_ai::ToolResultMessage {
        role: xjpcode_ai::ToolResultRole::ToolResult,
        tool_call_id: "codemode-1".to_string(),
        tool_name: "codemode".to_string(),
        content: Vec::new(),
        details: None,
        usage: None,
        nested_calls: Some(xjpcode_ai::NestedToolCalls {
            calls: vec![
                xjpcode_ai::NestedToolCallRecord {
                    id: "codemode-1/1".to_string(),
                    name: "read".to_string(),
                    arguments: Some(read_arguments),
                    arguments_bytes: None,
                    status: xjpcode_ai::NestedToolCallStatus::Ok,
                    duration_ms: None,
                    error: None,
                },
                xjpcode_ai::NestedToolCallRecord {
                    id: "codemode-1/2".to_string(),
                    name: "edit".to_string(),
                    arguments: Some(edit_arguments),
                    arguments_bytes: None,
                    status: xjpcode_ai::NestedToolCallStatus::Ok,
                    duration_ms: None,
                    error: None,
                },
                xjpcode_ai::NestedToolCallRecord {
                    id: "codemode-1/3".to_string(),
                    name: "write".to_string(),
                    arguments: None,
                    arguments_bytes: Some(40_000),
                    status: xjpcode_ai::NestedToolCallStatus::Ok,
                    duration_ms: None,
                    error: None,
                },
            ],
            complete: false,
        }),
        is_error: false,
        timestamp: 0,
        duration_ms: None,
    });
    let mut ops = create_file_ops();
    extract_file_ops_from_message(&message, &mut ops);
    assert_eq!(
        compute_file_lists(&ops),
        (vec!["a.ts".to_string()], vec!["b.ts".to_string()])
    );
}

trait IntoAssistant {
    fn into_assistant(self) -> Option<xjpcode_ai::AssistantMessage>;
}

impl IntoAssistant for AgentMessage {
    fn into_assistant(self) -> Option<xjpcode_ai::AssistantMessage> {
        match self {
            AgentMessage::Assistant(assistant) => Some(assistant),
            _ => None,
        }
    }
}
