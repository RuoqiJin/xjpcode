//! 对应上游 `src/core/resource-loader.ts` 中本里程碑用到的部分：规矩文件与技能。
//!
//! 上游的加载器还管扩展、提示词模板、主题、包安装；这一层只有规矩文件（全局那份 + 从 cwd 往上
//! 各层）和技能（项目各层的 `.agents/skills`、用户 `agentDir/skills`、用户 `~/.agents/skills`）。
//! 项目信任（`project-trust.ts`）还没搬：项目里的技能一律当可信，见 `docs/port/milestone-4.md`。

use std::path::{Path, PathBuf};

use crate::core::skills::{
    load_skill_file, load_skills_from_dir, load_skills_from_dir_with_root_files, LoadSkillsResult,
    ResourceDiagnostic, Skill,
};
use crate::core::source_info::get_resource_path_key;
use crate::utils::paths::{
    canonicalize_path, is_path_within, paths_equal, resolve_path, PathInputOptions,
};
use crate::utils::text::strip_bom;

/// pi: `loadContextFileFromDir` 的候选文件名（按顺序取第一个存在的）。
const CONTEXT_FILE_CANDIDATES: [&str; 5] = [
    "AGENTS.override.md",
    "AGENTS.md",
    "AGENTS.MD",
    "CLAUDE.md",
    "CLAUDE.MD",
];

/// pi: `loadContextFileFromDir`
fn load_context_file_from_dir(dir: &Path) -> Option<(PathBuf, String)> {
    for filename in CONTEXT_FILE_CANDIDATES {
        let file_path = dir.join(filename);
        if !file_path.exists() {
            continue;
        }
        let Ok(metadata) = std::fs::metadata(&file_path) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        match std::fs::read_to_string(&file_path) {
            Ok(content) => return Some((file_path, strip_bom(&content).to_string())),
            Err(_) => continue,
        }
    }
    None
}

/// pi: `findGitPaths`
pub struct GitPaths {
    pub repo_dir: PathBuf,
    pub common_git_dir: PathBuf,
    pub head_path: PathBuf,
}

/// pi: `findGitPaths`——从 `cwd` 往上找 `.git`，认出 worktree 的 `gitdir:` 文件。
pub fn find_git_paths(cwd: &Path) -> Option<GitPaths> {
    let mut dir = cwd.to_path_buf();
    loop {
        let git_path = dir.join(".git");
        if git_path.exists() {
            let metadata = std::fs::metadata(&git_path).ok()?;
            if metadata.is_file() {
                let content = std::fs::read_to_string(&git_path).ok()?;
                let content = content.trim();
                if let Some(rest) = content.strip_prefix("gitdir: ") {
                    let git_dir = resolve_path(
                        rest.trim(),
                        &dir.to_string_lossy(),
                        &PathInputOptions::with_defaults(),
                    );
                    let head_path = git_dir.join("HEAD");
                    if !head_path.exists() {
                        return None;
                    }
                    let common_dir_path = git_dir.join("commondir");
                    let common_git_dir = if common_dir_path.exists() {
                        let value = std::fs::read_to_string(&common_dir_path).ok()?;
                        resolve_path(
                            value.trim(),
                            &git_dir.to_string_lossy(),
                            &PathInputOptions::with_defaults(),
                        )
                    } else {
                        git_dir.clone()
                    };
                    return Some(GitPaths {
                        repo_dir: dir,
                        common_git_dir,
                        head_path,
                    });
                }
            } else if metadata.is_dir() {
                let head_path = git_path.join("HEAD");
                if !head_path.exists() {
                    return None;
                }
                return Some(GitPaths {
                    repo_dir: dir,
                    common_git_dir: git_path,
                    head_path,
                });
            }
        }
        let Some(parent) = dir.parent() else {
            return None;
        };
        if parent == dir {
            return None;
        }
        dir = parent.to_path_buf();
    }
}

