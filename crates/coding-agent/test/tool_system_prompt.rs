//! 对应上游 `test/tool-system-prompt-contributions.test.ts`：工具定义与提示词贡献保持一致。

use xjpcode_coding_agent::core::tools::bash::{
    bash_tool_system_prompt_contribution, create_bash_tool,
};
use xjpcode_coding_agent::core::tools::edit::{create_edit_tool, edit_tool_system_prompt_contribution};
use xjpcode_coding_agent::core::tools::read::{create_read_tool, read_tool_system_prompt_contribution};
use xjpcode_coding_agent::core::tools::write::{create_write_tool, write_tool_system_prompt_contribution};
use xjpcode_coding_agent::{
    BashToolOptions, EditToolOptions, ReadToolOptions, WriteToolOptions,
};

#[test]
fn keeps_each_tool_aligned_with_its_contribution() {
    let read = create_read_tool("/workspace", ReadToolOptions::default());
    let contribution = read_tool_system_prompt_contribution();
    assert_eq!(read.system_prompt_contribution(), contribution);

    let edit = create_edit_tool("/workspace", EditToolOptions::default());
    let contribution = edit_tool_system_prompt_contribution();
    assert_eq!(edit.system_prompt_contribution(), contribution);

    let write = create_write_tool("/workspace", WriteToolOptions::default());
    let contribution = write_tool_system_prompt_contribution();
    assert_eq!(write.system_prompt_contribution(), contribution);

    let bash = create_bash_tool("/workspace", BashToolOptions::default());
    let contribution = bash_tool_system_prompt_contribution();
    assert_eq!(bash.prompt_snippet(), contribution.snippet);
}

#[test]
fn keeps_bash_session_environment_guidance_conditional() {
    let bash = create_bash_tool(
        "/workspace",
        BashToolOptions {
            expose_session_environment: Some(false),
            ..BashToolOptions::default()
        },
    );
    assert_eq!(bash.prompt_guidelines(), None);

    let bash = create_bash_tool(
        "/workspace",
        BashToolOptions {
            expose_session_environment: Some(true),
            ..BashToolOptions::default()
        },
    );
    assert_eq!(
        bash.prompt_guidelines(),
        Some(bash_tool_system_prompt_contribution().guidelines)
    );
}
