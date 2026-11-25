//! 对应上游 `packages/ai/src/utils/transcript.ts`。
//!
//! 对话记录怎么表达系统提示与工具表的增减：系统消息可以分段更新（`content` 追加、
//! `sections` 按名字替换/删除），工具表的变化用追加一条系统消息表达（`toolsAdded` /
//! `toolsRemoved`）。把全部系统消息按顺序重放，就得到当前的提示词与工具表。
//!
//! 顺序是可观察的（渲染出来的提示词、发给 provider 的工具表顺序都受它影响），
//! 因此工具表与片段都用 `IndexMap`：重声明一个已存在的键保持原位置，删除后再加回到末尾，
//! 与上游的 `Map` / `Object.entries` 一致。

use std::collections::BTreeSet;

use indexmap::IndexMap;

use crate::types::{
    Context, Message, SystemContent, SystemMessage, SystemRole, Tool, ToolReference,
    TranscriptContext,
};
use crate::utils::text::{content_text, get_system_message_text};

/// 为提示词和工具表构造开头那条系统消息。两者都为空时返回 `None`，空记录保持为空。
pub fn create_initial_system_message(
    system_prompt: Option<&str>,
    tools: Option<&[Tool]>,
) -> Option<SystemMessage> {
    let system_prompt = system_prompt.filter(|prompt| !prompt.is_empty());
    let tools = tools.filter(|tools| !tools.is_empty());
    if system_prompt.is_none() && tools.is_none() {
        return None;
    }
    Some(SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text(system_prompt.unwrap_or_default().to_string()),
        sections: None,
        tools_added: tools.map(<[Tool]>::to_vec),
        tools_removed: None,
        timestamp: 0,
    })
}

/// 把 `Context.systemPrompt` 与 `Context.tools` 并进开头那条系统消息。
///
/// 这是唯一产出 `TranscriptContext` 的入口；面向 provider 的函数都只接受它的结果。
pub fn normalize_context(context: Context) -> TranscriptContext {
    let initial =
        create_initial_system_message(context.system_prompt.as_deref(), context.tools.as_deref());
    let mut messages = context.messages;
    if let Some(initial) = initial {
        messages.insert(0, Message::System(initial));
    }
    TranscriptContext { messages }
}

/// pi 的 `isSystemMessage` 类型守卫。
fn system_message(message: &Message) -> Option<&SystemMessage> {
    match message {
        Message::System(message) => Some(message),
        _ => None,
    }
}

/// 记录以系统消息开头时，返回开头的那条。
pub fn get_initial_system_message(messages: &[Message]) -> Option<&SystemMessage> {
    messages.first().and_then(system_message)
}

/// 去掉开头那条系统消息，供把提示词放在消息列表之外的接口使用。
pub fn without_initial_system_message(messages: &[Message]) -> Vec<Message> {
    let skip = usize::from(get_initial_system_message(messages).is_some());
    messages[skip..].to_vec()
}

/// 依次应用记录里的每条工具增减，得到当前可用的工具表。
pub fn get_current_tools(messages: &[Message]) -> Vec<Tool> {
    let system_messages: Vec<&SystemMessage> = messages.iter().filter_map(system_message).collect();
    tools_after(&system_messages)
}

fn tools_after(system_messages: &[&SystemMessage]) -> Vec<Tool> {
    let mut tools: IndexMap<String, Tool> = IndexMap::new();
    for message in system_messages {
        for tool in message.tools_removed.iter().flatten() {
            tools.shift_remove(&tool.name);
        }
        for tool in message.tools_added.iter().flatten() {
            tools.insert(tool.name.clone(), tool.clone());
        }
    }
    tools.into_values().collect()
}

