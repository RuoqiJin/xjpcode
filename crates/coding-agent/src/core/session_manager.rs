//! 对应上游 `src/core/session-manager.ts` 中本里程碑用到的部分：会话文件、追加、投影。
//!
//! 文件格式与上游逐字段相同（JSONL：第一行 header，其余每行一个带 `id`/`parentId` 的条目）。
//! 分支、标签、压缩与上下文编辑的读写都在；会话列举、迁移、fork 不在这一层（见
//! `docs/port/milestone-4.md` 差异表）。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use xjpcode_agent::AgentMessage;
use xjpcode_ai::types::Usage;
use xjpcode_ai::JsonObject;
use xjpcode_ai::{SystemContent, SystemMessage, SystemRole};

use crate::core::messages::{
    create_branch_summary_message, create_compaction_summary_message, create_custom_message,
};
use crate::utils::paths::{paths_equal, resolve_path, PathInputOptions};

pub const CURRENT_SESSION_VERSION: u32 = 3;

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn iso_to_ms(timestamp: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|value| value.timestamp_millis().max(0) as u64)
        .unwrap_or(0)
}

fn create_session_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// pi: `assertValidSessionId`
pub fn assert_valid_session_id(id: &str) -> Result<(), String> {
    let valid = !id.is_empty()
        && id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
        && id
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphanumeric())
        && id
            .chars()
            .last()
            .is_some_and(|character| character.is_ascii_alphanumeric());
    if valid {
        Ok(())
    } else {
        Err("Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character".to_string())
    }
}

fn generate_id(seen: &HashSet<String>) -> String {
    for _ in 0..100 {
        let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        if !seen.contains(&id) {
            return id;
        }
    }
    uuid::Uuid::new_v4().to_string()
}

/// pi: `SessionHeader`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHeader {
    pub r#type: String,
    pub version: u32,
    pub id: String,
    pub timestamp: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
}

