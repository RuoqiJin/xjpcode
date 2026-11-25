//! 对应上游 `src/core/tools/`：本里程碑搬了读、改、写、跑命令四样及其依赖。

pub mod bash;
pub mod edit;
pub mod edit_diff;
pub mod file_mutation_queue;
pub mod output_accumulator;
pub mod path_utils;
pub mod read;
pub mod truncate;
pub mod write;

/// 上游每个工具定义上的 `promptSnippet` / `promptGuidelines`：系统提示词用它们列工具、加规矩。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolSystemPromptContribution {
    pub snippet: &'static str,
    pub guidelines: &'static [&'static str],
}
