//! 对应上游 `packages/ai/src/utils/text.ts`。
//!
//! 上游 `contentText(content: string | readonly Content[])` 用联合类型同时接受字符串和内容块
//! 数组；Rust 侧用一个 `ContentText` trait 表达同一个范围（字符串原样通过，块数组只取
//! `text` 块），调用点写成 `content_text(&message.content, "\n")`。

use crate::types::{
    AssistantContentBlock, ImageContent, SystemContent, SystemMessage, TextContent,
    ThinkingContent, ToolCall, ToolResultContentBlock, UserContentBlock,
};

/// pi: `contentText` 接受的 `string | readonly Content[]`。
///
/// `Content` 是 `TextContent | ImageContent | ThinkingContent | ToolCall`：Rust 侧让这四种块
/// 各自实现本 trait（只有文本块贡献文本），块数组与联合类型再由下面的批量实现组合出来。
pub trait ContentText {
    /// 按顺序取出参与拼接的文本片段。
    fn text_parts(&self) -> Vec<&str>;
}

impl ContentText for str {
    fn text_parts(&self) -> Vec<&str> {
        vec![self]
    }
}

impl ContentText for String {
    fn text_parts(&self) -> Vec<&str> {
        vec![self.as_str()]
    }
}

impl ContentText for TextContent {
    fn text_parts(&self) -> Vec<&str> {
        vec![self.text.as_str()]
    }
}

impl ContentText for ThinkingContent {
    fn text_parts(&self) -> Vec<&str> {
        Vec::new()
    }
}

impl ContentText for ImageContent {
    fn text_parts(&self) -> Vec<&str> {
        Vec::new()
    }
}

impl ContentText for ToolCall {
    fn text_parts(&self) -> Vec<&str> {
        Vec::new()
    }
}

impl ContentText for AssistantContentBlock {
    fn text_parts(&self) -> Vec<&str> {
        match self {
            Self::Text(block) => block.text_parts(),
            Self::Thinking(block) => block.text_parts(),
            Self::ToolCall(block) => block.text_parts(),
        }
    }
}

impl ContentText for ToolResultContentBlock {
    fn text_parts(&self) -> Vec<&str> {
        match self {
            Self::Text(block) => block.text_parts(),
            Self::Image(block) => block.text_parts(),
        }
    }
}

impl ContentText for UserContentBlock {
    fn text_parts(&self) -> Vec<&str> {
        match self {
            Self::Text(block) => block.text_parts(),
            Self::Image(block) => block.text_parts(),
        }
    }
}

impl ContentText for SystemContent {
    fn text_parts(&self) -> Vec<&str> {
        match self {
            Self::Text(text) => text.text_parts(),
            Self::Blocks(blocks) => blocks.text_parts(),
        }
    }
}

impl<T: ContentText> ContentText for [T] {
    fn text_parts(&self) -> Vec<&str> {
        self.iter().flat_map(ContentText::text_parts).collect()
    }
}

impl<T: ContentText> ContentText for Vec<T> {
    fn text_parts(&self) -> Vec<&str> {
        self.as_slice().text_parts()
    }
}

/// 取出并拼接内容里的文本块。字符串内容原样通过。
pub fn content_text(content: &(impl ContentText + ?Sized), separator: &str) -> String {
    content.text_parts().join(separator)
}

/// 把系统消息渲染成完整提示词：正文在前，其后按顺序是各个片段。
pub fn get_system_message_text(message: &SystemMessage) -> String {
    let mut parts: Vec<String> = vec![content_text(&message.content, "\n")];
    if let Some(sections) = &message.sections {
        for value in sections.values() {
            if let Some(text) = value {
                parts.push(text.clone());
            }
        }
    }
    parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 为支持会话中途系统消息的接口渲染后来的系统消息。
///
/// 片段变更按名字框住，好让模型把它和开头提示词对上。这段措辞只在请求时使用，
/// 版本之间可能变化——与上游一致。
pub fn render_system_message_update(message: &SystemMessage) -> String {
    let mut parts: Vec<String> = Vec::new();
    let text = content_text(&message.content, "\n");
    if !text.is_empty() {
        parts.push(text);
    }
    if let Some(sections) = &message.sections {
        for (name, value) in sections {
            parts.push(match value {
                None => format!("Removed system prompt section \"{name}\"."),
                Some(value) => format!("Updated system prompt section \"{name}\":\n\n{value}"),
            });
        }
    }
    parts.join("\n\n")
}
