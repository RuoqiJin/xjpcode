use std::path::{Path, PathBuf};

use xjpcode_agent::AgentMessage;
use xjpcode_coding_agent::core::session_manager::{
    build_session_context_for_path, load_entries_from_file, read_session_header, SessionEntry,
};

use super::picker::{PickerItem, PickerKind, PickerState};

pub struct SessionListItem {
    pub id: String,
    pub path: PathBuf,
    pub label: String,
    pub message_count: usize,
    pub modified: std::time::SystemTime,
}

fn first_user_text(messages: &[AgentMessage]) -> Option<String> {
    messages.iter().find_map(|message| match message {
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
        _ => None,
    })
}

fn one_line(text: &str, max_chars: usize) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let trimmed = line.trim();
    let mut characters = trimmed.chars();
    let short: String = characters.by_ref().take(max_chars).collect();
    if characters.next().is_some() {
        format!("{short}…")
    } else if short.is_empty() {
        "(空会话)".to_string()
    } else {
        short
    }
}

/// 会话目录里按修改时间从新到旧列出会话。
pub fn list_sessions(session_dir: &Path) -> Vec<SessionListItem> {
    let Ok(entries) = std::fs::read_dir(session_dir) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(header) = read_session_header(&path) else {
            continue;
        };
        let Some(id) = header.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        let entries: Vec<SessionEntry> = load_entries_from_file(&path)
            .into_iter()
            .filter_map(|value| serde_json::from_value(value).ok())
            .collect();
        let messages = build_session_context_for_path(&entries).messages;
        let label = first_user_text(&messages)
            .map(|text| one_line(&text, 40))
            .unwrap_or_else(|| format!("会话 {}", &id[..id.len().min(8)]));
        items.push(SessionListItem {
            id: id.to_string(),
            path,
            label,
            message_count: messages.len(),
            modified,
        });
    }
    items.sort_by(|left, right| right.modified.cmp(&left.modified));
    items
}

pub fn build_session_picker(
    sessions: &[SessionListItem],
    current_session_id: &str,
) -> Option<PickerState> {
    if sessions.is_empty() {
        return None;
    }
    let mut index = 0;
    let items = sessions
        .iter()
        .enumerate()
        .map(|(position, session)| {
            let active = session.id == current_session_id;
            if active {
                index = position;
            }
            PickerItem {
                id: session.path.display().to_string(),
                label: session.label.clone(),
                detail: format!("ID: {} · {} 条消息", session.id, session.message_count),
                active,
            }
        })
        .collect();
    Some(PickerState {
        kind: PickerKind::Resume,
        title: "选择要恢复的历史会话 (↑/↓ 切换，Enter 确认)",
        items,
        index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_labels_are_shortened_to_one_line() {
        assert_eq!(one_line("第一行\n第二行", 40), "第一行");
        assert_eq!(one_line(&"字".repeat(50), 4), "字字字字…");
        assert_eq!(one_line("   ", 4), "(空会话)");
    }
}
