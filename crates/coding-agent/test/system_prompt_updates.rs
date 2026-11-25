//! 对应上游 `test/system-prompt-updates.test.ts` 里不依赖会话的几条：片段的 diff、开头不包标签、
//! 强制提示词不拆段、非法片段名报错。会话级的那几条等应用层（见 `docs/port/milestone-3.md`）。

use indexmap::IndexMap;
use xjpcode_coding_agent::core::system_prompt::{
    build_system_prompt_sections, build_system_prompt_state, diff_system_prompt_sections,
    BuildSystemPromptOptions,
};

fn sections(options: BuildSystemPromptOptions) -> IndexMap<String, String> {
    build_system_prompt_sections(options).unwrap()
}

fn as_previous(sections: &IndexMap<String, String>) -> IndexMap<String, Option<String>> {
    sections
        .iter()
        .map(|(name, content)| (name.clone(), Some(content.clone())))
        .collect()
}

#[test]
fn diffs_sections_into_a_patch() {
    let mut with_section = IndexMap::new();
    with_section.insert("plan_mode".to_string(), "Plan only.".to_string());
    let previous = sections(BuildSystemPromptOptions {
        cwd: "/tmp".to_string(),
        sections: Some(with_section),
        ..Default::default()
    });
    let mut changed_section = IndexMap::new();
    changed_section.insert("plan_mode".to_string(), "Implementation allowed.".to_string());
    let current = sections(BuildSystemPromptOptions {
        cwd: "/tmp".to_string(),
        sections: Some(changed_section),
        ..Default::default()
    });

    let patch = diff_system_prompt_sections(&as_previous(&previous), &current).unwrap();
    assert_eq!(
        patch.get("plan_mode").cloned().flatten().unwrap(),
        "<plan_mode>\nImplementation allowed.\n</plan_mode>"
    );
    assert_eq!(patch.len(), 1);
    assert_eq!(diff_system_prompt_sections(&as_previous(&previous), &previous), None);
    assert_eq!(
        diff_system_prompt_sections(
            &as_previous(&previous),
            &sections(BuildSystemPromptOptions {
                cwd: "/tmp".to_string(),
                ..Default::default()
            })
        )
        .unwrap()
        .get("plan_mode")
        .cloned(),
        Some(None)
    );
}

#[test]
fn keeps_the_preamble_untagged_and_replaces_it_like_any_section() {
    let previous = sections(BuildSystemPromptOptions {
        custom_prompt: Some("You are A.".to_string()),
        cwd: "/tmp".to_string(),
        ..Default::default()
    });
    let current = sections(BuildSystemPromptOptions {
        custom_prompt: Some("You are B.".to_string()),
        cwd: "/tmp".to_string(),
        ..Default::default()
    });
    assert_eq!(previous.get("preamble").unwrap(), "You are A.");
    assert_eq!(
        diff_system_prompt_sections(&as_previous(&previous), &current)
            .unwrap()
            .get("preamble")
            .cloned()
            .flatten()
            .unwrap(),
        "You are B."
    );

    let forced = build_system_prompt_state(BuildSystemPromptOptions {
        force_system_prompt: Some("Exact prompt.".to_string()),
        cwd: "/tmp".to_string(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(forced.content, "Exact prompt.");
    assert!(forced.sections.is_none());

    let default_state = build_system_prompt_state(BuildSystemPromptOptions {
        cwd: "/tmp".to_string(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(default_state.content, "");
    assert_eq!(
        default_state.sections.unwrap(),
        sections(BuildSystemPromptOptions {
            cwd: "/tmp".to_string(),
            ..Default::default()
        })
    );

    let mut invalid = IndexMap::new();
    invalid.insert("preamble".to_string(), "x".to_string());
    let error = build_system_prompt_sections(BuildSystemPromptOptions {
        cwd: "/tmp".to_string(),
        sections: Some(invalid),
        ..Default::default()
    })
    .unwrap_err();
    assert!(error.contains("Invalid system prompt section name"));
}
