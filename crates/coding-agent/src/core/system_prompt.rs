//! 对应上游 `src/core/system-prompt.ts`：默认系统提示词、具名片段、工具清单、技能块。
//!
//! 上游的片段是 `Record<string, string>`（插入顺序可观察）；这里用 `IndexMap`。渲染仍交给
//! `xjpcode-ai` 的 `getSystemMessageText`，与从会话记录重放时逐字相同。

use indexmap::IndexMap;

use crate::config::{get_docs_path, get_examples_path, get_readme_path};
use crate::core::skills::{format_skills_for_prompt, Skill, SkillFileReadTool};
use xjpcode_ai::{get_system_message_text, SystemContent, SystemMessage, SystemRole};

/// pi: `BuildSystemPromptOptions`
#[derive(Debug, Clone, Default)]
pub struct BuildSystemPromptOptions {
    /// 换掉默认开头。
    pub custom_prompt: Option<String>,
    /// 整份替换（由 `before_agent_start` 给出）。
    pub force_system_prompt: Option<String>,
    /// 列进提示词的工具；缺省是 `[read, bash, edit, write]`。
    pub selected_tools: Option<Vec<String>>,
    /// 声明里有、请求里不发的工具（只能通过另一个工具用到）。
    pub hidden_tools: Option<Vec<String>>,
    /// 工具名对应的一句说明。
    pub tool_snippets: Option<IndexMap<String, String>>,
    /// 每个工具贡献的规矩。
    pub tool_guidelines: Option<IndexMap<String, Vec<String>>>,
    /// 追加在默认规矩后面的几条。
    pub prompt_guidelines: Option<Vec<String>>,
    /// 用户配置追加的正文，排在项目上下文、技能、cwd 之前。
    pub append_system_prompt: Option<String>,
    /// 额外具名片段。
    pub sections: Option<IndexMap<String, String>>,
    /// 工作目录。
    pub cwd: String,
    /// 预先读好的项目上下文文件。
    pub context_files: Option<Vec<ContextFile>>,
    /// 预先读好的技能。
    pub skills: Option<Vec<Skill>>,
}

/// pi: `contextFiles: Array<{ path, content }>`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFile {
    pub path: String,
    pub content: String,
}

/// pi: `NormalizedBuildSystemPromptOptions`
#[derive(Debug, Clone)]
pub struct NormalizedBuildSystemPromptOptions {
    pub custom_prompt: Option<String>,
    pub force_system_prompt: Option<String>,
    pub selected_tools: Vec<String>,
    pub hidden_tools: Vec<String>,
    pub tool_snippets: IndexMap<String, String>,
    pub tool_guidelines: IndexMap<String, Vec<String>>,
    pub prompt_guidelines: Vec<String>,
    pub append_system_prompt: String,
    pub sections: IndexMap<String, String>,
    pub cwd: String,
    pub context_files: Vec<ContextFile>,
    pub skills: Vec<Skill>,
}

/// pi: `SystemPromptSections`
pub type SystemPromptSections = IndexMap<String, String>;

/// pi: `normalizeBuildSystemPromptOptions`
pub fn normalize_build_system_prompt_options(
    input: BuildSystemPromptOptions,
) -> NormalizedBuildSystemPromptOptions {
    NormalizedBuildSystemPromptOptions {
        custom_prompt: input.custom_prompt,
        force_system_prompt: input.force_system_prompt,
        selected_tools: input.selected_tools.unwrap_or_else(|| {
            vec![
                "read".to_string(),
                "bash".to_string(),
                "edit".to_string(),
                "write".to_string(),
            ]
        }),
        hidden_tools: input.hidden_tools.unwrap_or_default(),
        tool_snippets: input.tool_snippets.unwrap_or_default(),
        tool_guidelines: input.tool_guidelines.unwrap_or_default(),
        prompt_guidelines: input.prompt_guidelines.unwrap_or_default(),
        append_system_prompt: input.append_system_prompt.unwrap_or_default(),
        sections: input.sections.unwrap_or_default(),
        cwd: input.cwd,
        context_files: input.context_files.unwrap_or_default(),
        skills: input.skills.unwrap_or_default(),
    }
}

