//! 对应上游 `src/core/skills.ts`：扫描、解析、校验技能，以及系统提示词里的技能块。
//!
//! 上游用 `ignore` 库读 `.gitignore`/`.ignore`/`.fdignore`，Rust 侧用同名的 `ignore` crate 的
//! `GitignoreBuilder`；目录规则照上游：一个目录里有 `SKILL.md` 就当作技能根、不再往里递归，
//! 否则递归子目录，并在顶层目录收直接放在里面的 `.md`。

use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::core::source_info::{get_resource_path_key, SourceInfo, SourceScope};
use crate::utils::text::strip_bom;

/// `/` 分隔的相对路径（上游 `toPosixPath`）。
fn to_posix_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

/// pi: `IGNORE_FILE_NAMES`
const IGNORE_FILE_NAMES: [&str; 3] = [".gitignore", ".ignore", ".fdignore"];

/// pi: `prefixIgnorePattern`——把子目录里的规则前缀成相对根目录的写法。
fn prefix_ignore_pattern(line: &str, prefix: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('#') && !trimmed.starts_with("\\#") {
        return None;
    }

    let mut pattern = line;
    let mut negated = false;
    if let Some(rest) = pattern.strip_prefix('!') {
        negated = true;
        pattern = rest;
    } else if let Some(rest) = pattern.strip_prefix("\\!") {
        pattern = rest;
    }
    if let Some(rest) = pattern.strip_prefix('/') {
        pattern = rest;
    }
    let prefixed = format!("{prefix}{pattern}");
    Some(if negated {
        format!("!{prefixed}")
    } else {
        prefixed
    })
}

fn add_ignore_rules(builder: &mut GitignoreBuilder, dir: &Path, root: &Path) {
    let relative_dir = dir.strip_prefix(root).unwrap_or(Path::new(""));
    let prefix = if relative_dir.as_os_str().is_empty() {
        String::new()
    } else {
        format!("{}/", to_posix_path(relative_dir))
    };
    for filename in IGNORE_FILE_NAMES {
        let ignore_path = dir.join(filename);
        let Ok(content) = std::fs::read_to_string(&ignore_path) else {
            continue;
        };
        for line in content.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if let Some(pattern) = prefix_ignore_pattern(line, &prefix) {
                let _ = builder.add_line(Some(ignore_path.clone()), &pattern);
            }
        }
    }
}

/// 一棵技能目录扫描时共用的忽略规则。
struct IgnoreMatcher {
    root: PathBuf,
    builder: GitignoreBuilder,
    matcher: Option<Gitignore>,
}

impl IgnoreMatcher {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            builder: GitignoreBuilder::new(root),
            matcher: None,
        }
    }

    fn add_dir(&mut self, dir: &Path) {
        add_ignore_rules(&mut self.builder, dir, &self.root);
        // 规则加过就重建；上游每次 `ig.add` 之后立刻能查。
        self.matcher = self.builder.build().ok();
    }

    fn ignored(&self, relative_path: &str, is_dir: bool) -> bool {
        match &self.matcher {
            Some(matcher) => matcher.matched(relative_path, is_dir).is_ignore(),
            None => false,
        }
    }
}

/// pi: `Skill`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub file_path: String,
    pub base_dir: String,
    pub source_info: SourceInfo,
    pub disable_model_invocation: bool,
}

/// pi: `ResourceDiagnostic`（本层用到的字段）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceDiagnostic {
    pub r#type: String,
    pub message: String,
    pub path: String,
}

impl ResourceDiagnostic {
    pub fn warning(message: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            r#type: "warning".to_string(),
            message: message.into(),
            path: path.into(),
        }
    }

    pub fn error(message: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            r#type: "error".to_string(),
            message: message.into(),
            path: path.into(),
        }
    }
}

/// pi: `LoadSkillsResult`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadSkillsResult {
    pub skills: Vec<Skill>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

/// pi: `SkillFrontmatter`——frontmatter 里技能用到的字段。上游返回一张任意键的表；
/// 这里只留读到的三项。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SkillFrontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub disable_model_invocation: bool,
}

