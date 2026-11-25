//! 对应上游 `test/session-manager/*`：会话文件、树、标签、投影与自定义 id。
//!
//! 没搬的：会话列举（`list`/`findMostRecentSession`）、旧版本迁移、fork、
//! 大文件扫描上限；见 `docs/port/milestone-4.md`。

use std::fs;
use std::path::Path;

use serde_json::json;
use tempfile::TempDir;
use xjpcode_agent::{empty_assistant_message, AgentMessage};
use xjpcode_ai::{SystemContent, SystemMessage, SystemRole, UserContent, UserMessage, UserRole};
use xjpcode_coding_agent::core::session_manager::{
    assert_valid_session_id, build_session_context, get_default_session_dir,
    load_entries_from_file, NewSessionOptions, SessionEntry, SessionManager,
    CURRENT_SESSION_VERSION,
};

fn user(text: &str) -> AgentMessage {
    AgentMessage::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Blocks(vec![xjpcode_ai::UserContentBlock::Text(
            xjpcode_ai::TextContent::new(text),
        )]),
        timestamp: 1_700_000_000_000,
    })
}

fn assistant(text: &str, provider: &str, model: &str) -> AgentMessage {
    let mut message = empty_assistant_message("openai-completions", provider, model);
    message.content = vec![xjpcode_ai::AssistantContentBlock::Text(
        xjpcode_ai::TextContent::new(text),
    )];
    AgentMessage::Assistant(message)
}

fn system_message(sections: &[(&str, &str)]) -> SystemMessage {
    SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text(String::new()),
        sections: Some(
            sections
                .iter()
                .map(|(name, text)| ((*name).to_string(), Some((*text).to_string())))
                .collect(),
        ),
        tools_added: None,
        tools_removed: None,
        timestamp: 1_700_000_000_000,
    }
}

fn manager_in_memory() -> SessionManager {
    SessionManager::in_memory(Path::new("/work"), NewSessionOptions::default(), None).unwrap()
}

#[test]
fn writes_a_header_with_the_current_version_and_custom_id() {
    let dir = TempDir::new().unwrap();
    let manager = SessionManager::create(
        Path::new("/work"),
        Some(dir.path().to_path_buf()),
        NewSessionOptions {
            id: Some("my-session.1".to_string()),
            parent_session: None,
        },
    )
    .unwrap();
    let header = manager.get_header().unwrap();
    assert_eq!(header.r#type, "session");
    assert_eq!(header.version, CURRENT_SESSION_VERSION);
    assert_eq!(header.id, "my-session.1");
    assert_eq!(header.cwd, "/work");
    assert!(manager.get_session_file().is_some());
    assert!(manager
        .get_session_file()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .ends_with("_my-session.1.jsonl"));
}

#[test]
fn rejects_invalid_custom_session_ids() {
    for id in ["", " bad", "bad ", "bad/id", "-bad", "bad-"] {
        assert!(
            assert_valid_session_id(id).is_err(),
            "{id:?} should be rejected"
        );
    }
    for id in ["good", "a.b_c-1", "A1"] {
        assert!(
            assert_valid_session_id(id).is_ok(),
            "{id:?} should be accepted"
        );
    }
}

#[test]
fn generates_uuid_v7_when_no_id_is_given() {
    let manager = manager_in_memory();
    let id = manager.get_session_id();
    assert_eq!(id.len(), 36);
    assert_eq!(id.as_bytes()[14], b'7');
}

#[test]
fn does_not_create_a_file_for_setup_entries_alone() {
    let dir = TempDir::new().unwrap();
    let mut manager = SessionManager::create(
        Path::new("/work"),
        Some(dir.path().to_path_buf()),
        NewSessionOptions::default(),
    )
    .unwrap();
    manager
        .append_model_change("deepseek", "deepseek-flash")
        .unwrap();
    manager.append_thinking_level_change("off").unwrap();
    let path = manager.get_session_file().unwrap().to_path_buf();
    assert!(!path.exists());
}

#[test]
fn creates_the_file_when_the_first_user_message_is_appended() {
    let dir = TempDir::new().unwrap();
    let mut manager = SessionManager::create(
        Path::new("/work"),
        Some(dir.path().to_path_buf()),
        NewSessionOptions::default(),
    )
    .unwrap();
    manager
        .append_model_change("deepseek", "deepseek-flash")
        .unwrap();
    manager.append_thinking_level_change("off").unwrap();
    manager.append_message(user("hello")).unwrap();
    let path = manager.get_session_file().unwrap().to_path_buf();
    assert!(path.exists());

    let content = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 4);
    let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(header["type"], "session");
    assert_eq!(header["version"], 3);
    let model: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(model["type"], "model_change");
    assert_eq!(model["provider"], "deepseek");
    assert_eq!(model["modelId"], "deepseek-flash");
    assert_eq!(model["parentId"], serde_json::Value::Null);
    let thinking: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
    assert_eq!(thinking["type"], "thinking_level_change");
    assert_eq!(thinking["thinkingLevel"], "off");
    assert_eq!(thinking["parentId"], model["id"]);
    let message: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
    assert_eq!(message["type"], "message");
    assert_eq!(message["message"]["role"], "user");
    assert_eq!(message["parentId"], thinking["id"]);
}

