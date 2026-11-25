//! 朴素入口用到的会话文本小工具（全屏界面不用）。

use xjpcode_agent::AgentToolResult;
use xjpcode_ai::ToolResultContentBlock;
use xjpcode_coding_agent::core::agent_session::CompactionReason;

pub fn reason_name(reason: CompactionReason) -> &'static str {
    match reason {
        CompactionReason::Manual => "manual",
        CompactionReason::Threshold => "threshold",
        CompactionReason::Overflow => "overflow",
    }
}

/// 工具结果里交给模型看的那段文本。
pub fn tool_result_text(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .map(|block| match block {
            ToolResultContentBlock::Text(text) => text.text.clone(),
            ToolResultContentBlock::Image(_) => "[image]".to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn split_command(input: &str) -> (&str, &str) {
    match input.find(' ') {
        Some(position) => (&input[..position], input[position + 1..].trim()),
        None => (input, ""),
    }
}