/// pi: `stripFrontmatter`——去掉 frontmatter，正文前后空白去掉。
pub fn strip_frontmatter(content: &str) -> String {
    let normalized = normalize_frontmatter_input(content);
    if !normalized.starts_with("---") {
        return normalized;
    }
    let Some(end_index) = normalized[3..].find("\n---").map(|index| index + 3) else {
        return normalized;
    };
    normalized[end_index + 4..].trim().to_string()
}

fn normalize_frontmatter_input(content: &str) -> String {
    strip_bom(content).replace("\r\n", "\n").replace('\r', "\n")
}

/// pi: `parseFrontmatter`（`utils/frontmatter.ts`）
pub fn parse_frontmatter(content: &str) -> Result<(SkillFrontmatter, String), String> {
    let normalized = normalize_frontmatter_input(content);
    if !normalized.starts_with("---") {
        return Ok((SkillFrontmatter::default(), normalized));
    }
    let Some(end_index) = normalized[3..].find("\n---").map(|index| index + 3) else {
        return Ok((SkillFrontmatter::default(), normalized));
    };
    let yaml_string = &normalized[4..end_index];
    let body = normalized[end_index + 4..].trim().to_string();
    let value: serde_yaml::Value =
        serde_yaml::from_str(yaml_string).map_err(|error| error.to_string())?;
    let mapping = match value {
        serde_yaml::Value::Null => serde_yaml::Mapping::new(),
        serde_yaml::Value::Mapping(mapping) => mapping,
        other => {
            return Err(format!(
                "frontmatter must be a YAML mapping, not {}",
                match other {
                    serde_yaml::Value::Sequence(_) => "a sequence",
                    serde_yaml::Value::String(_) => "a string",
                    _ => "a scalar",
                }
            ));
        }
    };
    let mut frontmatter = SkillFrontmatter::default();
    for (key, value) in mapping {
        let Some(key) = key.as_str() else {
            continue;
        };
        match key {
            "name" => {
                frontmatter.name = value.as_str().map(ToString::to_string);
            }
            "description" => {
                frontmatter.description = value.as_str().map(ToString::to_string);
            }
            "disable-model-invocation" => {
                frontmatter.disable_model_invocation = value.as_bool().unwrap_or(false);
            }
            _ => {}
        }
    }
    Ok((frontmatter, body))
}

const MAX_NAME_LENGTH: usize = 64;
const MAX_DESCRIPTION_LENGTH: usize = 1024;

/// pi: `validateName`
fn validate_name(name: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if name.chars().count() > MAX_NAME_LENGTH {
        errors.push(format!(
            "name exceeds {MAX_NAME_LENGTH} characters ({})",
            name.chars().count()
        ));
    }
    if !name.chars().all(|character| {
        character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
    }) {
        errors.push(
            "name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)"
                .to_string(),
        );
    }
    if name.starts_with('-') || name.ends_with('-') {
        errors.push("name must not start or end with a hyphen".to_string());
    }
    if name.contains("--") {
        errors.push("name must not contain consecutive hyphens".to_string());
    }
    errors
}

/// pi: `validateDescription`
fn validate_description(description: Option<&str>) -> Vec<String> {
    let mut errors = Vec::new();
    match description {
        None => errors.push("description is required".to_string()),
        Some(description) if description.trim().is_empty() => {
            errors.push("description is required".to_string());
        }
        Some(description) if description.chars().count() > MAX_DESCRIPTION_LENGTH => {
            errors.push(format!(
                "description exceeds {MAX_DESCRIPTION_LENGTH} characters ({})",
                description.chars().count()
            ));
        }
        Some(_) => {}
    }
    errors
}

fn create_skill_source_info(file_path: &str, base_dir: &str, source: &str) -> SourceInfo {
    let scope = match source {
        "user" => Some(SourceScope::User),
        "project" => Some(SourceScope::Project),
        _ => None,
    };
    SourceInfo::synthetic(file_path, source, scope, Some(base_dir))
}

/// 单个技能文件（上游 `loadSkillFromFile` 的外壳，`loadSkills` 的 `skillPaths` 用）。
pub fn load_skill_file(file_path: &Path, source: &str) -> LoadSkillsResult {
    let result = load_skill_from_file(file_path, source);
    LoadSkillsResult {
        skills: result.skill.into_iter().collect(),
        diagnostics: result.diagnostics,
    }
}

