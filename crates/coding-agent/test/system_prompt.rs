//! 对应上游 `test/system-prompt.test.ts`：默认提示词的结构、工具清单、规矩、技能块。

use indexmap::IndexMap;
use xjpcode_coding_agent::core::skills::Skill;
use xjpcode_coding_agent::core::source_info::SourceInfo;
use xjpcode_coding_agent::core::system_prompt::{
    build_system_prompt, BuildSystemPromptOptions, ContextFile,
};

fn skill() -> Skill {
    Skill {
        name: "test-skill".to_string(),
        description: "A test skill.".to_string(),
        file_path: "/skills/test-skill/SKILL.md".to_string(),
        base_dir: "/skills/test-skill".to_string(),
        source_info: SourceInfo::synthetic(
            "/skills/test-skill/SKILL.md",
            "test",
            None,
            Some("/skills/test-skill"),
        ),
        disable_model_invocation: false,
    }
}

fn build(mut options: BuildSystemPromptOptions) -> String {
    if options.cwd.is_empty() {
        options.cwd = "/tmp".to_string();
    }
    build_system_prompt(options).unwrap()
}

#[test]
fn shows_none_for_an_empty_tools_list() {
    let prompt = build(BuildSystemPromptOptions {
        selected_tools: Some(vec![]),
        ..Default::default()
    });
    assert!(prompt.contains("<tools>\n(none)\n"));
}

#[test]
fn shows_the_file_paths_guideline_even_with_no_tools() {
    let prompt = build(BuildSystemPromptOptions {
        selected_tools: Some(vec![]),
        ..Default::default()
    });
    assert!(prompt.contains("Show file paths clearly"));
}

#[test]
fn keeps_the_default_and_custom_prompt_prefixes_exact() {
    let default_prompt = build(BuildSystemPromptOptions {
        selected_tools: Some(vec![]),
        cwd: "/tmp".to_string(),
        ..Default::default()
    });
    assert!(default_prompt
        .starts_with("You are a coding assistant running inside pi, a coding agent harness."));
    let custom_prompt = build(BuildSystemPromptOptions {
        custom_prompt: Some("You are Exact.".to_string()),
        selected_tools: Some(vec![]),
        cwd: "/tmp".to_string(),
        ..Default::default()
    });
    assert!(custom_prompt.starts_with("You are Exact.\n\n<cwd>"));
}

#[test]
fn preserves_an_exact_forced_prompt_without_sections() {
    assert_eq!(
        build_system_prompt(BuildSystemPromptOptions {
            force_system_prompt: Some("exact".to_string()),
            cwd: "/tmp".to_string(),
            ..Default::default()
        })
        .unwrap(),
        "exact"
    );
}

#[test]
fn maps_appended_instructions_and_project_context_to_stable_sections() {
    let prompt = build(BuildSystemPromptOptions {
        custom_prompt: Some("You are Exact.".to_string()),
        append_system_prompt: Some("Additional instructions.".to_string()),
        context_files: Some(vec![ContextFile {
            path: "/tmp/AGENTS.md".to_string(),
            content: "Project instructions.".to_string(),
        }]),
        selected_tools: Some(vec![]),
        cwd: "/tmp".to_string(),
        ..Default::default()
    });
    assert!(prompt.contains("<addendum>\nAdditional instructions.\n</addendum>"));
    assert!(prompt.contains(
        "<project_context>\nProject-specific instructions and guidelines:\n\n<project_instructions path=\"/tmp/AGENTS.md\">"
    ));
    assert!(prompt.contains("<cwd>\n/tmp\n</cwd>"));
}

#[test]
fn includes_all_default_tools_when_snippets_are_provided() {
    let mut snippets = IndexMap::new();
    snippets.insert("read".to_string(), "Read file contents".to_string());
    snippets.insert("bash".to_string(), "Execute bash commands".to_string());
    snippets.insert("edit".to_string(), "Make surgical edits".to_string());
    snippets.insert("write".to_string(), "Create or overwrite files".to_string());
    let prompt = build(BuildSystemPromptOptions {
        tool_snippets: Some(snippets),
        ..Default::default()
    });
    assert!(prompt.contains("- read:"));
    assert!(prompt.contains("- bash:"));
    assert!(prompt.contains("- edit:"));
    assert!(prompt.contains("- write:"));
}

#[test]
fn uses_shell_specific_guidance() {
    for (tools, expected) in [
        (
            vec!["powershell".to_string()],
            "Use PowerShell for file operations",
        ),
        (
            vec!["bash".to_string(), "powershell".to_string()],
            "Use bash or PowerShell for file operations",
        ),
    ] {
        let prompt = build(BuildSystemPromptOptions {
            selected_tools: Some(tools),
            ..Default::default()
        });
        assert!(prompt.contains(expected));
    }
}