#[test]
fn appends_later_entries_without_rewriting_earlier_ones() {
    let dir = TempDir::new().unwrap();
    let mut manager = SessionManager::create(
        Path::new("/work"),
        Some(dir.path().to_path_buf()),
        NewSessionOptions::default(),
    )
    .unwrap();
    manager.append_message(user("one")).unwrap();
    manager
        .append_message(assistant("two", "deepseek", "deepseek-flash"))
        .unwrap();
    let path = manager.get_session_file().unwrap().to_path_buf();
    let first = fs::read_to_string(&path).unwrap();
    manager.append_message(user("three")).unwrap();
    let second = fs::read_to_string(&path).unwrap();
    assert!(second.starts_with(&first));
    assert_eq!(second.lines().count(), first.lines().count() + 1);
}

#[test]
fn append_operations_form_a_parent_chain_and_advance_the_leaf() {
    let mut manager = manager_in_memory();
    let one = manager.append_message(user("one")).unwrap();
    let thinking = manager.append_thinking_level_change("high").unwrap();
    let model = manager
        .append_model_change("deepseek", "deepseek-flash")
        .unwrap();
    let two = manager
        .append_message(assistant("two", "deepseek", "deepseek-flash"))
        .unwrap();
    assert_eq!(manager.get_leaf_id(), Some(two.as_str()));
    let branch = manager.get_branch();
    assert_eq!(
        branch.iter().map(|entry| entry.id()).collect::<Vec<_>>(),
        vec![
            one.as_str(),
            thinking.as_str(),
            model.as_str(),
            two.as_str()
        ]
    );
    assert_eq!(branch[0].parent_id(), None);
    assert_eq!(branch[1].parent_id(), Some(one.as_str()));
    assert_eq!(manager.get_leaf_entry().unwrap().id(), two);
}

#[test]
fn branch_moves_the_leaf_and_new_appends_become_children() {
    let mut manager = manager_in_memory();
    let one = manager.append_message(user("one")).unwrap();
    manager
        .append_message(assistant("two", "deepseek", "deepseek-flash"))
        .unwrap();
    manager.branch(&one).unwrap();
    let other = manager.append_message(user("edited")).unwrap();
    assert_eq!(manager.get_leaf_id(), Some(other.as_str()));
    let children = manager.get_children(&one);
    assert_eq!(children.len(), 2);
    assert!(manager.branch("missing").is_err());
}

#[test]
fn reset_leaf_makes_the_next_append_a_root() {
    let mut manager = manager_in_memory();
    manager.append_message(user("one")).unwrap();
    manager.reset_leaf();
    let root = manager.append_message(user("again")).unwrap();
    assert_eq!(manager.get_entry(&root).unwrap().parent_id(), None);
}

