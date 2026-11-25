//! 对应上游 `test/skills.test.ts`：扫描、解析、校验技能与提示词里的技能块。
//!
//! 上游用 `homedir()` 的那条（`~` 展开）没搬：测试不该读用户的真实家目录。

use std::path::{Path, PathBuf};

use xjpcode_coding_agent::core::skills::{
    format_skills_for_prompt, load_skills, load_skills_from_dir, LoadSkillsOptions, Skill,
    SkillFileReadTool,
};
use xjpcode_coding_agent::core::source_info::SourceInfo;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("test/fixtures/skills")
}

fn collision_fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("test/fixtures/skills-collision")
}

fn create_test_skill(name: &str, description: &str, file_path: &str, base_dir: &str) -> Skill {
    Skill {
        name: name.to_string(),
        description: description.to_string(),
        file_path: file_path.to_string(),
        base_dir: base_dir.to_string(),
        source_info: SourceInfo::synthetic(file_path, "test", None, Some(base_dir)),
        disable_model_invocation: false,
    }
}

#[test]
fn loads_a_valid_skill() {
    let result = load_skills_from_dir(&fixtures_dir().join("valid-skill"), "test");
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "valid-skill");
    assert_eq!(
        result.skills[0].description,
        "A valid skill for testing purposes."
    );
    assert_eq!(result.skills[0].source_info.source, "test");
    assert!(result.diagnostics.is_empty());
}

#[test]
fn allows_names_that_dont_match_parent_directory() {
    let result = load_skills_from_dir(&fixtures_dir().join("name-mismatch"), "test");
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "different-name");
    assert!(!result.diagnostics.iter().any(|diagnostic| diagnostic
        .message
        .contains("does not match parent directory")));
}

#[test]
fn warns_when_name_contains_invalid_characters() {
    let result = load_skills_from_dir(&fixtures_dir().join("invalid-name-chars"), "test");
    assert_eq!(result.skills.len(), 1);
    assert!(result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message.contains("invalid characters")));
}

#[test]
fn warns_when_name_exceeds_64_characters() {
    let result = load_skills_from_dir(&fixtures_dir().join("long-name"), "test");
    assert_eq!(result.skills.len(), 1);
    assert!(result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message.contains("exceeds 64 characters")));
}

#[test]
fn warns_and_skips_skill_when_description_is_missing() {
    let result = load_skills_from_dir(&fixtures_dir().join("missing-description"), "test");
    assert_eq!(result.skills.len(), 0);
    assert!(result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message.contains("description is required")));
}

#[test]
fn ignores_unknown_frontmatter_fields() {
    let result = load_skills_from_dir(&fixtures_dir().join("unknown-field"), "test");
    assert_eq!(result.skills.len(), 1);
    assert!(result.diagnostics.is_empty());
}

#[test]
fn loads_nested_skills_recursively() {
    let result = load_skills_from_dir(&fixtures_dir().join("nested"), "test");
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "child-skill");
    assert!(result.diagnostics.is_empty());
}

#[test]
fn prefers_a_directorys_root_skill_md_over_nested_ones() {
    let result = load_skills_from_dir(&fixtures_dir().join("root-skill-preferred"), "test");
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "root-skill-preferred");
    assert_eq!(result.skills[0].description, "Root skill should win.");
    assert!(result.diagnostics.is_empty());
}

#[test]
fn skips_files_without_frontmatter() {
    let result = load_skills_from_dir(&fixtures_dir().join("no-frontmatter"), "test");
    assert_eq!(result.skills.len(), 0);
    assert!(result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message.contains("description is required")));
}

#[test]
fn warns_and_skips_skill_when_yaml_frontmatter_is_invalid() {
    let result = load_skills_from_dir(&fixtures_dir().join("invalid-yaml"), "test");
    assert_eq!(result.skills.len(), 0);
    assert!(result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message.contains("at line")));
}

#[test]
fn preserves_multiline_descriptions_from_yaml() {
    let result = load_skills_from_dir(&fixtures_dir().join("multiline-description"), "test");
    assert_eq!(result.skills.len(), 1);
    assert!(result.skills[0].description.contains('\n'));
    assert!(result.skills[0]
        .description
        .contains("This is a multiline description."));
    assert!(result.diagnostics.is_empty());
}

#[test]
fn warns_when_name_contains_consecutive_hyphens() {
    let result = load_skills_from_dir(&fixtures_dir().join("consecutive-hyphens"), "test");
    assert_eq!(result.skills.len(), 1);
    assert!(result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message.contains("consecutive hyphens")));
}

#[test]
fn loads_all_skills_from_fixture_directory() {
    let result = load_skills_from_dir(&fixtures_dir(), "test");
    assert!(result.skills.len() >= 6);
}

#[test]
fn returns_empty_for_non_existent_directory() {
    let result = load_skills_from_dir(Path::new("/non/existent/path"), "test");
    assert!(result.skills.is_empty());
    assert!(result.diagnostics.is_empty());
}

#[test]
fn parses_disable_model_invocation_frontmatter_field() {
    let result = load_skills_from_dir(&fixtures_dir().join("disable-model-invocation"), "test");
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "disable-model-invocation");
    assert!(result.skills[0].disable_model_invocation);
    assert!(!result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message.contains("unknown frontmatter field")));
}

#[test]
fn defaults_disable_model_invocation_to_false() {
    let result = load_skills_from_dir(&fixtures_dir().join("valid-skill"), "test");
    assert!(!result.skills[0].disable_model_invocation);
}