fn render_project_context(context_files: &[ContextFile]) -> String {
    let mut parts = vec!["Project-specific instructions and guidelines:".to_string()];
    for file in context_files {
        parts.push(format!(
            "<project_instructions path=\"{}\">\n{}\n</project_instructions>",
            file.path, file.content
        ));
    }
    parts.join("\n\n")
}

fn build_rules(
    selected_tools: &[String],
    tool_guidelines: &IndexMap<String, Vec<String>>,
    prompt_guidelines: &[String],
) -> String {
    let mut rules: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut add_rule = |rule: &str| {
        let normalized = rule.trim();
        if normalized.is_empty() || seen.iter().any(|seen| seen == normalized) {
            return;
        }
        seen.push(normalized.to_string());
        rules.push(normalized.to_string());
    };

    let has_bash = selected_tools.iter().any(|tool| tool == "bash");
    let has_power_shell = selected_tools.iter().any(|tool| tool == "powershell");
    let has_grep = selected_tools.iter().any(|tool| tool == "grep");
    let has_find = selected_tools.iter().any(|tool| tool == "find");
    let has_ls = selected_tools.iter().any(|tool| tool == "ls");

    if (has_bash || has_power_shell) && !has_grep && !has_find && !has_ls {
        if has_bash && has_power_shell {
            add_rule("Use bash or PowerShell for file operations like listing, searching, and finding files");
        } else if has_power_shell {
            add_rule(
                "Use PowerShell for file operations like listing, searching, and finding files",
            );
        } else {
            add_rule("Use bash for file operations like ls, rg, find");
        }
    }

    for name in selected_tools {
        if let Some(guidelines) = tool_guidelines.get(name) {
            for rule in guidelines {
                add_rule(rule);
            }
        }
    }
    for rule in prompt_guidelines {
        add_rule(rule);
    }
    add_rule("Be concise in your responses");
    add_rule("Show file paths clearly when working with files");
    rules
        .iter()
        .map(|rule| format!("- {rule}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_valid_section_name(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    characters.all(|character| {
        character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || character == '_'
            || character == '-'
    })
}