/// 把所有系统消息重放成一条开头消息：后来的 `content` 追加到基础提示词，
/// `sections` 按名字打补丁，工具表按 `get_current_tools` 结算。
pub fn get_current_system_message(messages: &[Message]) -> Option<SystemMessage> {
    let mut content: Vec<String> = Vec::new();
    let mut sections: IndexMap<String, String> = IndexMap::new();
    let mut timestamp: Option<u64> = None;

    for message in messages.iter().filter_map(system_message) {
        if timestamp.is_none() {
            timestamp = Some(message.timestamp);
        }
        let text = content_text(&message.content, "\n");
        if !text.is_empty() {
            content.push(text);
        }
        if let Some(patches) = &message.sections {
            for (name, value) in patches {
                match value {
                    None => {
                        sections.shift_remove(name);
                    }
                    Some(value) => {
                        sections.insert(name.clone(), value.clone());
                    }
                }
            }
        }
    }

    let tools = get_current_tools(messages);
    if timestamp.is_none() && tools.is_empty() {
        return None;
    }

    Some(SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text(content.join("\n\n")),
        sections: (!sections.is_empty()).then(|| {
            sections
                .into_iter()
                .map(|(name, text)| (name, Some(text)))
                .collect()
        }),
        tools_added: (!tools.is_empty()).then_some(tools),
        tools_removed: None,
        timestamp: timestamp.unwrap_or(0),
    })
}

/// 重放之后当前的系统提示词文本。
pub fn get_current_system_prompt(messages: &[Message]) -> String {
    match get_current_system_message(messages) {
        Some(message) => get_system_message_text(&message),
        None => String::new(),
    }
}

/// 为不支持会话中途系统消息的接口重建记录：重放出来的系统消息打头，其余系统消息全部丢掉。
pub fn collapse_system_messages(context: &TranscriptContext) -> TranscriptContext {
    let head = get_current_system_message(&context.messages);
    let mut messages: Vec<Message> = context
        .messages
        .iter()
        .filter(|message| message.role() != "system")
        .cloned()
        .collect();
    if let Some(head) = head {
        messages.insert(0, Message::System(head));
    }
    TranscriptContext { messages }
}

/// 模型接受中途系统消息就原样保留，否则折叠。
pub fn resolve_transcript(
    context: &TranscriptContext,
    supports_mid_convo_system_messages: Option<bool>,
) -> TranscriptContext {
    if supports_mid_convo_system_messages == Some(true) {
        context.clone()
    } else {
        collapse_system_messages(context)
    }
}

/// 比较或持久化之前，去掉工具里可执行和仅用于展示的字段。
///
/// 上游走一遍 `JSON.parse(JSON.stringify(...))`：丢掉不可枚举的 schema 元数据与 `undefined`
/// 字段，并把对象的可观察键序换成 JS 的键序。Rust 侧的 `parameters` 本来就是 `Value`，没有
/// 不可枚举的东西可丢，所以这里只做那一步键序归一。
pub fn to_tool_declaration(tool: &Tool) -> Tool {
    let parameters = serde_json::to_value(&tool.parameters)
        .and_then(serde_json::from_value)
        .map(|parameters: serde_json::Value| xjpcode_schema::js::reorder(&parameters))
        .unwrap_or(serde_json::Value::Null);
    Tool {
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters,
        constrained_sampling: tool.constrained_sampling.clone(),
    }
}

/// 两个工具是否向模型声明了同一个接口。
///
/// 上游比的是 `JSON.stringify(toToolDeclaration(...))`：键按 JS 的键序走，数字按 JS 的写法
/// 写。`serde_json::to_string` 只保住插入顺序、还会把 `1.0` 写成 `1.0`，所以这里用
/// `js::stringify`：`{"1":1,"b":2}` 与 `{"b":2,"1":1}` 是同一份声明，`1` 与 `1.0` 也是
/// 同一个数。
pub fn declarations_equal(left: &Tool, right: &Tool) -> bool {
    declaration_text(left) == declaration_text(right)
}

fn declaration_text(tool: &Tool) -> Option<String> {
    let declaration = serde_json::to_value(to_tool_declaration(tool)).ok()?;
    Some(xjpcode_schema::js::stringify(&declaration))
}

/// 一次工具表变化的两个方向。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolStateChanges {
    pub tools_added: Vec<Tool>,
    pub tools_removed: Vec<ToolReference>,
}

/// 比较两个完整工具表。定义变了按"先移除再加回"处理。
pub fn get_tool_state_changes(previous: &[Tool], current: &[Tool]) -> ToolStateChanges {
    let previous_tools: IndexMap<&str, &Tool> = previous
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect();
    let current_tools: IndexMap<&str, &Tool> = current
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect();

    let tools_added = current
        .iter()
        .filter(|tool| match previous_tools.get(tool.name.as_str()) {
            None => true,
            Some(previous) => !declarations_equal(previous, tool),
        })
        .map(to_tool_declaration)
        .collect();
    let tools_removed = previous
        .iter()
        .filter(|tool| match current_tools.get(tool.name.as_str()) {
            None => true,
            Some(current) => !declarations_equal(tool, current),
        })
        .map(|tool| ToolReference {
            name: tool.name.clone(),
        })
        .collect();

    ToolStateChanges {
        tools_added,
        tools_removed,
    }
}