#[test]
fn branch_with_summary_records_the_source_and_destination() {
    let mut manager = manager_in_memory();
    let one = manager.append_message(user("one")).unwrap();
    let two = manager
        .append_message(assistant("two", "deepseek", "deepseek-flash"))
        .unwrap();
    let summary = manager
        .append_branch_summary(Some(&one), "summary", None, None, None)
        .unwrap();
    let entry = manager.get_entry(&summary).unwrap();
    match entry {
        SessionEntry::BranchSummary(summary) => {
            assert_eq!(summary.from_id, two);
            assert_eq!(summary.summary, "summary");
        }
        _ => panic!("expected a branch summary"),
    }
    assert_eq!(manager.get_leaf_id(), Some(summary.as_str()));
}

#[test]
fn get_tree_returns_children_in_time_order() {
    let mut manager = manager_in_memory();
    let root = manager.append_message(user("root")).unwrap();
    manager
        .append_message(assistant("a", "deepseek", "deepseek-flash"))
        .unwrap();
    manager.branch(&root).unwrap();
    manager
        .append_message(assistant("b", "deepseek", "deepseek-flash"))
        .unwrap();
    let tree = manager.get_tree();
    assert_eq!(tree.len(), 1);
    assert_eq!(tree[0].entry.id(), root);
    assert_eq!(tree[0].children.len(), 2);
}

#[test]
fn sets_gets_and_clears_labels() {
    let mut manager = manager_in_memory();
    let one = manager.append_message(user("one")).unwrap();
    let two = manager.append_message(user("two")).unwrap();
    manager.append_label_change(&one, Some("first")).unwrap();
    assert_eq!(manager.get_label(&one), Some("first"));
    manager.append_label_change(&two, Some("second")).unwrap();
    manager.append_label_change(&one, Some("updated")).unwrap();
    assert_eq!(manager.get_label(&one), Some("updated"));
    manager.append_label_change(&one, None).unwrap();
    assert_eq!(manager.get_label(&one), None);
    assert!(manager.append_label_change("missing", Some("x")).is_err());
    // 标签条目不参与模型上下文。
    let context = manager.build_session_context();
    assert_eq!(context.messages.len(), 2);
}

#[test]
fn custom_entries_stay_out_of_context_but_custom_messages_join_it() {
    let mut manager = manager_in_memory();
    manager
        .append_custom_entry("state", Some(json!({"a": 1})))
        .unwrap();
    manager
        .append_custom_message_entry("note", json!("hello"), true, None)
        .unwrap();
    let context = manager.build_session_context();
    assert_eq!(context.messages.len(), 1);
    assert_eq!(context.messages[0].role(), "custom");
}

#[test]
fn build_context_follows_the_leaf_path_only() {
    let mut manager = manager_in_memory();
    let root = manager.append_message(user("root")).unwrap();
    manager
        .append_message(assistant("a", "deepseek", "deepseek-flash"))
        .unwrap();
    manager.branch(&root).unwrap();
    manager.append_message(user("b")).unwrap();
    let messages = manager.build_session_context().messages;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role(), "user");
    assert_eq!(messages[1].role(), "user");
}

#[test]
fn tracks_thinking_level_and_model_from_entries() {
    let mut manager = manager_in_memory();
    manager
        .append_model_change("deepseek", "deepseek-flash")
        .unwrap();
    manager.append_thinking_level_change("medium").unwrap();
    let context = manager.build_session_context();
    assert_eq!(context.thinking_level, "medium");
    assert_eq!(
        context.model,
        Some(("deepseek".to_string(), "deepseek-flash".to_string()))
    );

    let mut manager = manager_in_memory();
    manager
        .append_message(assistant("hi", "other", "other-model"))
        .unwrap();
    assert_eq!(
        manager.build_session_context().model,
        Some(("other".to_string(), "other-model".to_string()))
    );
}