/// pi: `findShadowedContextFile`——worktree 自己的规矩文件会盖住主仓库同一份。
fn find_shadowed_context_file(cwd: &Path) -> Option<PathBuf> {
    let git_paths = find_git_paths(cwd)?;
    let common_git_dir = canonicalize_path(&git_paths.common_git_dir);
    let worktree_root = canonicalize_path(&git_paths.repo_dir);
    let main_repo_root = common_git_dir.parent()?.to_path_buf();
    if !is_path_within(&main_repo_root, &worktree_root)
        || paths_equal(&worktree_root, &main_repo_root)
    {
        return None;
    }
    if !paths_equal(
        &canonicalize_path(&main_repo_root.join(".git")),
        &common_git_dir,
    ) {
        return None;
    }
    let worktree_context = load_context_file_from_dir(&worktree_root)?;
    let filename = worktree_context.0.file_name()?.to_owned();
    Some(main_repo_root.join(filename))
}

/// pi: `loadProjectContextFiles`
pub fn load_project_context_files(cwd: &Path, agent_dir: &Path) -> Vec<(String, String)> {
    let resolved_cwd = resolve_path(
        &cwd.to_string_lossy(),
        &std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy(),
        &PathInputOptions::with_defaults(),
    );
    let resolved_agent_dir = resolve_path(
        &agent_dir.to_string_lossy(),
        &resolved_cwd.to_string_lossy(),
        &PathInputOptions::with_defaults(),
    );

    let mut context_files: Vec<(String, String)> = Vec::new();
    let mut seen_paths: std::collections::HashSet<String> = std::collections::HashSet::new();

    if let Some((path, content)) = load_context_file_from_dir(&resolved_agent_dir) {
        seen_paths.insert(path.to_string_lossy().into_owned());
        context_files.push((path.to_string_lossy().into_owned(), content));
    }

    let mut ancestor_context_files: Vec<(String, String)> = Vec::new();
    let shadowed_context_file = find_shadowed_context_file(&resolved_cwd);
    let mut current_dir = resolved_cwd;
    loop {
        let context_file = load_context_file_from_dir(&current_dir);
        let is_shadowed = match (&shadowed_context_file, &context_file) {
            (Some(shadowed), Some((path, _))) => path
                .file_name()
                .map(|name| canonicalize_path(&current_dir).join(name))
                .is_some_and(|path| paths_equal(&path, shadowed)),
            _ => false,
        };
        if let Some((path, content)) = context_file {
            let path_text = path.to_string_lossy().into_owned();
            if !is_shadowed && !seen_paths.contains(&path_text) {
                seen_paths.insert(path_text.clone());
                ancestor_context_files.insert(0, (path_text, content));
            }
        }
        let Some(parent) = current_dir.parent() else {
            break;
        };
        if parent == current_dir {
            break;
        }
        current_dir = parent.to_path_buf();
    }
    context_files.extend(ancestor_context_files);
    context_files
}

/// pi: `collectAncestorAgentsSkillDirs`——从 cwd 往上，到 git 仓库根为止。
fn collect_ancestor_agents_skill_dirs(start_dir: &Path) -> Vec<PathBuf> {
    let mut skill_dirs = Vec::new();
    let resolved_start = resolve_path(
        &start_dir.to_string_lossy(),
        &std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy(),
        &PathInputOptions::with_defaults(),
    );
    let git_repo_root = find_git_repo_root(&resolved_start);
    let mut dir = resolved_start;
    loop {
        skill_dirs.push(dir.join(".agents").join("skills"));
        if git_repo_root.as_ref().is_some_and(|root| *root == dir) {
            break;
        }
        let Some(parent) = dir.parent() else {
            break;
        };
        if parent == dir {
            break;
        }
        dir = parent.to_path_buf();
    }
    skill_dirs
}

fn find_git_repo_root(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        let parent = dir.parent()?;
        if parent == dir {
            return None;
        }
        dir = parent.to_path_buf();
    }
}

/// pi: `merge`——先来的赢，同一个真实文件只收一次。
fn merge_skill_results(results: Vec<LoadSkillsResult>) -> LoadSkillsResult {
    let mut skills: Vec<Skill> = Vec::new();
    let mut by_name: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut real_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    for result in results {
        diagnostics.extend(result.diagnostics);
        for skill in result.skills {
            let real_path = get_resource_path_key(Path::new(&skill.file_path));
            if real_paths.contains(&real_path) {
                continue;
            }
            if let Some(winner) = by_name.get(&skill.name) {
                diagnostics.push(ResourceDiagnostic {
                    r#type: "collision".to_string(),
                    message: format!("name \"{}\" collision", skill.name),
                    path: skill.file_path.clone(),
                });
                let _ = winner;
                continue;
            }
            by_name.insert(skill.name.clone(), skill.file_path.clone());
            real_paths.insert(real_path);
            skills.push(skill);
        }
    }
    LoadSkillsResult {
        skills,
        diagnostics,
    }
}

