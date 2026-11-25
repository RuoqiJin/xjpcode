//! `@earendil-works/pi-coding-agent` 的 crate 根，对应上游 `src/index.ts`。
//!
//! 本里程碑只搬两个叶子：默认系统提示词（`core/system-prompt.ts`）与四个内置工具
//! （`core/tools/` 的 `read.ts`、`edit.ts`、`write.ts`、`bash.ts` 及其依赖）。
//! 应用层的其余部分（会话、扩展、压缩、TUI）还没搬，不在这里导出；对照表见
//! `docs/port/milestone-3.md`。

pub mod config;
pub mod core;
pub mod utils;

pub use config::{get_agent_dir, get_bin_dir, get_docs_path, get_examples_path, get_readme_path};
pub use core::agent_session::{
    AgentSession, AgentSessionEvent, AgentSessionOptions, AgentSessionSubscription, CompactionReason,
    StreamingBehavior,
};
pub use core::compaction::{
    CompactionPreparation, CompactionResult, CompactionSettings, DEFAULT_COMPACTION_SETTINGS,
};
pub use core::session_manager::{SessionManager, SessionProjection};
pub use core::settings_manager::Settings;
pub use core::skills::{format_skills_for_prompt, Skill};
pub use core::system_prompt::{
    build_system_prompt, build_system_prompt_sections, build_system_prompt_state,
    diff_system_prompt_sections, normalize_build_system_prompt_options, BuildSystemPromptOptions,
    NormalizedBuildSystemPromptOptions, SystemPromptSections, SystemPromptState,
};
pub use core::tools::bash::{
    bash_tool_system_prompt_contribution, create_bash_tool, create_local_bash_operations,
    BashOperations, BashTool, BashToolOptions,
};
pub use core::tools::edit::{
    create_edit_tool, edit_tool_system_prompt_contribution, EditOperations, EditTool, EditToolOptions,
};
pub use core::tools::read::{
    create_read_tool, read_tool_system_prompt_contribution, ReadOperations, ReadTool,
    ReadToolOptions,
};
pub use core::tools::write::{
    create_write_tool, write_tool_system_prompt_contribution, WriteOperations, WriteTool,
    WriteToolOptions,
};
pub use core::tools::ToolSystemPromptContribution;