/// pi: `loadSkillsFromDir`
pub fn load_skills_from_dir(dir: &Path, source: &str) -> LoadSkillsResult {
    load_skills_from_dir_internal(dir, source, true, None, None)
}

/// 自动发现目录里的技能（上游 `collectSkillEntries`）：`pi` 模式的目录收根部的 `.md`，
/// `agents` 模式的目录（`.agents/skills`）只收带 `SKILL.md` 的子目录。
pub fn load_skills_from_dir_with_root_files(
    dir: &Path,
    source: &str,
    include_root_files: bool,
) -> LoadSkillsResult {
    load_skills_from_dir_internal(dir, source, include_root_files, None, None)
}

fn load_skills_from_dir_internal(
    dir: &Path,
    source: &str,
    include_root_files: bool,
    ignore_matcher: Option<&mut IgnoreMatcher>,
    root_dir: Option<&Path>,
) -> LoadSkillsResult {
    let mut skills = Vec::new();
    let mut diagnostics = Vec::new();
    if !dir.exists() {
        return LoadSkillsResult {
            skills,
            diagnostics,
        };
    }

    let root = root_dir.unwrap_or(dir).to_path_buf();
    let mut owned_matcher;
    let matcher = match ignore_matcher {
        Some(matcher) => matcher,
        None => {
            owned_matcher = IgnoreMatcher::new(&root);
            &mut owned_matcher
        }
    };
    matcher.add_dir(dir);

    let Ok(entries) = std::fs::read_dir(dir) else {
        return LoadSkillsResult {
            skills,
            diagnostics,
        };
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in &entries {
        if entry.file_name() != "SKILL.md" {
            continue;
        }
        let full_path = entry.path();
        let is_file = entry
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false);
        if !is_file {
            continue;
        }
        let relative = full_path.strip_prefix(&root).unwrap_or(&full_path);
        if matcher.ignored(&to_posix_path(relative), false) {
            continue;
        }
        let result = load_skill_from_file(&full_path, source);
        if let Some(skill) = result.skill {
            skills.push(skill);
        }
        diagnostics.extend(result.diagnostics);
        return LoadSkillsResult {
            skills,
            diagnostics,
        };
    }

    for entry in &entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let full_path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let (is_directory, is_file) = if file_type.is_symlink() {
            match std::fs::metadata(&full_path) {
                Ok(metadata) => (metadata.is_dir(), metadata.is_file()),
                Err(_) => continue,
            }
        } else {
            (file_type.is_dir(), file_type.is_file())
        };
        let relative = full_path.strip_prefix(&root).unwrap_or(&full_path);
        let relative_text = to_posix_path(relative);
        let ignore_path = if is_directory {
            format!("{relative_text}/")
        } else {
            relative_text
        };
        if matcher.ignored(&ignore_path, is_directory) {
            continue;
        }
        if is_directory {
            let sub_result = load_skills_from_dir_internal(
                &full_path,
                source,
                false,
                Some(matcher),
                Some(&root),
            );
            skills.extend(sub_result.skills);
            diagnostics.extend(sub_result.diagnostics);
            continue;
        }
        if !is_file || !include_root_files || !name.ends_with(".md") {
            continue;
        }
        let result = load_skill_from_file(&full_path, source);
        if let Some(skill) = result.skill {
            skills.push(skill);
        }
        diagnostics.extend(result.diagnostics);
    }

    LoadSkillsResult {
        skills,
        diagnostics,
    }
}

struct SingleSkillResult {
    skill: Option<Skill>,
    diagnostics: Vec<ResourceDiagnostic>,
}