/// pi: `SessionEntryBase`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEntryBase {
    pub id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMessageEntry {
    #[serde(flatten)]
    pub base: SessionEntryBase,
    pub message: AgentMessage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelChangeEntry {
    #[serde(flatten)]
    pub base: SessionEntryBase,
    pub thinking_level: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChangeEntry {
    #[serde(flatten)]
    pub base: SessionEntryBase,
    pub provider: String,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageEntry {
    #[serde(flatten)]
    pub base: SessionEntryBase,
    pub kind: String,
    pub provider: String,
    pub model: String,
    pub usage: Usage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionEntry {
    #[serde(flatten)]
    pub base: SessionEntryBase,
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_message: Option<SystemMessage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryEntry {
    #[serde(flatten)]
    pub base: SessionEntryBase,
    pub from_id: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_hook: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEntry {
    #[serde(flatten)]
    pub base: SessionEntryBase,
    pub custom_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessageEntry {
    #[serde(flatten)]
    pub base: SessionEntryBase,
    pub custom_type: String,
    pub content: Value,
    pub display: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEditEntry {
    #[serde(flatten)]
    pub base: SessionEntryBase,
    pub target_id: String,
    /// `null` 表示把目标从上下文里去掉。
    pub replacement: Option<ContextEditReplacement>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextEditReplacement {
    pub content: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelEntry {
    #[serde(flatten)]
    pub base: SessionEntryBase,
    pub target_id: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoEntry {
    #[serde(flatten)]
    pub base: SessionEntryBase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// pi: `SessionEntry`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEntry {
    Message(SessionMessageEntry),
    ThinkingLevelChange(ThinkingLevelChangeEntry),
    ModelChange(ModelChangeEntry),
    Usage(UsageEntry),
    Compaction(CompactionEntry),
    BranchSummary(BranchSummaryEntry),
    Custom(CustomEntry),
    CustomMessage(CustomMessageEntry),
    ContextEdit(ContextEditEntry),
    Label(LabelEntry),
    SessionInfo(SessionInfoEntry),
}

impl SessionEntry {
    pub fn base(&self) -> &SessionEntryBase {
        match self {
            Self::Message(entry) => &entry.base,
            Self::ThinkingLevelChange(entry) => &entry.base,
            Self::ModelChange(entry) => &entry.base,
            Self::Usage(entry) => &entry.base,
            Self::Compaction(entry) => &entry.base,
            Self::BranchSummary(entry) => &entry.base,
            Self::Custom(entry) => &entry.base,
            Self::CustomMessage(entry) => &entry.base,
            Self::ContextEdit(entry) => &entry.base,
            Self::Label(entry) => &entry.base,
            Self::SessionInfo(entry) => &entry.base,
        }
    }

    pub fn id(&self) -> &str {
        &self.base().id
    }

    pub fn parent_id(&self) -> Option<&str> {
        self.base().parent_id.as_deref()
    }

    pub fn timestamp(&self) -> &str {
        &self.base().timestamp
    }
}

/// pi: `SessionProjection`
#[derive(Debug, Clone, Default)]
pub struct SessionProjection {
    pub entries: Vec<ProjectedSessionEntry>,
    pub messages: Vec<AgentMessage>,
    pub thinking_level: String,
    pub model: Option<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ProjectedSessionEntry {
    pub source_entry: SessionEntry,
    pub messages: Vec<AgentMessage>,
}

/// pi: `SessionContext`
#[derive(Debug, Clone, Default)]
pub struct SessionContext {
    pub messages: Vec<AgentMessage>,
    pub thinking_level: String,
    pub model: Option<(String, String)>,
}

impl Default for SessionHeader {
    fn default() -> Self {
        Self {
            r#type: "session".to_string(),
            version: CURRENT_SESSION_VERSION,
            id: String::new(),
            timestamp: String::new(),
            cwd: String::new(),
            parent_session: None,
        }
    }
}

/// pi: `parseSessionEntries`
pub fn parse_session_entries(content: &str) -> Vec<Value> {
    content
        .trim()
        .split('\n')
        .filter_map(|line| {
            if line.trim().is_empty() {
                return None;
            }
            serde_json::from_str::<Value>(line).ok()
        })
        .collect()
}

/// pi: `loadEntriesFromFile`——首行必须是 session header；坏行跳过。
pub fn load_entries_from_file(file_path: &Path) -> Vec<Value> {
    let Ok(content) = std::fs::read_to_string(file_path) else {
        return Vec::new();
    };
    let entries = parse_session_entries(&content);
    let Some(first) = entries.first() else {
        return Vec::new();
    };
    if first.get("type").and_then(Value::as_str) != Some("session")
        || first.get("id").and_then(Value::as_str).is_none()
    {
        return Vec::new();
    }
    entries
}

/// pi: `buildSessionPath`
fn build_session_path(
    entries: &[SessionEntry],
    leaf_id: Option<&str>,
    by_id: &HashMap<String, SessionEntry>,
) -> Vec<SessionEntry> {
    let mut leaf = match leaf_id {
        Some(id) => by_id.get(id).cloned(),
        None => entries.last().cloned(),
    };
    if leaf.is_none() && leaf_id.is_some() {
        leaf = entries.last().cloned();
    }
    let mut path = Vec::new();
    let mut current = leaf;
    while let Some(entry) = current {
        let parent = entry.parent_id().map(ToString::to_string);
        path.push(entry);
        current = parent.and_then(|id| by_id.get(&id).cloned());
    }
    path.reverse();
    path
}

/// pi: `getSessionContextSettings`
fn get_session_context_settings(path: &[SessionEntry]) -> (String, Option<(String, String)>) {
    let mut thinking_level = "off".to_string();
    let mut model = None;
    for entry in path {
        match entry {
            SessionEntry::ThinkingLevelChange(entry) => {
                thinking_level = entry.thinking_level.clone();
            }
            SessionEntry::ModelChange(entry) => {
                model = Some((entry.provider.clone(), entry.model_id.clone()));
            }
            SessionEntry::Message(entry) => {
                if let AgentMessage::Assistant(assistant) = &entry.message {
                    model = Some((assistant.provider.clone(), assistant.model.clone()));
                }
            }
            _ => {}
        }
    }
    (thinking_level, model)
}

/// pi: `sessionEntryToContextMessages`
pub fn session_entry_to_context_messages(entry: &SessionEntry) -> Vec<AgentMessage> {
    match entry {
        SessionEntry::Message(entry) => vec![entry.message.clone()],
        SessionEntry::CustomMessage(entry) => vec![create_custom_message(
            &entry.custom_type,
            entry.content.clone(),
            entry.display,
            entry.details.clone(),
            iso_to_ms(entry.base.timestamp.as_str()),
        )],
        SessionEntry::BranchSummary(entry) if !entry.summary.is_empty() => {
            vec![create_branch_summary_message(
                &entry.summary,
                &entry.from_id,
                iso_to_ms(entry.base.timestamp.as_str()),
            )]
        }
        SessionEntry::Compaction(entry) => {
            let summary = create_compaction_summary_message(
                &entry.summary,
                entry.tokens_before,
                iso_to_ms(entry.base.timestamp.as_str()),
            );
            match &entry.system_message {
                Some(system_message) => {
                    vec![AgentMessage::System(system_message.clone()), summary]
                }
                None => vec![summary],
            }
        }
        _ => Vec::new(),
    }
}

/// pi: `projectContextEntry`
pub fn project_context_entry(
    entry: &SessionEntry,
    edit: Option<&ContextEditEntry>,
) -> Vec<AgentMessage> {
    let messages = session_entry_to_context_messages(entry);
    let Some(edit) = edit else {
        return messages;
    };
    let Some(replacement) = &edit.replacement else {
        return Vec::new();
    };
    messages
        .into_iter()
        .filter_map(|message| match message {
            AgentMessage::User(mut user) => {
                user.content = replacement_content(&replacement.content);
                Some(AgentMessage::User(user))
            }
            AgentMessage::Assistant(mut assistant) => {
                assistant.content = replacement_blocks(&replacement.content);
                Some(AgentMessage::Assistant(assistant))
            }
            AgentMessage::ToolResult(mut result) => {
                result.content = match &replacement.content {
                    Value::String(text) => {
                        vec![xjpcode_ai::ToolResultContentBlock::Text(
                            xjpcode_ai::TextContent::new(text.clone()),
                        )]
                    }
                    Value::Array(blocks) => blocks
                        .iter()
                        .filter_map(|block| serde_json::from_value(block.clone()).ok())
                        .collect(),
                    _ => result.content,
                };
                Some(AgentMessage::ToolResult(result))
            }
            AgentMessage::Custom(mut custom) => {
                custom
                    .payload
                    .insert("content".to_string(), replacement.content.clone());
                Some(AgentMessage::Custom(custom))
            }
            other => Some(other),
        })
        .collect()
}

fn replacement_content(value: &Value) -> xjpcode_ai::UserContent {
    match value {
        Value::String(text) => xjpcode_ai::UserContent::Text(text.clone()),
        Value::Array(blocks) => xjpcode_ai::UserContent::Blocks(
            blocks
                .iter()
                .filter_map(|block| serde_json::from_value(block.clone()).ok())
                .collect(),
        ),
        _ => xjpcode_ai::UserContent::Text(String::new()),
    }
}

fn replacement_blocks(value: &Value) -> Vec<xjpcode_ai::AssistantContentBlock> {
    match value {
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| serde_json::from_value(block.clone()).ok())
            .collect(),
        Value::String(text) => vec![xjpcode_ai::AssistantContentBlock::Text(
            xjpcode_ai::TextContent::new(text.clone()),
        )],
        _ => Vec::new(),
    }
}

/// pi: `buildContextEntries`——最新一条压缩条目取代它之前的旧条目。
pub fn build_context_entries(
    entries: &[SessionEntry],
    leaf_id: Option<&str>,
    by_id: &HashMap<String, SessionEntry>,
) -> Vec<SessionEntry> {
    let path = build_session_path(entries, leaf_id, by_id);
    let compaction = path
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::Compaction(compaction) => Some(compaction),
            _ => None,
        })
        .last();
    let Some(compaction) = compaction else {
        return path;
    };
    let Some(compaction_index) = path
        .iter()
        .position(|entry| entry.id() == compaction.base.id)
    else {
        return path;
    };

    let mut context_entries = vec![SessionEntry::Compaction(compaction.clone())];
    let mut found_first_kept = false;
    for entry in &path[..compaction_index] {
        if entry.id() == compaction.first_kept_entry_id {
            found_first_kept = true;
        }
        let is_system_message = matches!(
            entry,
            SessionEntry::Message(message)
                if matches!(message.message, AgentMessage::System(_))
        );
        if found_first_kept && !is_system_message {
            context_entries.push(entry.clone());
        }
    }
    context_entries.extend(path[compaction_index + 1..].iter().cloned());
    context_entries
}

/// pi: `buildSessionProjection`
pub fn build_session_projection(
    entries: &[SessionEntry],
    leaf_id: Option<&str>,
    by_id: &HashMap<String, SessionEntry>,
) -> SessionProjection {
    let path = build_session_path(entries, leaf_id, by_id);
    let (thinking_level, model) = get_session_context_settings(&path);
    let context_entries = build_context_entries(entries, leaf_id, by_id);
    let mut edits: HashMap<String, ContextEditEntry> = HashMap::new();
    for entry in &context_entries {
        if let SessionEntry::ContextEdit(edit) = entry {
            edits.insert(edit.target_id.clone(), edit.clone());
        }
    }
    let projected_entries: Vec<ProjectedSessionEntry> = context_entries
        .into_iter()
        .enumerate()
        .map(|(index, source_entry)| {
            let messages = if matches!(source_entry, SessionEntry::Compaction(_)) && index > 0 {
                Vec::new()
            } else {
                let edit = match &source_entry {
                    SessionEntry::ContextEdit(_) => None,
                    _ => edits.get(source_entry.id()),
                };
                project_context_entry(&source_entry, edit)
            };
            ProjectedSessionEntry {
                source_entry,
                messages,
            }
        })
        .collect();
    let messages = projected_entries
        .iter()
        .flat_map(|entry| entry.messages.iter().cloned())
        .collect();
    SessionProjection {
        entries: projected_entries,
        messages,
        thinking_level,
        model,
    }
}

/// 便利：把一串已经排好的路径条目当成整条分支（上游 `buildSessionContext(pathEntries)` 的重载）。
pub fn build_session_context_for_path(entries: &[SessionEntry]) -> SessionContext {
    let by_id: HashMap<String, SessionEntry> = entries
        .iter()
        .map(|entry| (entry.id().to_string(), entry.clone()))
        .collect();
    let leaf_id = entries.last().map(|entry| entry.id().to_string());
    build_session_context(entries, leaf_id.as_deref(), &by_id)
}

/// pi: `buildSessionContext`
pub fn build_session_context(
    entries: &[SessionEntry],
    leaf_id: Option<&str>,
    by_id: &HashMap<String, SessionEntry>,
) -> SessionContext {
    let projection = build_session_projection(entries, leaf_id, by_id);
    SessionContext {
        messages: projection.messages,
        thinking_level: projection.thinking_level,
        model: projection.model,
    }
}

/// pi: `getDefaultSessionDirPath`——把 cwd 编码成一个安全的目录名。
fn get_default_session_dir_path(cwd: &Path, agent_dir: &Path) -> PathBuf {
    let resolved_cwd = resolve_path(
        &cwd.to_string_lossy(),
        &std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy(),
        &PathInputOptions::with_defaults(),
    );
    let resolved_agent_dir = resolve_path(
        &agent_dir.to_string_lossy(),
        &resolved_cwd.to_string_lossy(),
        &PathInputOptions::with_defaults(),
    );
    let path = resolved_cwd.to_string_lossy();
    let without_root = path.trim_start_matches(['/', '\\']);
    let safe_path = format!("--{}--", without_root.replace(['/', '\\', ':'], "-"));
    resolved_agent_dir.join("sessions").join(safe_path)
}

/// pi: `getDefaultSessionDir`
pub fn get_default_session_dir(cwd: &Path, agent_dir: &Path) -> PathBuf {
    let dir = get_default_session_dir_path(cwd, agent_dir);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// pi: `readSessionHeader`——只看第一行，坏文件当没有。
pub fn read_session_header(path: &Path) -> Option<Value> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(file).read_line(&mut line).ok()?;
    let value: Value = serde_json::from_str(&line).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("session") {
        return None;
    }
    Some(value)
}

/// pi: `findMostRecentSession`——按 mtime 从新到旧，可只用某个 cwd 的会话。
pub fn find_most_recent_session(session_dir: &Path, cwd: Option<&Path>) -> Option<PathBuf> {
    let resolved_cwd = cwd.map(|cwd| {
        resolve_path(
            &cwd.to_string_lossy(),
            &std::env::current_dir().unwrap_or_default().to_string_lossy(),
            &PathInputOptions::with_defaults(),
        )
    });
    let mut files: Vec<(PathBuf, std::time::SystemTime)> = std::fs::read_dir(session_dir)
        .ok()?
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
                return None;
            }
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            Some((path, modified))
        })
        .collect();
    files.sort_by(|left, right| right.1.cmp(&left.1));
    for (path, _) in files {
        let Some(header) = read_session_header(&path) else {
            continue;
        };
        if let Some(resolved_cwd) = &resolved_cwd {
            let header_cwd = header.get("cwd").and_then(Value::as_str);
            let Some(header_cwd) = header_cwd.filter(|cwd| !cwd.is_empty()) else {
                continue;
            };
            let header_cwd = resolve_path(
                header_cwd,
                &std::env::current_dir().unwrap_or_default().to_string_lossy(),
                &PathInputOptions::with_defaults(),
            );
            if !paths_equal(&header_cwd, resolved_cwd) {
                continue;
            }
        }
        return Some(path);
    }
    None
}

/// 按会话 id 前缀在目录里找一场（`--session 编号前几位`）。
pub fn find_session_by_id(session_dir: &Path, id_prefix: &str) -> Option<PathBuf> {
    if id_prefix.is_empty() {
        return None;
    }
    let mut files: Vec<(PathBuf, std::time::SystemTime)> = std::fs::read_dir(session_dir)
        .ok()?
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
                return None;
            }
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            Some((path, modified))
        })
        .collect();
    files.sort_by(|left, right| right.1.cmp(&left.1));
    for (path, _) in files {
        let header = read_session_header(&path);
        let header_id = header
            .as_ref()
            .and_then(|header| header.get("id"))
            .and_then(Value::as_str);
        let file_id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.rsplit_once('_').map(|(_, id)| id));
        if header_id.is_some_and(|id| id.starts_with(id_prefix))
            || file_id.is_some_and(|id| id.starts_with(id_prefix))
        {
            return Some(path);
        }
    }
    None
}

/// `NewSessionOptions`
#[derive(Debug, Clone, Default)]
pub struct NewSessionOptions {
    pub id: Option<String>,
    pub parent_session: Option<String>,
}

/// pi: `SessionManager`（本里程碑的这一半）
pub struct SessionManager {
    session_id: String,
    session_file: Option<PathBuf>,
    session_dir: PathBuf,
    cwd: PathBuf,
    persist: bool,
    flushed: bool,
    file_entries: Vec<SessionEntry>,
    header: Option<SessionHeader>,
    by_id: HashMap<String, SessionEntry>,
    labels_by_id: HashMap<String, String>,
    label_timestamps_by_id: HashMap<String, String>,
    leaf_id: Option<String>,
}

impl SessionManager {
    fn new(
        cwd: PathBuf,
        session_dir: PathBuf,
        session_file: Option<PathBuf>,
        persist: bool,
        options: NewSessionOptions,
        preloaded: Option<Vec<Value>>,
    ) -> Result<Self, String> {
        let resolved_cwd = resolve_path(
            &cwd.to_string_lossy(),
            &std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy(),
            &PathInputOptions::with_defaults(),
        );
        let mut manager = Self {
            session_id: String::new(),
            session_file: None,
            session_dir,
            cwd: resolved_cwd,
            persist,
            flushed: false,
            file_entries: Vec::new(),
            header: None,
            by_id: HashMap::new(),
            labels_by_id: HashMap::new(),
            label_timestamps_by_id: HashMap::new(),
            leaf_id: None,
        };
        if manager.persist {
            let _ = std::fs::create_dir_all(&manager.session_dir);
        }
        if let Some(file) = session_file {
            manager.set_session_file_internal(&file, preloaded)?;
        } else if let Some(entries) = preloaded.filter(|entries| !entries.is_empty()) {
            manager.load_entries(entries, options);
        } else {
            manager.new_session(options)?;
        }
        Ok(manager)
    }

    /// pi: `SessionManager.create`
    pub fn create(
        cwd: &Path,
        session_dir: Option<PathBuf>,
        options: NewSessionOptions,
    ) -> Result<Self, String> {
        let dir = session_dir
            .unwrap_or_else(|| get_default_session_dir(cwd, &crate::config::get_agent_dir()));
        Self::new(cwd.to_path_buf(), dir, None, true, options, None)
    }

    /// pi: `SessionManager.inMemory`
    pub fn in_memory(
        cwd: &Path,
        options: NewSessionOptions,
        preloaded: Option<Vec<Value>>,
    ) -> Result<Self, String> {
        Self::new(
            cwd.to_path_buf(),
            PathBuf::new(),
            None,
            false,
            options,
            preloaded,
        )
    }

    /// pi: `SessionManager.open`
    pub fn open(
        path: &Path,
        session_dir: Option<PathBuf>,
        cwd_override: Option<&Path>,
    ) -> Result<Self, String> {
        let entries = load_entries_from_file(path);
        let header = entries
            .first()
            .and_then(|value| serde_json::from_value::<SessionHeader>(value.clone()).ok());
        let cwd = cwd_override
            .map(Path::to_path_buf)
            .or_else(|| header.as_ref().map(|header| PathBuf::from(&header.cwd)))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let dir =
            session_dir.unwrap_or_else(|| path.parent().unwrap_or(Path::new(".")).to_path_buf());
        Self::new(
            cwd,
            dir,
            Some(path.to_path_buf()),
            true,
            NewSessionOptions::default(),
            Some(entries),
        )
    }

    /// pi: `SessionManager.continueRecent`——同目录里最近的一场，没有就新开一场。
    pub fn continue_recent(cwd: &Path, session_dir: Option<PathBuf>) -> Result<Self, String> {
        let dir = session_dir.unwrap_or_else(|| {
            get_default_session_dir(cwd, &crate::config::get_agent_dir())
        });
        let filter_cwd =
            dir != get_default_session_dir_path(cwd, &crate::config::get_agent_dir());
        let most_recent = find_most_recent_session(&dir, filter_cwd.then_some(cwd));
        match most_recent {
            Some(file) => Self::open(&file, Some(dir), None),
            None => Self::create(cwd, Some(dir), NewSessionOptions::default()),
        }
    }

    fn set_session_file_internal(
        &mut self,
        file: &Path,
        preloaded: Option<Vec<Value>>,
    ) -> Result<(), String> {
        self.session_file = Some(file.to_path_buf());
        if file.exists() {
            let entries = preloaded.unwrap_or_else(|| load_entries_from_file(file));
            if entries.is_empty() {
                if std::fs::metadata(file)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0)
                    > 0
                {
                    return Err(format!(
                        "Session file is not a valid xjpcode session: {}",
                        file.display()
                    ));
                }
                self.new_session(NewSessionOptions::default())?;
                self.session_file = Some(file.to_path_buf());
                self.rewrite_file();
                self.flushed = true;
                return Ok(());
            }
            self.load_entries(entries, NewSessionOptions::default());
            self.flushed = true;
            Ok(())
        } else {
            self.new_session(NewSessionOptions::default())?;
            self.session_file = Some(file.to_path_buf());
            Ok(())
        }
    }

    fn new_session(&mut self, options: NewSessionOptions) -> Result<(), String> {
        if let Some(id) = &options.id {
            assert_valid_session_id(id)?;
        }
        self.session_id = options.id.clone().unwrap_or_else(create_session_id);
        let timestamp = now_iso();
        let header = SessionHeader {
            r#type: "session".to_string(),
            version: CURRENT_SESSION_VERSION,
            id: self.session_id.clone(),
            timestamp: timestamp.clone(),
            cwd: self.cwd.to_string_lossy().into_owned(),
            parent_session: options.parent_session.clone(),
        };
        self.header = Some(header);
        self.file_entries.clear();
        self.by_id.clear();
        self.labels_by_id.clear();
        self.label_timestamps_by_id.clear();
        self.leaf_id = None;
        self.flushed = false;
        if self.persist {
            let file_timestamp = timestamp.replace([':', '.'], "-");
            self.session_file = Some(
                self.session_dir
                    .join(format!("{file_timestamp}_{}.jsonl", self.session_id)),
            );
        }
        Ok(())
    }

    fn load_entries(&mut self, entries: Vec<Value>, options: NewSessionOptions) {
        let header = entries
            .iter()
            .find_map(|value| serde_json::from_value::<SessionHeader>(value.clone()).ok());
        let first_is_header = entries
            .first()
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str)
            == Some("session");
        match header {
            Some(header) => {
                self.header = Some(header.clone());
                self.session_id = header.id;
                self.file_entries = entries
                    .iter()
                    .skip(usize::from(first_is_header))
                    .filter_map(|value| serde_json::from_value::<SessionEntry>(value.clone()).ok())
                    .collect();
                // 上游会在这里迁移旧版本；本里程碑不读 v1/v2（差异表）。
            }
            None => {
                let _ = self.new_session(options);
                self.file_entries = entries
                    .iter()
                    .filter_map(|value| serde_json::from_value::<SessionEntry>(value.clone()).ok())
                    .collect();
            }
        }
        self.build_index();
    }

    fn build_index(&mut self) {
        self.by_id.clear();
        self.labels_by_id.clear();
        self.label_timestamps_by_id.clear();
        self.leaf_id = None;
        for entry in &self.file_entries {
            self.by_id.insert(entry.id().to_string(), entry.clone());
            self.leaf_id = Some(entry.id().to_string());
            if let SessionEntry::Label(label) = entry {
                match &label.label {
                    Some(label_text) => {
                        self.labels_by_id
                            .insert(label.target_id.clone(), label_text.clone());
                        self.label_timestamps_by_id
                            .insert(label.target_id.clone(), label.base.timestamp.clone());
                    }
                    None => {
                        self.labels_by_id.remove(&label.target_id);
                        self.label_timestamps_by_id.remove(&label.target_id);
                    }
                }
            }
        }
    }

    fn rewrite_file(&self) {
        if !self.persist {
            return;
        }
        let Some(file) = &self.session_file else {
            return;
        };
        let mut content = String::new();
        if let Some(header) = &self.header {
            if let Ok(line) = serde_json::to_string(header) {
                content.push_str(&line);
                content.push('\n');
            }
        }
        for entry in &self.file_entries {
            if let Ok(line) = serde_json::to_string(entry) {
                content.push_str(&line);
                content.push('\n');
            }
        }
        let _ = std::fs::write(file, content);
    }

    fn has_conversation(&self) -> bool {
        self.file_entries.iter().any(|entry| match entry {
            SessionEntry::Message(message) => {
                matches!(
                    message.message,
                    AgentMessage::User(_) | AgentMessage::Assistant(_)
                )
            }
            _ => false,
        })
    }

    fn persist_entry(&mut self, entry: &SessionEntry) -> Result<(), String> {
        if !self.persist {
            return Ok(());
        }
        let Some(file) = &self.session_file else {
            return Ok(());
        };
        if !self.flushed {
            if !self.has_conversation() {
                return Ok(());
            }
            let mut content = String::new();
            if let Some(header) = &self.header {
                content
                    .push_str(&serde_json::to_string(header).map_err(|error| error.to_string())?);
                content.push('\n');
            }
            for existing in &self.file_entries {
                content
                    .push_str(&serde_json::to_string(existing).map_err(|error| error.to_string())?);
                content.push('\n');
            }
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(file)
                .and_then(|mut handle| {
                    use std::io::Write;
                    handle.write_all(content.as_bytes())
                })
                .map_err(|error| error.to_string())?;
            self.flushed = true;
        } else {
            use std::io::Write;
            let line = serde_json::to_string(entry).map_err(|error| error.to_string())?;
            let mut handle = std::fs::OpenOptions::new()
                .append(true)
                .open(file)
                .map_err(|error| error.to_string())?;
            writeln!(handle, "{line}").map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn append_entry(&mut self, entry: SessionEntry) -> Result<String, String> {
        let previous_leaf = self.leaf_id.clone();
        let id = entry.id().to_string();
        self.file_entries.push(entry.clone());
        self.by_id.insert(id.clone(), entry.clone());
        self.leaf_id = Some(id.clone());
        if let Err(error) = self.persist_entry(&entry) {
            self.file_entries.pop();
            self.by_id.remove(&id);
            self.leaf_id = previous_leaf;
            return Err(error);
        }
        Ok(id)
    }

    fn make_base(&self) -> SessionEntryBase {
        let seen: HashSet<String> = self.by_id.keys().cloned().collect();
        SessionEntryBase {
            id: generate_id(&seen),
            parent_id: self.leaf_id.clone(),
            timestamp: now_iso(),
        }
    }

    /// pi: `appendMessage`
    pub fn append_message(&mut self, message: AgentMessage) -> Result<String, String> {
        let entry = SessionEntry::Message(SessionMessageEntry {
            base: self.make_base(),
            message,
        });
        self.append_entry(entry)
    }

    /// pi: `appendThinkingLevelChange`
    pub fn append_thinking_level_change(&mut self, thinking_level: &str) -> Result<String, String> {
        let entry = SessionEntry::ThinkingLevelChange(ThinkingLevelChangeEntry {
            base: self.make_base(),
            thinking_level: thinking_level.to_string(),
        });
        self.append_entry(entry)
    }

    /// pi: `appendModelChange`
    pub fn append_model_change(
        &mut self,
        provider: &str,
        model_id: &str,
    ) -> Result<String, String> {
        let entry = SessionEntry::ModelChange(ModelChangeEntry {
            base: self.make_base(),
            provider: provider.to_string(),
            model_id: model_id.to_string(),
        });
        self.append_entry(entry)
    }

    /// pi: `appendUsage`
    pub fn append_usage(
        &mut self,
        kind: &str,
        provider: &str,
        model: &str,
        usage: Usage,
        note: Option<String>,
    ) -> Result<UsageEntry, String> {
        let entry = UsageEntry {
            base: self.make_base(),
            kind: kind.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            usage,
            note,
        };
        let id = self.append_entry(SessionEntry::Usage(entry.clone()))?;
        let mut entry = entry;
        entry.base.id = id;
        Ok(entry)
    }

    /// pi: `appendCompaction`
    pub fn append_compaction(
        &mut self,
        summary: &str,
        first_kept_entry_id: Option<&str>,
        tokens_before: u64,
        details: Option<Value>,
        from_hook: Option<bool>,
        usage: Option<Usage>,
    ) -> Result<String, String> {
        let base = self.make_base();
        let id = base.id.clone();
        let system_messages: Vec<xjpcode_ai::Message> =
            build_session_projection(&self.file_entries, self.leaf_id.as_deref(), &self.by_id)
                .messages
                .iter()
                .filter_map(|message| match message {
                    AgentMessage::System(system_message) => {
                        Some(xjpcode_ai::Message::System(system_message.clone()))
                    }
                    _ => None,
                })
                .collect();
        let system_message =
            xjpcode_ai::get_current_system_message(&system_messages).map(|mut message| {
                message.timestamp = iso_to_ms(&base.timestamp);
                message
            });
        let entry = SessionEntry::Compaction(CompactionEntry {
            base,
            summary: summary.to_string(),
            first_kept_entry_id: first_kept_entry_id.unwrap_or(&id).to_string(),
            tokens_before,
            details,
            usage,
            from_hook,
            system_message,
        });
        self.append_entry(entry)
    }

    /// pi: `appendCustomEntry`
    pub fn append_custom_entry(
        &mut self,
        custom_type: &str,
        data: Option<Value>,
    ) -> Result<String, String> {
        let entry = SessionEntry::Custom(CustomEntry {
            base: self.make_base(),
            custom_type: custom_type.to_string(),
            data,
        });
        self.append_entry(entry)
    }

    /// pi: `appendSessionInfo`
    pub fn append_session_info(&mut self, name: &str) -> Result<String, String> {
        let sanitized = name.replace(['\r', '\n'], " ");
        let entry = SessionEntry::SessionInfo(SessionInfoEntry {
            base: self.make_base(),
            name: Some(sanitized.trim().to_string()),
        });
        self.append_entry(entry)
    }

    /// pi: `getSessionName`
    pub fn get_session_name(&self) -> Option<String> {
        self.file_entries
            .iter()
            .rev()
            .find_map(|entry| match entry {
                SessionEntry::SessionInfo(info) => info
                    .name
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(ToString::to_string),
                _ => None,
            })
    }

    /// pi: `appendCustomMessageEntry`
    pub fn append_custom_message_entry(
        &mut self,
        custom_type: &str,
        content: Value,
        display: bool,
        details: Option<Value>,
    ) -> Result<String, String> {
        let entry = SessionEntry::CustomMessage(CustomMessageEntry {
            base: self.make_base(),
            custom_type: custom_type.to_string(),
            content,
            display,
            details,
        });
        self.append_entry(entry)
    }

    /// pi: `appendContextEdit`
    pub fn append_context_edit(
        &mut self,
        target_id: &str,
        replacement: Option<ContextEditReplacement>,
    ) -> Result<String, String> {
        let target = self
            .by_id
            .get(target_id)
            .ok_or_else(|| format!("Entry {target_id} not found"))?;
        if !self
            .get_branch()
            .iter()
            .any(|entry| entry.id() == target_id)
        {
            return Err(format!("Entry {target_id} is not on the active branch"));
        }
        let editable = match target {
            SessionEntry::CustomMessage(_) => true,
            SessionEntry::Message(message) => matches!(
                message.message,
                AgentMessage::User(_) | AgentMessage::Assistant(_) | AgentMessage::ToolResult(_)
            ),
            _ => false,
        };
        if !editable {
            return Err(format!(
                "Entry {target_id} does not contribute editable model content"
            ));
        }
        let entry = SessionEntry::ContextEdit(ContextEditEntry {
            base: self.make_base(),
            target_id: target_id.to_string(),
            replacement,
        });
        self.append_entry(entry)
    }

    /// pi: `appendLabelChange`
    pub fn append_label_change(
        &mut self,
        target_id: &str,
        label: Option<&str>,
    ) -> Result<String, String> {
        if !self.by_id.contains_key(target_id) {
            return Err(format!("Entry {target_id} not found"));
        }
        let entry = SessionEntry::Label(LabelEntry {
            base: self.make_base(),
            target_id: target_id.to_string(),
            label: label.map(ToString::to_string),
        });
        let id = entry.id().to_string();
        let timestamp = entry.timestamp().to_string();
        self.append_entry(entry)?;
        match label {
            Some(label) if !label.is_empty() => {
                self.labels_by_id
                    .insert(target_id.to_string(), label.to_string());
                self.label_timestamps_by_id
                    .insert(target_id.to_string(), timestamp);
            }
            _ => {
                self.labels_by_id.remove(target_id);
                self.label_timestamps_by_id.remove(target_id);
            }
        }
        Ok(id)
    }

    /// pi: `appendBranchSummary`
    pub fn append_branch_summary(
        &mut self,
        branch_from_id: Option<&str>,
        summary: &str,
        details: Option<Value>,
        from_hook: Option<bool>,
        usage: Option<Usage>,
    ) -> Result<String, String> {
        if let Some(id) = branch_from_id {
            if !self.by_id.contains_key(id) {
                return Err(format!("Entry {id} not found"));
            }
        }
        let from_id = self.leaf_id.clone().unwrap_or_else(|| "root".to_string());
        self.leaf_id = branch_from_id.map(ToString::to_string);
        let entry = SessionEntry::BranchSummary(BranchSummaryEntry {
            base: SessionEntryBase {
                id: generate_id(&self.by_id.keys().cloned().collect()),
                parent_id: branch_from_id.map(ToString::to_string),
                timestamp: now_iso(),
            },
            from_id,
            summary: summary.to_string(),
            details,
            usage,
            from_hook,
        });
        self.append_entry(entry)
    }

    /// pi: `branch`
    pub fn branch(&mut self, branch_from_id: &str) -> Result<(), String> {
        if !self.by_id.contains_key(branch_from_id) {
            return Err(format!("Entry {branch_from_id} not found"));
        }
        self.leaf_id = Some(branch_from_id.to_string());
        Ok(())
    }

    /// pi: `resetLeaf`
    pub fn reset_leaf(&mut self) {
        self.leaf_id = None;
    }

    pub fn get_leaf_id(&self) -> Option<&str> {
        self.leaf_id.as_deref()
    }

    pub fn get_leaf_entry(&self) -> Option<&SessionEntry> {
        self.leaf_id.as_ref().and_then(|id| self.by_id.get(id))
    }

    pub fn get_entry(&self, id: &str) -> Option<&SessionEntry> {
        self.by_id.get(id)
    }

    pub fn get_label(&self, id: &str) -> Option<&str> {
        self.labels_by_id.get(id).map(String::as_str)
    }

    /// pi: `getBranch`
    pub fn get_branch(&self) -> Vec<SessionEntry> {
        let mut path = Vec::new();
        let mut current = self
            .leaf_id
            .as_ref()
            .and_then(|id| self.by_id.get(id))
            .cloned();
        while let Some(entry) = current {
            let parent = entry.parent_id().map(ToString::to_string);
            path.push(entry);
            current = parent.and_then(|id| self.by_id.get(&id).cloned());
        }
        path.reverse();
        path
    }

    pub fn get_children(&self, parent_id: &str) -> Vec<&SessionEntry> {
        self.file_entries
            .iter()
            .filter(|entry| entry.parent_id() == Some(parent_id))
            .collect()
    }

    pub fn build_context_entries(&self) -> Vec<SessionEntry> {
        build_context_entries(&self.file_entries, self.leaf_id.as_deref(), &self.by_id)
    }

    pub fn build_session_projection(&self) -> SessionProjection {
        build_session_projection(&self.file_entries, self.leaf_id.as_deref(), &self.by_id)
    }

    pub fn build_session_context(&self) -> SessionContext {
        build_session_context(&self.file_entries, self.leaf_id.as_deref(), &self.by_id)
    }

    pub fn get_header(&self) -> Option<&SessionHeader> {
        self.header.as_ref()
    }

    pub fn get_entries(&self) -> &[SessionEntry] {
        &self.file_entries
    }

    pub fn get_session_id(&self) -> &str {
        &self.session_id
    }

    pub fn get_cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn get_session_dir(&self) -> &Path {
        &self.session_dir
    }

    pub fn get_session_file(&self) -> Option<&Path> {
        self.session_file.as_deref()
    }

    pub fn is_persisted(&self) -> bool {
        self.persist
    }

    pub fn set_session_file(&mut self, file: &Path) -> Result<(), String> {
        self.set_session_file_internal(file, None)
    }

    /// pi: `getTree`——子节点按时间排序。
    pub fn get_tree(&self) -> Vec<SessionTreeNode> {
        let mut by_id: HashMap<String, &SessionEntry> = HashMap::new();
        for entry in &self.file_entries {
            by_id.insert(entry.id().to_string(), entry);
        }
        let mut children: HashMap<Option<String>, Vec<String>> = HashMap::new();
        let mut roots: Vec<String> = Vec::new();
        for entry in &self.file_entries {
            let parent = entry.parent_id().map(ToString::to_string);
            match &parent {
                None => roots.push(entry.id().to_string()),
                Some(parent_id) if parent_id == entry.id() => roots.push(entry.id().to_string()),
                Some(parent_id) => {
                    if by_id.contains_key(parent_id) {
                        children
                            .entry(Some(parent_id.clone()))
                            .or_default()
                            .push(entry.id().to_string());
                    } else {
                        roots.push(entry.id().to_string());
                    }
                }
            }
        }
        fn build_node(
            id: &str,
            by_id: &HashMap<String, &SessionEntry>,
            children: &mut HashMap<Option<String>, Vec<String>>,
            labels: &HashMap<String, String>,
            label_timestamps: &HashMap<String, String>,
        ) -> Option<SessionTreeNode> {
            let entry = (*by_id.get(id)?).clone();
            let mut child_ids = children.remove(&Some(id.to_string())).unwrap_or_default();
            child_ids.sort_by_key(|child_id| {
                by_id
                    .get(child_id)
                    .map(|entry| iso_to_ms(entry.timestamp()))
                    .unwrap_or(0)
            });
            let child_nodes: Vec<SessionTreeNode> = child_ids
                .iter()
                .filter_map(|child_id| {
                    build_node(child_id, by_id, children, labels, label_timestamps)
                })
                .collect();
            Some(SessionTreeNode {
                entry,
                children: child_nodes,
                label: labels.get(id).cloned(),
                label_timestamp: label_timestamps.get(id).cloned(),
            })
        }
        roots
            .iter()
            .filter_map(|id| {
                build_node(
                    id,
                    &by_id,
                    &mut children,
                    &self.labels_by_id,
                    &self.label_timestamps_by_id,
                )
            })
            .collect()
    }

    /// 会话文件里那一行 header 是否与内存一致（供测试与诊断）。
    pub fn header_is_session(&self) -> bool {
        self.header
            .as_ref()
            .is_some_and(|header| header.r#type == "session")
    }

    /// 把重放出来的系统消息换成会话里的那一份（压缩条目用）。
    pub fn current_system_message(&self) -> Option<SystemMessage> {
        let projection = self.build_session_projection();
        projection
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                AgentMessage::System(system) => Some(system.clone()),
                _ => None,
            })
    }

    /// 只给测试：写一条内存里的系统消息，格式与读回来的一致。
    pub fn system_message_entry(&self, message: SystemMessage) -> SessionEntry {
        SessionEntry::Message(SessionMessageEntry {
            base: self.make_base(),
            message: AgentMessage::System(message),
        })
    }
}

#[derive(Clone)]
pub struct SessionTreeNode {
    pub entry: SessionEntry,
    pub children: Vec<SessionTreeNode>,
    pub label: Option<String>,
    pub label_timestamp: Option<String>,
}

/// 把一条消息的 UI 文本取出来（会话列举用，暂不实现列举）。
pub fn content_text(message: &AgentMessage) -> Option<String> {
    match message {
        AgentMessage::User(user) => Some(match &user.content {
            xjpcode_ai::UserContent::Text(text) => text.clone(),
            xjpcode_ai::UserContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    xjpcode_ai::UserContentBlock::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
        }),
        AgentMessage::Assistant(assistant) => Some(
            assistant
                .content
                .iter()
                .filter_map(|block| match block {
                    xjpcode_ai::AssistantContentBlock::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

/// 供 CLI 判断目录是否就是默认目录。
pub fn is_default_session_dir(cwd: &Path, agent_dir: &Path, session_dir: &Path) -> bool {
    paths_equal(session_dir, &get_default_session_dir_path(cwd, agent_dir))
}

/// 空系统消息（压缩用）。
pub fn empty_system_message() -> SystemMessage {
    SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text(String::new()),
        sections: None,
        tools_added: None,
        tools_removed: None,
        timestamp: 0,
    }
}

/// 给测试构造 JSON 条目时的字段名常量（避免拼错）。
pub fn entry_json(base: &SessionEntryBase) -> JsonObject {
    let mut object = JsonObject::new();
    object.insert("id".to_string(), Value::String(base.id.clone()));
    object.insert(
        "parentId".to_string(),
        base.parent_id
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    object.insert(
        "timestamp".to_string(),
        Value::String(base.timestamp.clone()),
    );
    object
}