#[test]
fn context_edits_replace_or_remove_earlier_messages() {
    let mut manager = manager_in_memory();
    let one = manager.append_message(user("long text")).unwrap();
    let two = manager
        .append_message(assistant("reply", "deepseek", "deepseek-flash"))
        .unwrap();
    manager
        .append_context_edit(
            &one,
            Some(
                xjpcode_coding_agent::core::session_manager::ContextEditReplacement {
                    content: json!("short"),
                },
            ),
        )
        .unwrap();
    let messages = manager.build_session_context().messages;
    assert_eq!(messages.len(), 2);
    match &messages[0] {
        AgentMessage::User(user) => match &user.content {
            UserContent::Text(text) => assert_eq!(text, "short"),
            _ => panic!("expected text content"),
        },
        _ => panic!("expected a user message"),
    }

    manager.append_context_edit(&two, None).unwrap();
    let messages = manager.build_session_context().messages;
    assert_eq!(messages.len(), 1);
}

#[test]
fn compaction_keeps_the_summary_and_the_kept_range() {
    let mut manager = manager_in_memory();
    manager.append_message(user("one")).unwrap();
    let kept = manager
        .append_message(assistant("two", "deepseek", "deepseek-flash"))
        .unwrap();
    manager.append_message(user("three")).unwrap();
    manager
        .append_compaction("summary", Some(&kept), 100, None, None, None)
        .unwrap();
    manager
        .append_message(assistant("four", "deepseek", "deepseek-flash"))
        .unwrap();

    let context = manager.build_session_context();
    assert_eq!(context.messages.len(), 4);
    assert_eq!(context.messages[0].role(), "compactionSummary");
    assert_eq!(context.messages[1].role(), "assistant");
    assert_eq!(context.messages[2].role(), "user");
    assert_eq!(context.messages[3].role(), "assistant");
}

#[test]
fn compaction_includes_the_system_message_at_the_boundary() {
    let dir = TempDir::new().unwrap();
    let mut manager = SessionManager::create(
        Path::new("/work"),
        Some(dir.path().to_path_buf()),
        NewSessionOptions::default(),
    )
    .unwrap();
    manager
        .append_message(AgentMessage::System(system_message(&[(
            "preamble", "hello",
        )])))
        .unwrap();
    let kept = manager.append_message(user("one")).unwrap();
    manager
        .append_compaction("summary", Some(&kept), 10, None, None, None)
        .unwrap();
    let messages = manager.build_session_context().messages;
    assert_eq!(messages[0].role(), "system");
    assert_eq!(messages[1].role(), "compactionSummary");
}

#[test]
fn uses_the_last_entry_when_the_leaf_is_not_found_and_tolerates_orphans() {
    let entries = vec![
        json!({"type":"message","id":"a","parentId":null,"timestamp":"2026-01-01T00:00:00.000Z","message":{"role":"user","content":[{"type":"text","text":"a"}],"timestamp":1}}),
        json!({"type":"message","id":"b","parentId":"missing","timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"b"}],"timestamp":2}}),
    ];
    let parsed: Vec<SessionEntry> = entries
        .iter()
        .map(|value| serde_json::from_value(value.clone()).unwrap())
        .collect();
    let by_id = parsed
        .iter()
        .map(|entry| (entry.id().to_string(), entry.clone()))
        .collect();
    let context = build_session_context(&parsed, Some("missing"), &by_id);
    assert_eq!(context.messages.len(), 1);
    assert_eq!(context.messages[0].role(), "user");
}

#[test]
fn reads_entries_and_skips_malformed_lines() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("session.jsonl");
    let content = r#"{"type":"session","version":3,"id":"abc","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/work"}
{"type":"message","id":"a","parentId":null,"timestamp":"2026-01-01T00:00:00.000Z","message":{"role":"user","content":"hi","timestamp":1}}
not json
{"type":"thinking_level_change","id":"b","parentId":"a","timestamp":"2026-01-01T00:00:01.000Z","thinkingLevel":"high"}
"#;
    fs::write(&path, content).unwrap();
    let entries = load_entries_from_file(&path);
    assert_eq!(entries.len(), 3);
}

#[test]
fn rejects_files_without_a_session_header() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(&path, "{\"type\":\"message\",\"id\":\"a\"}\n").unwrap();
    assert!(load_entries_from_file(&path).is_empty());
    assert!(SessionManager::open(&path, None, Some(Path::new("/work"))).is_err());
}