#[test]
fn instructs_models_to_resolve_docs_and_examples_under_absolute_base_paths() {
    let prompt = build(BuildSystemPromptOptions::default());
    assert!(prompt.contains(
        "- When reading pi docs or examples, resolve docs/... under Additional docs and examples/... under Examples, not the current working directory"
    ));
    assert!(prompt.contains(
        "environment variables (docs/environment-variables.md), MCP servers (docs/mcp.md)"
    ));
    assert!(prompt.contains(
        "Before implementing harness changes, read the relevant docs and examples in full, following related cross-references."
    ));
}

#[test]
fn includes_custom_tools_only_when_a_snippet_is_provided() {
    let mut snippets = IndexMap::new();
    snippets.insert(
        "dynamic_tool".to_string(),
        "Run dynamic test behavior".to_string(),
    );
    let prompt = build(BuildSystemPromptOptions {
        selected_tools: Some(vec!["read".to_string(), "dynamic_tool".to_string()]),
        tool_snippets: Some(snippets),
        ..Default::default()
    });
    assert!(prompt.contains("- dynamic_tool: Run dynamic test behavior"));

    let prompt = build(BuildSystemPromptOptions {
        selected_tools: Some(vec!["read".to_string(), "dynamic_tool".to_string()]),
        ..Default::default()
    });
    assert!(!prompt.contains("dynamic_tool"));
}

#[test]
fn appends_and_deduplicates_prompt_guidelines() {
    let prompt = build(BuildSystemPromptOptions {
        selected_tools: Some(vec!["read".to_string(), "dynamic_tool".to_string()]),
        prompt_guidelines: Some(vec!["Use dynamic_tool for project summaries.".to_string()]),
        ..Default::default()
    });
    assert!(prompt.contains("- Use dynamic_tool for project summaries."));

    let prompt = build(BuildSystemPromptOptions {
        selected_tools: Some(vec!["read".to_string(), "dynamic_tool".to_string()]),
        prompt_guidelines: Some(vec![
            "Use dynamic_tool for summaries.".to_string(),
            "  Use dynamic_tool for summaries.  ".to_string(),
            "   ".to_string(),
        ]),
        ..Default::default()
    });
    assert_eq!(
        prompt.matches("- Use dynamic_tool for summaries.").count(),
        1
    );
}

#[test]
fn includes_skills_with_only_bash_in_default_and_custom_prompts() {
    for custom_prompt in [None, Some("Custom system prompt".to_string())] {
        let prompt = build(BuildSystemPromptOptions {
            custom_prompt,
            selected_tools: Some(vec!["bash".to_string()]),
            skills: Some(vec![skill()]),
            ..Default::default()
        });
        assert!(prompt.contains("<skills>"));
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<name>test-skill</name>"));
        assert!(prompt.contains("Use bash to load a skill's file"));
        assert!(prompt.contains(
            "Resolve relative paths against the skill's directory (parent of SKILL.md). Use absolute paths in tool calls."
        ));
    }
}

#[test]
fn omits_skills_without_read_or_bash() {
    let prompt = build(BuildSystemPromptOptions {
        selected_tools: Some(vec!["write".to_string()]),
        skills: Some(vec![skill()]),
        ..Default::default()
    });
    assert!(!prompt.contains("<available_skills>"));
}

#[test]
fn leaves_hidden_tools_out_of_the_tool_list_and_rules() {
    let mut snippets = IndexMap::new();
    snippets.insert("read".to_string(), "Read files".to_string());
    snippets.insert("bash".to_string(), "Run commands".to_string());
    snippets.insert("run".to_string(), "Run a task".to_string());
    let mut guidelines = IndexMap::new();
    guidelines.insert("read".to_string(), vec!["Use read for files.".to_string()]);
    guidelines.insert("run".to_string(), vec!["Prefer run.".to_string()]);
    let build_hidden = |hidden: Vec<&str>| {
        build(BuildSystemPromptOptions {
            selected_tools: Some(vec![
                "read".to_string(),
                "bash".to_string(),
                "run".to_string(),
            ]),
            hidden_tools: Some(hidden.into_iter().map(str::to_string).collect()),
            tool_snippets: Some(snippets.clone()),
            tool_guidelines: Some(guidelines.clone()),
            skills: Some(vec![skill()]),
            ..Default::default()
        })
    };

    let prompt = build_hidden(vec!["read", "bash"]);
    assert!(prompt.contains("<tools>\n- run: Run a task\n"));
    assert!(!prompt.contains("- read: "));
    assert!(!prompt.contains("Use read for files."));
    assert!(!prompt.contains("Use bash for file operations"));
    assert!(prompt.contains("- Prefer run."));
    assert!(prompt.contains("\nLoad a skill's file when the task matches its description."));

    assert!(build_hidden(vec!["read"]).contains("Use bash to load a skill's file"));
    assert!(build_hidden(vec![]).contains("Use the read tool to load a skill's file"));
}
