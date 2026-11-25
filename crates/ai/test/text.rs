//! 移植自上游 `packages/ai/test/text.test.ts`。

use xjpcode_ai::types::{
    AssistantContentBlock, ImageContent, TextContent, ThinkingContent, ThinkingContentType,
    ToolCall, ToolResultContentBlock,
};
use xjpcode_ai::utils::text::content_text;

fn content() -> Vec<AssistantContentBlock> {
    vec![
        AssistantContentBlock::Thinking(ThinkingContent {
            r#type: ThinkingContentType::Thinking,
            thinking: "reasoning".into(),
            thinking_signature: None,
            redacted: None,
        }),
        AssistantContentBlock::Text(TextContent::new("first")),
        AssistantContentBlock::ToolCall(ToolCall::new("1", "read", serde_json::Map::new())),
        AssistantContentBlock::Text(TextContent::new("second")),
    ]
}

#[test]
fn extracts_assistant_text_blocks() {
    assert_eq!(content_text(&content(), "\n"), "first\nsecond");
}

#[test]
fn supports_custom_separators() {
    assert_eq!(content_text(&content(), ""), "firstsecond");
}

#[test]
fn passes_string_content_through() {
    assert_eq!(content_text("hello", "\n"), "hello");
}

#[test]
fn extracts_text_from_tool_result_content() {
    let tool_result_content = vec![
        ToolResultContentBlock::Text(TextContent::new("first")),
        ToolResultContentBlock::Image(ImageContent::new("...", "image/png")),
        ToolResultContentBlock::Text(TextContent::new("second")),
    ];

    assert_eq!(content_text(&tool_result_content, ""), "firstsecond");
}