#[test]
fn opens_a_session_file_and_continues_its_tree() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("session.jsonl");
    let content = r#"{"type":"session","version":3,"id":"abc","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/work"}
{"type":"message","id":"a","parentId":null,"timestamp":"2026-01-01T00:00:00.000Z","message":{"role":"user","content":"hi","timestamp":1}}
"#;
    fs::write(&path, content).unwrap();
    let mut manager = SessionManager::open(&path, None, None).unwrap();
    assert_eq!(manager.get_session_id(), "abc");
    assert_eq!(manager.get_leaf_id(), Some("a"));
    let next = manager.append_message(user("again")).unwrap();
    assert_eq!(manager.get_entry(&next).unwrap().parent_id(), Some("a"));
    let reread = load_entries_from_file(&path);
    assert_eq!(reread.len(), 3);
}

#[test]
fn session_dir_encodes_the_working_directory() {
    let dir = get_default_session_dir(Path::new("/Users/me/proj"), Path::new("/home/me/.agent"));
    assert!(dir.ends_with("sessions/--Users-me-proj--"));
}

#[test]
fn system_message_entries_serialize_in_pi_shape() {
    let entry = SessionEntry::Message(
        xjpcode_coding_agent::core::session_manager::SessionMessageEntry {
            base: xjpcode_coding_agent::core::session_manager::SessionEntryBase {
                id: "a".to_string(),
                parent_id: None,
                timestamp: "2026-01-01T00:00:00.000Z".to_string(),
            },
            message: AgentMessage::System(system_message(&[("skills", "<skills>x</skills>")])),
        },
    );
    let value = serde_json::to_value(&entry).unwrap();
    assert_eq!(value["type"], "message");
    assert_eq!(value["id"], "a");
    assert_eq!(value["parentId"], serde_json::Value::Null);
    assert_eq!(value["message"]["role"], "system");
    assert_eq!(value["message"]["sections"]["skills"], "<skills>x</skills>");
}

#[test]
fn finds_the_most_recent_session_for_the_working_directory() {
    let dir = TempDir::new().unwrap();
    let mut work = SessionManager::create(
        Path::new("/work"),
        Some(dir.path().to_path_buf()),
        NewSessionOptions::default(),
    )
    .unwrap();
    work.append_message(user("work one")).unwrap();
    let work_id = work.get_session_id().to_string();

    let mut other = SessionManager::create(
        Path::new("/other"),
        Some(dir.path().to_path_buf()),
        NewSessionOptions::default(),
    )
    .unwrap();
    other.append_message(user("other one")).unwrap();

    // 新的那场属于别的目录，按 cwd 过滤时要回到 /work 的那一场。
    std::thread::sleep(std::time::Duration::from_millis(10));
    other.append_message(user("other two")).unwrap();

    let found = xjpcode_coding_agent::core::session_manager::find_most_recent_session(
        dir.path(),
        Some(Path::new("/work")),
    )
    .unwrap();
    let header = xjpcode_coding_agent::core::session_manager::read_session_header(&found).unwrap();
    assert_eq!(header["id"], work_id);
}

#[test]
fn continues_the_most_recent_session_or_starts_a_new_one() {
    let dir = TempDir::new().unwrap();
    let mut first = SessionManager::create(
        Path::new("/work"),
        Some(dir.path().to_path_buf()),
        NewSessionOptions::default(),
    )
    .unwrap();
    first.append_message(user("hello")).unwrap();
    let first_id = first.get_session_id().to_string();
    let first_file = first.get_session_file().unwrap().to_path_buf();
    drop(first);

    let mut continued = SessionManager::continue_recent(
        Path::new("/work"),
        Some(dir.path().to_path_buf()),
    )
    .unwrap();
    assert_eq!(continued.get_session_id(), first_id);
    assert_eq!(continued.get_session_file(), Some(first_file.as_path()));
    let branch = continued.get_branch();
    assert_eq!(branch.len(), 1);
    continued.append_message(user("again")).unwrap();
    let lines = std::fs::read_to_string(&first_file).unwrap();
    assert_eq!(lines.lines().count(), 3);
}