fn load_skill_from_file(file_path: &Path, source: &str) -> SingleSkillResult {
    let mut diagnostics = Vec::new();
    let path_text = file_path.to_string_lossy().into_owned();
    let is_declared_skill = file_path.file_name().is_some_and(|name| name == "SKILL.md");

    let raw_content = match std::fs::read_to_string(file_path) {
        Ok(content) => content,
        Err(error) => {
            diagnostics.push(ResourceDiagnostic::warning(error.to_string(), path_text));
            return SingleSkillResult {
                skill: None,
                diagnostics,
            };
        }
    };

    let frontmatter = match parse_frontmatter(&raw_content) {
        Ok((frontmatter, _body)) => frontmatter,
        Err(error) => {
            if is_declared_skill {
                diagnostics.push(ResourceDiagnostic::warning(error, path_text));
            }
            return SingleSkillResult {
                skill: None,
                diagnostics,
            };
        }
    };

    let has_description = frontmatter
        .description
        .as_deref()
        .is_some_and(|description| !description.trim().is_empty());
    if !is_declared_skill && !has_description {
        return SingleSkillResult {
            skill: None,
            diagnostics,
        };
    }

    let skill_dir = file_path.parent().unwrap_or(Path::new(""));
    let parent_dir_name = skill_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    for error in validate_description(frontmatter.description.as_deref()) {
        diagnostics.push(ResourceDiagnostic::warning(error, path_text.clone()));
    }
    let name = frontmatter.name.clone().unwrap_or(parent_dir_name);
    for error in validate_name(&name) {
        diagnostics.push(ResourceDiagnostic::warning(error, path_text.clone()));
    }
    if !has_description {
        return SingleSkillResult {
            skill: None,
            diagnostics,
        };
    }
    let description = frontmatter.description.unwrap_or_default();
    let base_dir = skill_dir.to_string_lossy().into_owned();
    let source_info = create_skill_source_info(&path_text, &base_dir, source);
    SingleSkillResult {
        skill: Some(Skill {
            name,
            description,
            file_path: path_text,
            base_dir,
            source_info,
            disable_model_invocation: frontmatter.disable_model_invocation,
        }),
        diagnostics,
    }
}