/// 记录里的工具状态引用到的每个定义，按首次声明的顺序。
pub fn get_declared_tools(messages: &[Message]) -> Vec<Tool> {
    let mut definitions: IndexMap<String, Tool> = IndexMap::new();
    for message in messages.iter().filter_map(system_message) {
        for tool in message.tools_added.iter().flatten() {
            definitions.insert(tool.name.clone(), tool.clone());
        }
    }
    definitions.into_values().collect()
}

/// 是否有工具名被用不同定义声明了两次。
///
/// 只能按名字引用已声明工具的传输方式无法重放这样的历史。
pub fn has_tool_redefinitions(messages: &[Message]) -> bool {
    let mut declared: IndexMap<String, Tool> = IndexMap::new();
    for message in messages.iter().filter_map(system_message) {
        for tool in message.tools_added.iter().flatten() {
            if let Some(previous) = declared.get(&tool.name) {
                if !declarations_equal(previous, tool) {
                    return true;
                }
            }
            declared.insert(tool.name.clone(), tool.clone());
        }
    }
    false
}

/// 工具历史里是否有"删除"或"同名重声明"——只支持追加的传输方式重放不了。
pub fn has_non_additive_tool_changes(messages: &[Message]) -> bool {
    let system_messages: Vec<&SystemMessage> = messages.iter().filter_map(system_message).collect();
    find_last_non_additive_tool_change(&system_messages).is_some()
}

/// 最后一条带删除或同名重声明的系统消息下标；没有则 `None`。
fn find_last_non_additive_tool_change(system_messages: &[&SystemMessage]) -> Option<usize> {
    // 删除过的名字仍算"声明过"：Kim K3 会忽略被删除、随后又被工具消息重新声明的工具。
    let mut declared: BTreeSet<String> = BTreeSet::new();
    let mut last = None;
    for (index, message) in system_messages.iter().enumerate() {
        if message
            .tools_removed
            .as_ref()
            .is_some_and(|removed| !removed.is_empty())
        {
            last = Some(index);
        }
        for tool in message.tools_added.iter().flatten() {
            if declared.contains(&tool.name) {
                last = Some(index);
            }
            declared.insert(tool.name.clone());
        }
    }
    last
}

/// 传输方式能怎样表达首次请求之后的工具变化。
///
/// - `None`：只有顶层工具表存在，它永远等于当前工具。
/// - `Additions`：后续消息可以加工具，但不能删或改。
/// - `Changes`：每个工具（包括最初的）都在原位声明，同名后声明替换先前声明。
///   删除在线上无法表达，传输方式把它写成文字，被删的工具保持声明。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TranscriptToolSupport {
    #[default]
    None,
    Additions,
    Changes,
}

/// 工具声明在顶层请求字段和原位变化之间的分配。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TranscriptTools {
    /// 放进顶层请求字段的工具。
    pub request_tools: Vec<Tool>,
    /// 开头有多少条系统消息的工具变化已经被 `request_tools` 吸收；其后每条系统消息
    /// 都自带原位变化。
    pub folded_system_messages: usize,
}

/// 把工具声明分配到顶层请求字段和原位变化之间。
pub fn resolve_transcript_tools(
    messages: &[Message],
    support: TranscriptToolSupport,
) -> TranscriptTools {
    let system_messages: Vec<&SystemMessage> = messages.iter().filter_map(system_message).collect();
    match support {
        TranscriptToolSupport::None => TranscriptTools {
            request_tools: tools_after(&system_messages),
            folded_system_messages: system_messages.len(),
        },
        TranscriptToolSupport::Changes => TranscriptTools {
            request_tools: Vec::new(),
            folded_system_messages: 0,
        },
        TranscriptToolSupport::Additions => {
            let folded = usize::from(get_initial_system_message(messages).is_some()).max(
                find_last_non_additive_tool_change(&system_messages).map_or(0, |index| index + 1),
            );
            TranscriptTools {
                request_tools: tools_after(&system_messages[..folded]),
                folded_system_messages: folded,
            }
        }
    }
}