/// pi: `buildSystemPromptSections`
pub fn build_system_prompt_sections(
    input: BuildSystemPromptOptions,
) -> Result<SystemPromptSections, String> {
    let options = normalize_build_system_prompt_options(input);
    let NormalizedBuildSystemPromptOptions {
        custom_prompt,
        selected_tools,
        hidden_tools,
        tool_snippets,
        tool_guidelines,
        prompt_guidelines,
        append_system_prompt,
        sections: custom_sections,
        cwd,
        context_files,
        skills,
        force_system_prompt: _,
    } = options;

    for name in custom_sections.keys() {
        if !is_valid_section_name(name) || name == "preamble" {
            return Err(format!("Invalid system prompt section name: {name}"));
        }
    }

    let declared_tools: Vec<String> = selected_tools
        .iter()
        .filter(|name| !hidden_tools.contains(name))
        .cloned()
        .collect();
    let mut prompt_sections: IndexMap<String, String> = IndexMap::new();
    match &custom_prompt {
        Some(custom_prompt) => {
            prompt_sections.insert("preamble".to_string(), custom_prompt.clone());
        }
        None => {
            prompt_sections.insert(
                "preamble".to_string(),
                "You are a coding assistant running inside pi, a coding agent harness. Help users inspect files, run commands, and modify code.".to_string(),
            );
            let visible_tools: Vec<&String> = declared_tools
                .iter()
                .filter(|name| tool_snippets.contains_key(*name))
                .collect();
            let tools = if visible_tools.is_empty() {
                "(none)".to_string()
            } else {
                visible_tools
                    .iter()
                    .map(|name| format!("- {name}: {}", tool_snippets[*name]))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            prompt_sections.insert(
                "tools".to_string(),
                format!("{tools}\n\nAdditional tools may be available."),
            );
            prompt_sections.insert(
                "rules".to_string(),
                build_rules(&declared_tools, &tool_guidelines, &prompt_guidelines),
            );
            prompt_sections.insert(
                "docs".to_string(),
                format!(
                    "Pi documentation (read only when the user asks about the harness itself, its SDK, extensions, themes, skills, or TUI):
- Main documentation: {}
- Additional docs: {}
- Examples: {} (extensions, custom tools, SDK)
- When reading pi docs or examples, resolve docs/... under Additional docs and examples/... under Examples, not the current working directory
- When asked about: extensions (docs/extensions.md, examples/extensions/), themes (docs/themes.md), skills (docs/skills.md), prompt templates (docs/prompt-templates.md), TUI components (docs/tui.md), keybindings (docs/keybindings.md), SDK integrations (docs/sdk.md), custom providers (docs/custom-provider.md), adding models (docs/models.md), packages (docs/packages.md), environment variables (docs/environment-variables.md), MCP servers (docs/mcp.md), codemode scripts and non-LLM models such as classifiers and image models (docs/codemode.md)
- Before implementing harness changes, read the relevant docs and examples in full, following related cross-references.",
                    get_readme_path(),
                    get_docs_path(),
                    get_examples_path()
                ),
            );
        }
    }

    if !append_system_prompt.is_empty() {
        prompt_sections.insert("addendum".to_string(), append_system_prompt);
    }
    if !context_files.is_empty() {
        prompt_sections.insert(
            "project_context".to_string(),
            render_project_context(&context_files),
        );
    }
    let readers = ["read", "bash"];
    let skill_file_read_tool = readers
        .iter()
        .find(|tool| declared_tools.iter().any(|declared| declared == *tool))
        .map(|tool| {
            if *tool == "read" {
                SkillFileReadTool::Read
            } else {
                SkillFileReadTool::Bash
            }
        })
        .or_else(|| {
            if readers
                .iter()
                .any(|tool| selected_tools.iter().any(|selected| selected == tool))
            {
                Some(SkillFileReadTool::Indirect)
            } else {
                None
            }
        });
    if let Some(skill_file_read_tool) = skill_file_read_tool {
        if !skills.is_empty() {
            let skills_prompt = format_skills_for_prompt(&skills, skill_file_read_tool)
                .trim()
                .to_string();
            if !skills_prompt.is_empty() {
                prompt_sections.insert("skills".to_string(), skills_prompt);
            }
        }
    }
    prompt_sections.insert("cwd".to_string(), cwd.replace('\\', "/"));
    for (name, content) in custom_sections {
        if !content.is_empty() {
            prompt_sections.insert(name, content);
        }
    }

    let mut sections: SystemPromptSections = IndexMap::new();
    let preamble = prompt_sections.shift_remove("preamble").unwrap_or_default();
    sections.insert("preamble".to_string(), preamble);
    for (name, content) in prompt_sections {
        sections.insert(name.clone(), format!("<{name}>\n{content}\n</{name}>"));
    }
    Ok(sections)
}

/// pi: `buildSystemPromptState`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPromptState {
    pub content: String,
    pub sections: Option<SystemPromptSections>,
}

/// pi: `buildSystemPromptState`
pub fn build_system_prompt_state(
    input: BuildSystemPromptOptions,
) -> Result<SystemPromptState, String> {
    if let Some(force_system_prompt) = input.force_system_prompt.clone() {
        return Ok(SystemPromptState {
            content: force_system_prompt,
            sections: None,
        });
    }
    Ok(SystemPromptState {
        content: String::new(),
        sections: Some(build_system_prompt_sections(input)?),
    })
}

/// pi: `buildSystemPrompt`
pub fn build_system_prompt(input: BuildSystemPromptOptions) -> Result<String, String> {
    let state = build_system_prompt_state(input)?;
    let message = SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text(state.content),
        sections: state.sections.map(|sections| {
            sections
                .into_iter()
                .map(|(name, content)| (name, Some(content)))
                .collect()
        }),
        tools_added: None,
        tools_removed: None,
        timestamp: 0,
    };
    Ok(get_system_message_text(&message))
}

/// pi: `diffSystemPromptSections`
pub fn diff_system_prompt_sections(
    previous: &IndexMap<String, Option<String>>,
    current: &SystemPromptSections,
) -> Option<IndexMap<String, Option<String>>> {
    let mut patch: IndexMap<String, Option<String>> = IndexMap::new();
    for (name, text) in current {
        let unchanged = previous
            .get(name)
            .and_then(|value| value.as_ref())
            .is_some_and(|value| value == text);
        if !unchanged {
            patch.insert(name.clone(), Some(text.clone()));
        }
    }
    for name in previous.keys() {
        if !current.contains_key(name) {
            patch.insert(name.clone(), None);
        }
    }
    if patch.is_empty() {
        None
    } else {
        Some(patch)
    }
}