#[test]
fn formats_skills_as_xml() {
    let skills = vec![create_test_skill(
        "test-skill",
        "A test skill.",
        "/path/to/skill/SKILL.md",
        "/path/to/skill",
    )];
    let text = format_skills_for_prompt(&skills, SkillFileReadTool::Read);
    assert!(text.contains("<available_skills>"));
    assert!(text.contains("</available_skills>"));
    assert!(text.contains("<name>test-skill</name>"));
    assert!(text.contains("<description>A test skill.</description>"));
    assert!(text.contains("<location>/path/to/skill/SKILL.md</location>"));
    assert!(text.contains("The following skills provide specialized instructions"));
    assert!(text.contains("Use the read tool to load a skill's file"));
}

#[test]
fn escapes_xml_special_characters() {
    let skills = vec![create_test_skill(
        "test-skill",
        "A skill with <special> & \"characters\".",
        "/path/to/skill/SKILL.md",
        "/path/to/skill",
    )];
    let text = format_skills_for_prompt(&skills, SkillFileReadTool::Read);
    assert!(text.contains("&lt;special&gt;"));
    assert!(text.contains("&amp;"));
    assert!(text.contains("&quot;characters&quot;"));
}

#[test]
fn formats_multiple_skills() {
    let skills = vec![
        create_test_skill(
            "skill-one",
            "First skill.",
            "/path/one/SKILL.md",
            "/path/one",
        ),
        create_test_skill(
            "skill-two",
            "Second skill.",
            "/path/two/SKILL.md",
            "/path/two",
        ),
    ];
    let text = format_skills_for_prompt(&skills, SkillFileReadTool::Read);
    assert!(text.contains("<name>skill-one</name>"));
    assert!(text.contains("<name>skill-two</name>"));
    assert_eq!(text.matches("<skill>").count(), 2);
}

#[test]
fn excludes_skills_with_disable_model_invocation_from_prompt() {
    let mut visible = create_test_skill(
        "visible-skill",
        "A visible skill.",
        "/path/visible/SKILL.md",
        "/path/visible",
    );
    visible.disable_model_invocation = false;
    let mut hidden = create_test_skill(
        "hidden-skill",
        "A hidden skill.",
        "/path/hidden/SKILL.md",
        "/path/hidden",
    );
    hidden.disable_model_invocation = true;
    let text = format_skills_for_prompt(&[visible, hidden], SkillFileReadTool::Read);
    assert!(text.contains("<name>visible-skill</name>"));
    assert!(!text.contains("<name>hidden-skill</name>"));
    assert_eq!(text.matches("<skill>").count(), 1);
}

#[test]
fn returns_empty_string_when_all_skills_are_hidden() {
    let mut skill = create_test_skill(
        "hidden-skill",
        "A hidden skill.",
        "/path/hidden/SKILL.md",
        "/path/hidden",
    );
    skill.disable_model_invocation = true;
    assert_eq!(
        format_skills_for_prompt(&[skill], SkillFileReadTool::Read),
        ""
    );
    assert_eq!(format_skills_for_prompt(&[], SkillFileReadTool::Read), "");
}

#[test]
fn loads_from_explicit_skill_paths() {
    let agent_dir = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    let result = load_skills(LoadSkillsOptions {
        cwd: cwd.path().to_path_buf(),
        agent_dir: agent_dir.path().to_path_buf(),
        skill_paths: vec![fixtures_dir()
            .join("valid-skill")
            .to_string_lossy()
            .into_owned()],
        include_defaults: true,
    });
    assert_eq!(result.skills.len(), 1);
    assert_eq!(
        result.skills[0].source_info.scope,
        xjpcode_coding_agent::core::source_info::SourceScope::Temporary
    );
    assert!(result.diagnostics.is_empty());
}

#[test]
fn warns_when_skill_path_does_not_exist() {
    let agent_dir = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    let result = load_skills(LoadSkillsOptions {
        cwd: cwd.path().to_path_buf(),
        agent_dir: agent_dir.path().to_path_buf(),
        skill_paths: vec!["/non/existent/path".to_string()],
        include_defaults: true,
    });
    assert!(result.skills.is_empty());
    assert!(result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message.contains("does not exist")));
}

#[test]
fn detects_name_collisions_and_keeps_the_first_skill() {
    let first = load_skills_from_dir(&collision_fixtures_dir().join("first"), "first");
    let second = load_skills_from_dir(&collision_fixtures_dir().join("second"), "second");
    assert_eq!(first.skills.len(), 1);
    assert_eq!(second.skills.len(), 1);
    assert_eq!(first.skills[0].name, "calendar");
    assert_eq!(second.skills[0].name, "calendar");
    // `loadSkills`（资源加载器那条路）的合并规则：先来的赢，后来的记一条 collision。
    let agent_dir = tempfile::TempDir::new().unwrap();
    let cwd = tempfile::TempDir::new().unwrap();
    let result = load_skills(LoadSkillsOptions {
        cwd: cwd.path().to_path_buf(),
        agent_dir: agent_dir.path().to_path_buf(),
        skill_paths: vec![
            collision_fixtures_dir()
                .join("first")
                .to_string_lossy()
                .into_owned(),
            collision_fixtures_dir()
                .join("second")
                .to_string_lossy()
                .into_owned(),
        ],
        include_defaults: true,
    });
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].source_info.source, "path");
    assert!(result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.r#type == "collision"));
}