/// pi: `formatSkillsForPrompt`
pub fn format_skills_for_prompt(skills: &[Skill], file_read_tool: SkillFileReadTool) -> String {
    let visible_skills: Vec<&Skill> = skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .collect();
    if visible_skills.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = vec![
        String::new(),
        String::new(),
        "The following skills provide specialized instructions for specific tasks.".to_string(),
        match file_read_tool {
            SkillFileReadTool::Read => {
                "Use the read tool to load a skill's file when the task matches its description."
                    .to_string()
            }
            SkillFileReadTool::Bash => {
                "Use bash to load a skill's file when the task matches its description.".to_string()
            }
            SkillFileReadTool::Indirect => {
                "Load a skill's file when the task matches its description.".to_string()
            }
        },
        "Resolve relative paths against the skill's directory (parent of SKILL.md). Use absolute paths in tool calls.".to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ];
    for skill in visible_skills {
        lines.push("  <skill>".to_string());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        lines.push(format!(
            "    <description>{}</description>",
            escape_xml(&skill.description)
        ));
        lines.push(format!(
            "    <location>{}</location>",
            escape_xml(&skill.file_path)
        ));
        lines.push("  </skill>".to_string());
    }
    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

/// pi: `fileReadTool` 的三种取值
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillFileReadTool {
    Read,
    Bash,
    Indirect,
}

impl Default for SkillFileReadTool {
    fn default() -> Self {
        Self::Read
    }
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// pi: `LoadSkillsOptions`
pub struct LoadSkillsOptions {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub skill_paths: Vec<String>,
    pub include_defaults: bool,
}

/// pi: `loadSkills`
pub fn load_skills(options: LoadSkillsOptions) -> LoadSkillsResult {
    let LoadSkillsOptions {
        cwd,
        agent_dir,
        skill_paths,
        include_defaults,
    } = options;
    let resolved_cwd = crate::utils::paths::resolve_path(
        &cwd.to_string_lossy(),
        &std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy(),
        &crate::utils::paths::PathInputOptions::with_defaults(),
    );
    let resolved_agent_dir = crate::utils::paths::resolve_path(
        &agent_dir.to_string_lossy(),
        &resolved_cwd.to_string_lossy(),
        &crate::utils::paths::PathInputOptions::with_defaults(),
    );

    let mut skill_map: indexmap::IndexMap<String, Skill> = indexmap::IndexMap::new();
    let mut real_path_set = std::collections::HashSet::new();
    let mut all_diagnostics = Vec::new();
    let mut collision_diagnostics = Vec::new();

    let add_skills = |result: LoadSkillsResult,
                      skill_map: &mut indexmap::IndexMap<String, Skill>,
                      real_path_set: &mut std::collections::HashSet<String>,
                      all_diagnostics: &mut Vec<ResourceDiagnostic>,
                      collision_diagnostics: &mut Vec<ResourceDiagnostic>| {
        all_diagnostics.extend(result.diagnostics);
        for skill in result.skills {
            let real_path = get_resource_path_key(Path::new(&skill.file_path));
            if real_path_set.contains(&real_path) {
                continue;
            }
            if let Some(existing) = skill_map.get(&skill.name) {
                collision_diagnostics.push(ResourceDiagnostic {
                    r#type: "collision".to_string(),
                    message: format!("name \"{}\" collision", skill.name),
                    path: skill.file_path.clone(),
                });
                let _ = existing;
            } else {
                skill_map.insert(skill.name.clone(), skill);
                real_path_set.insert(real_path);
            }
        }
    };

    if include_defaults {
        let user_result = load_skills_from_dir(&resolved_agent_dir.join("skills"), "user");
        add_skills(
            user_result,
            &mut skill_map,
            &mut real_path_set,
            &mut all_diagnostics,
            &mut collision_diagnostics,
        );
        let project_result = load_skills_from_dir(
            &resolved_cwd
                .join(crate::config::CONFIG_DIR_NAME)
                .join("skills"),
            "project",
        );
        add_skills(
            project_result,
            &mut skill_map,
            &mut real_path_set,
            &mut all_diagnostics,
            &mut collision_diagnostics,
        );
    }

    let user_skills_dir = resolved_agent_dir.join("skills");
    let project_skills_dir = resolved_cwd
        .join(crate::config::CONFIG_DIR_NAME)
        .join("skills");
    let source_for = |resolved_path: &Path| -> &'static str {
        if !include_defaults {
            if crate::utils::paths::is_path_within(&user_skills_dir, resolved_path) {
                return "user";
            }
            if crate::utils::paths::is_path_within(&project_skills_dir, resolved_path) {
                return "project";
            }
        }
        "path"
    };

    for raw_path in &skill_paths {
        let resolved_path = crate::utils::paths::resolve_path(
            raw_path,
            &resolved_cwd.to_string_lossy(),
            &crate::utils::paths::PathInputOptions {
                trim: true,
                ..crate::utils::paths::PathInputOptions::with_defaults()
            },
        );
        if !resolved_path.exists() {
            all_diagnostics.push(ResourceDiagnostic::warning(
                "skill path does not exist",
                resolved_path.to_string_lossy(),
            ));
            continue;
        }
        let source = source_for(&resolved_path);
        match std::fs::metadata(&resolved_path) {
            Ok(metadata) if metadata.is_dir() => {
                let result = load_skills_from_dir(&resolved_path, source);
                add_skills(
                    result,
                    &mut skill_map,
                    &mut real_path_set,
                    &mut all_diagnostics,
                    &mut collision_diagnostics,
                );
            }
            Ok(metadata)
                if metadata.is_file()
                    && resolved_path
                        .extension()
                        .is_some_and(|extension| extension == "md") =>
            {
                let result = load_skill_from_file(&resolved_path, source);
                if let Some(skill) = result.skill {
                    add_skills(
                        LoadSkillsResult {
                            skills: vec![skill],
                            diagnostics: result.diagnostics,
                        },
                        &mut skill_map,
                        &mut real_path_set,
                        &mut all_diagnostics,
                        &mut collision_diagnostics,
                    );
                } else {
                    all_diagnostics.extend(result.diagnostics);
                }
            }
            Ok(_) => all_diagnostics.push(ResourceDiagnostic::warning(
                "skill path is not a markdown file",
                resolved_path.to_string_lossy(),
            )),
            Err(error) => all_diagnostics.push(ResourceDiagnostic::warning(
                error.to_string(),
                resolved_path.to_string_lossy(),
            )),
        }
    }

    let mut diagnostics = all_diagnostics;
    diagnostics.extend(collision_diagnostics);
    LoadSkillsResult {
        skills: skill_map.into_values().collect(),
        diagnostics,
    }
}