#[test]
fn finds_a_session_by_its_id_prefix() {
    let dir = TempDir::new().unwrap();
    let mut manager = SessionManager::create(
        Path::new("/work"),
        Some(dir.path().to_path_buf()),
        NewSessionOptions::default(),
    )
    .unwrap();
    manager.append_message(user("hello")).unwrap();
    let id = manager.get_session_id().to_string();
    let file = manager.get_session_file().unwrap().to_path_buf();
    let prefix = &id[..8];
    let found = xjpcode_coding_agent::core::session_manager::find_session_by_id(dir.path(), prefix);
    assert_eq!(found, Some(file));
    assert_eq!(
        xjpcode_coding_agent::core::session_manager::find_session_by_id(dir.path(), "nope"),
        None
    );
}

// 下面几组对应上游 `test/session-manager/file-operations.test.ts` 的 `findMostRecentSession`。

fn write_session_header(dir: &Path, name: &str, id: &str, cwd: &str) -> std::path::PathBuf {
    let file = dir.join(name);
    fs::write(
        &file,
        format!(
            "{{\"type\":\"session\",\"id\":\"{id}\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"{cwd}\"}}\n"
        ),
    )
    .unwrap();
    file
}

#[test]
fn finds_no_recent_session_in_an_empty_or_missing_directory() {
    let dir = TempDir::new().unwrap();
    assert_eq!(
        xjpcode_coding_agent::core::session_manager::find_most_recent_session(dir.path(), None),
        None
    );
    assert_eq!(
        xjpcode_coding_agent::core::session_manager::find_most_recent_session(
            &dir.path().join("nonexistent"),
            None
        ),
        None
    );
}

#[test]
fn ignores_non_jsonl_and_headerless_jsonl_files() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("file.txt"), "hello").unwrap();
    fs::write(dir.path().join("file.json"), "{}").unwrap();
    fs::write(dir.path().join("invalid.jsonl"), "{\"type\":\"message\"}\n").unwrap();
    assert_eq!(
        xjpcode_coding_agent::core::session_manager::find_most_recent_session(dir.path(), None),
        None
    );
}

#[test]
fn returns_the_most_recent_valid_session_and_skips_invalid_ones() {
    let dir = TempDir::new().unwrap();
    let older = write_session_header(dir.path(), "older.jsonl", "old", "/tmp");
    std::thread::sleep(std::time::Duration::from_millis(10));
    let invalid = dir.path().join("invalid.jsonl");
    fs::write(&invalid, "{\"type\":\"not-session\"}\n").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));
    let valid = write_session_header(dir.path(), "valid.jsonl", "new", "/tmp");

    let found = xjpcode_coding_agent::core::session_manager::find_most_recent_session(
        dir.path(),
        None,
    );
    assert_eq!(found.as_deref(), Some(valid.as_path()));
    // 把最新的那份改坏，应回到上一份有效文件。
    fs::write(&valid, "x".repeat(10)).unwrap();
    assert_eq!(
        xjpcode_coding_agent::core::session_manager::find_most_recent_session(dir.path(), None),
        Some(older)
    );
}

#[test]
fn filters_the_most_recent_session_by_working_directory() {
    let dir = TempDir::new().unwrap();
    write_session_header(dir.path(), "a.jsonl", "a", "/tmp/project-a");
    std::thread::sleep(std::time::Duration::from_millis(10));
    write_session_header(dir.path(), "b.jsonl", "b", "/tmp/project-b");
    let find = |cwd: &str| {
        xjpcode_coding_agent::core::session_manager::find_most_recent_session(
            dir.path(),
            Some(Path::new(cwd)),
        )
    };
    assert_eq!(find("/tmp/project-a"), Some(dir.path().join("a.jsonl")));
    assert_eq!(find("/tmp/project-b"), Some(dir.path().join("b.jsonl")));
}