/// pi: `DefaultResourceLoader`（本里程碑的那一半）
pub struct DefaultResourceLoaderOptions {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub no_context_files: bool,
    pub no_skills: bool,
    pub additional_skill_paths: Vec<String>,
}

impl Default for DefaultResourceLoaderOptions {
    fn default() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_default(),
            agent_dir: crate::config::get_agent_dir(),
            no_context_files: false,
            no_skills: false,
            additional_skill_paths: Vec::new(),
        }
    }
}

pub struct DefaultResourceLoader {
    options: DefaultResourceLoaderOptions,
    skills: Vec<Skill>,
    skill_diagnostics: Vec<ResourceDiagnostic>,
    agents_files: Vec<(String, String)>,
    loaded: bool,
}

impl DefaultResourceLoader {
    pub fn new(options: DefaultResourceLoaderOptions) -> Self {
        Self {
            options,
            skills: Vec::new(),
            skill_diagnostics: Vec::new(),
            agents_files: Vec::new(),
            loaded: false,
        }
    }

    /// pi: `reload`（只做技能与规矩文件这一段）
    pub fn reload(&mut self) {
        let cwd = self.options.cwd.clone();
        let agent_dir = self.options.agent_dir.clone();

        let mut results: Vec<LoadSkillsResult> = Vec::new();
        if !self.options.no_skills {
            let config_skills_dir = cwd.join(crate::config::CONFIG_DIR_NAME).join("skills");
            results.push(load_skills_from_dir(&config_skills_dir, "project"));

            let user_agents_skills = dirs::home_dir()
                .unwrap_or_default()
                .join(".agents")
                .join("skills");
            for dir in collect_ancestor_agents_skill_dirs(&cwd) {
                if paths_equal(&dir, &user_agents_skills) {
                    continue;
                }
                results.push(load_skills_from_dir_with_root_files(&dir, "project", false));
            }

            results.push(load_skills_from_dir(&agent_dir.join("skills"), "user"));
            results.push(load_skills_from_dir_with_root_files(
                &user_agents_skills,
                "user",
                false,
            ));

            for raw_path in &self.options.additional_skill_paths {
                let resolved = resolve_path(
                    raw_path,
                    &cwd.to_string_lossy(),
                    &PathInputOptions {
                        trim: true,
                        ..PathInputOptions::with_defaults()
                    },
                );
                if !resolved.exists() {
                    results.push(LoadSkillsResult {
                        skills: Vec::new(),
                        diagnostics: vec![ResourceDiagnostic::warning(
                            "skill path does not exist",
                            resolved.to_string_lossy(),
                        )],
                    });
                    continue;
                }
                if resolved.is_dir() {
                    results.push(load_skills_from_dir(&resolved, "path"));
                } else if resolved.is_file()
                    && resolved
                        .extension()
                        .is_some_and(|extension| extension == "md")
                {
                    results.push(load_skill_file(&resolved, "path"));
                } else {
                    results.push(LoadSkillsResult {
                        skills: Vec::new(),
                        diagnostics: vec![ResourceDiagnostic::warning(
                            "skill path is not a markdown file",
                            resolved.to_string_lossy(),
                        )],
                    });
                }
            }
        }
        let merged = merge_skill_results(results);
        self.skills = merged.skills;
        self.skill_diagnostics = merged.diagnostics;

        self.agents_files = if self.options.no_context_files {
            Vec::new()
        } else {
            load_project_context_files(&cwd, &agent_dir)
        };
        self.loaded = true;
    }

    pub fn get_skills(&self) -> &[Skill] {
        &self.skills
    }

    pub fn get_skill_diagnostics(&self) -> &[ResourceDiagnostic] {
        &self.skill_diagnostics
    }

    pub fn get_agents_files(&self) -> &[(String, String)] {
        &self.agents_files
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }
}
