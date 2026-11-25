//! 对应上游 `test/resource-loader.test.ts` 里本里程碑的两块：技能发现与规矩文件发现
//! （含 worktree 去重那一组）。
//!
//! 扩展、提示词模板、主题、包安装、项目信任不在这一层，对应的用例没有搬。

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use xjpcode_coding_agent::core::resource_loader::{
    load_project_context_files, DefaultResourceLoader, DefaultResourceLoaderOptions,
};

fn write(path: impl AsRef<Path>, content: &str) {
    if let Some(parent) = path.as_ref().parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn skill_md(name: &str, description: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\n\nSkill content.\n")
}

fn context_contents(files: &[(String, String)]) -> Vec<String> {
    files.iter().map(|(_, content)| content.clone()).collect()
}

#[test]
fn discovers_skills_from_agent_dir() {
    let agent = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    write(
        agent.path().join("skills/test-skill.md"),
        &skill_md("test-skill", "A test skill"),
    );

    let mut loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: cwd.path().to_path_buf(),
        agent_dir: agent.path().to_path_buf(),
        ..DefaultResourceLoaderOptions::default()
    });
    loader.reload();
    assert!(loader
        .get_skills()
        .iter()
        .any(|skill| skill.name == "test-skill"));
}

#[test]
fn ignores_extra_markdown_files_in_auto_discovered_skill_dirs() {
    let agent = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let skill_dir = agent.path().join("skills/pi-skills/browser-tools");
    write(
        skill_dir.join("SKILL.md"),
        &skill_md("browser-tools", "Browser tools"),
    );
    write(skill_dir.join("EFFICIENCY.md"), "No frontmatter here");

    let mut loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: cwd.path().to_path_buf(),
        agent_dir: agent.path().to_path_buf(),
        ..DefaultResourceLoaderOptions::default()
    });
    loader.reload();
    assert!(loader
        .get_skills()
        .iter()
        .any(|skill| skill.name == "browser-tools"));
    assert!(!loader
        .get_skill_diagnostics()
        .iter()
        .any(|diagnostic| diagnostic.path.ends_with("EFFICIENCY.md")));
}

#[test]
fn discovers_agents_md_context_files() {
    let agent = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    write(
        cwd.path().join("AGENTS.md"),
        "# Project Guidelines\n\nBe helpful.",
    );

    let files = load_project_context_files(cwd.path(), agent.path());
    assert!(files.iter().any(|(path, _)| path.ends_with("AGENTS.md")));
}

#[test]
fn prefers_agents_override_md_within_each_directory_while_preserving_ancestor_layering() {
    let agent = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let nested = cwd.path().join("service");
    fs::create_dir_all(&nested).unwrap();
    write(agent.path().join("AGENTS.md"), "global instructions");
    write(agent.path().join("AGENTS.override.md"), "global override");
    write(cwd.path().join("AGENTS.md"), "project instructions");
    write(nested.join("AGENTS.md"), "service instructions");
    write(nested.join("AGENTS.override.md"), "service override");

    let files = load_project_context_files(&nested, agent.path());
    assert_eq!(
        files,
        vec![
            (
                agent
                    .path()
                    .join("AGENTS.override.md")
                    .to_string_lossy()
                    .into_owned(),
                "global override".to_string()
            ),
            (
                cwd.path().join("AGENTS.md").to_string_lossy().into_owned(),
                "project instructions".to_string()
            ),
            (
                nested
                    .join("AGENTS.override.md")
                    .to_string_lossy()
                    .into_owned(),
                "service override".to_string()
            ),
        ]
    );
}

#[test]
fn ignores_context_file_candidates_that_are_directories() {
    let agent = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    fs::create_dir_all(cwd.path().join("AGENTS.override.md")).unwrap();
    fs::create_dir_all(cwd.path().join("AGENTS.md")).unwrap();
    write(cwd.path().join("CLAUDE.md"), "Fallback instructions");

    let files = load_project_context_files(cwd.path(), agent.path());
    assert!(files
        .iter()
        .any(|(_, content)| content == "Fallback instructions"));
}

#[test]
fn skips_context_file_discovery_when_no_context_files_is_true() {
    let agent = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    write(cwd.path().join("AGENTS.md"), "# Project Guidelines\n");

    let mut loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: cwd.path().to_path_buf(),
        agent_dir: agent.path().to_path_buf(),
        no_context_files: true,
        ..DefaultResourceLoaderOptions::default()
    });
    loader.reload();
    assert!(loader.get_agents_files().is_empty());
}

#[test]
fn skips_skill_discovery_when_no_skills_is_true() {
    let agent = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    write(
        agent.path().join("skills/test-skill.md"),
        &skill_md("test-skill", "A test skill"),
    );

    let mut loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: cwd.path().to_path_buf(),
        agent_dir: agent.path().to_path_buf(),
        no_skills: true,
        ..DefaultResourceLoaderOptions::default()
    });
    loader.reload();
    assert!(loader.get_skills().is_empty());
}

// ===== loadProjectContextFiles - nested worktree dedup =====

fn link_worktree(main_dir: &Path, worktree_dir: &Path, name: &str) {
    let git_dir = main_dir.join(".git").join("worktrees").join(name);
    fs::create_dir_all(&git_dir).unwrap();
    write(main_dir.join(".git/HEAD"), "ref: refs/heads/main\n");
    write(git_dir.join("HEAD"), "ref: refs/heads/feat\n");
    write(git_dir.join("commondir"), "../..");
    write(
        worktree_dir.join(".git"),
        &format!("gitdir: {}\n", git_dir.display()),
    );
}

struct NestedWorktree {
    outer: PathBuf,
    main: PathBuf,
    worktree: PathBuf,
    worktree_src: PathBuf,
}

fn setup_nested_worktree(temp: &TempDir) -> NestedWorktree {
    let outer = temp.path().join("outer");
    let main = outer.join("main");
    let worktree = main.join("worktrees").join("feat");
    let worktree_src = worktree.join("src");
    fs::create_dir_all(&worktree_src).unwrap();
    link_worktree(&main, &worktree, "feat");
    NestedWorktree {
        outer,
        main,
        worktree,
        worktree_src,
    }
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[test]
fn skips_the_main_repos_duplicate_when_the_worktree_root_has_its_own_context() {
    let temp = TempDir::new().unwrap();
    let agent = TempDir::new().unwrap();
    let layout = setup_nested_worktree(&temp);
    write(layout.main.join("AGENTS.md"), "main repo instructions");
    write(layout.worktree.join("AGENTS.md"), "worktree instructions");

    let files = load_project_context_files(&layout.worktree_src, agent.path());
    assert_eq!(context_contents(&files), vec!["worktree instructions"]);
}

#[cfg(unix)]
#[test]
fn loads_the_worktrees_context_once_when_it_symlinks_to_the_main_repos_file() {
    let temp = TempDir::new().unwrap();
    let agent = TempDir::new().unwrap();
    let layout = setup_nested_worktree(&temp);
    write(layout.main.join("AGENTS.md"), "main repo instructions");
    symlink(
        &layout.main.join("AGENTS.md"),
        &layout.worktree.join("AGENTS.md"),
    );

    let files = load_project_context_files(&layout.worktree_src, agent.path());
    assert_eq!(files.len(), 1);
    assert_eq!(
        files[0].0,
        layout.worktree.join("AGENTS.md").to_string_lossy()
    );
}

#[cfg(unix)]
#[test]
fn skips_the_main_repos_duplicate_when_the_main_repos_file_is_a_symlink() {
    let temp = TempDir::new().unwrap();
    let agent = TempDir::new().unwrap();
    let layout = setup_nested_worktree(&temp);
    let shared = layout.outer.join("shared-agents.md");
    write(&shared, "shared instructions");
    symlink(&shared, &layout.main.join("AGENTS.md"));
    symlink(&shared, &layout.worktree.join("AGENTS.md"));

    let files = load_project_context_files(&layout.worktree_src, agent.path());
    assert_eq!(files.len(), 1);
    assert_eq!(
        files[0].0,
        layout.worktree.join("AGENTS.md").to_string_lossy()
    );
}

#[test]
fn still_inherits_the_main_repos_context_when_the_worktree_root_has_none() {
    let temp = TempDir::new().unwrap();
    let agent = TempDir::new().unwrap();
    let layout = setup_nested_worktree(&temp);
    write(layout.main.join("AGENTS.md"), "main repo instructions");

    let files = load_project_context_files(&layout.worktree_src, agent.path());
    assert_eq!(context_contents(&files), vec!["main repo instructions"]);
}

#[test]
fn only_skips_the_same_filename_not_a_differently_named_context_file() {
    let temp = TempDir::new().unwrap();
    let agent = TempDir::new().unwrap();
    let layout = setup_nested_worktree(&temp);
    write(layout.main.join("CLAUDE.md"), "main repo instructions");
    write(layout.worktree.join("AGENTS.md"), "worktree instructions");

    let files = load_project_context_files(&layout.worktree_src, agent.path());
    assert_eq!(
        context_contents(&files),
        vec!["main repo instructions", "worktree instructions"]
    );
}

#[test]
fn does_not_skip_the_containers_context_in_a_bare_layout() {
    let temp = TempDir::new().unwrap();
    let agent = TempDir::new().unwrap();
    let proj = temp.path().join("proj");
    let bare = proj.join(".bare");
    let worktree = proj.join("main");
    let worktree_git_dir = bare.join("worktrees").join("main");
    fs::create_dir_all(&worktree_git_dir).unwrap();
    fs::create_dir_all(&worktree).unwrap();
    write(bare.join("HEAD"), "ref: refs/heads/main\n");
    write(worktree_git_dir.join("HEAD"), "ref: refs/heads/main\n");
    write(worktree_git_dir.join("commondir"), "../..");
    write(
        worktree.join(".git"),
        &format!("gitdir: {}\n", worktree_git_dir.display()),
    );
    write(proj.join("AGENTS.md"), "container instructions");
    write(worktree.join("AGENTS.md"), "worktree instructions");

    let files = load_project_context_files(&worktree, agent.path());
    assert_eq!(
        context_contents(&files),
        vec!["container instructions", "worktree instructions"]
    );
}

#[test]
fn keeps_loading_ancestors_above_the_main_repo() {
    let temp = TempDir::new().unwrap();
    let agent = TempDir::new().unwrap();
    let layout = setup_nested_worktree(&temp);
    write(layout.outer.join("AGENTS.md"), "outer instructions");
    write(layout.main.join("AGENTS.md"), "main repo instructions");
    write(layout.worktree.join("AGENTS.md"), "worktree instructions");

    let files = load_project_context_files(&layout.worktree_src, agent.path());
    assert_eq!(
        context_contents(&files),
        vec!["outer instructions", "worktree instructions"]
    );
}

#[test]
fn does_not_skip_anything_for_a_sibling_worktree() {
    let temp = TempDir::new().unwrap();
    let agent = TempDir::new().unwrap();
    let outer = temp.path().join("outer");
    let main = outer.join("main");
    let sibling = outer.join("sib-feat");
    let sibling_src = sibling.join("src");
    fs::create_dir_all(&sibling_src).unwrap();
    fs::create_dir_all(&main).unwrap();
    write(outer.join("AGENTS.md"), "outer instructions");
    write(sibling.join("AGENTS.md"), "sibling worktree instructions");
    link_worktree(&main, &sibling, "sib");

    let files = load_project_context_files(&sibling_src, agent.path());
    assert_eq!(
        context_contents(&files),
        vec!["outer instructions", "sibling worktree instructions"]
    );
}

#[test]
fn does_not_skip_the_superprojects_context_from_inside_a_submodule() {
    let temp = TempDir::new().unwrap();
    let agent = TempDir::new().unwrap();
    let sup = temp.path().join("super");
    let sub = sup.join("vendor").join("lib");
    let sub_src = sub.join("src");
    fs::create_dir_all(&sub_src).unwrap();
    write(sup.join("AGENTS.md"), "superproject instructions");
    write(sub.join("AGENTS.md"), "submodule instructions");
    let sub_git_dir = sup.join(".git").join("modules").join("vendor").join("lib");
    fs::create_dir_all(&sub_git_dir).unwrap();
    write(sub_git_dir.join("HEAD"), "ref: refs/heads/main\n");
    write(
        sub.join(".git"),
        &format!("gitdir: {}\n", sub_git_dir.display()),
    );

    let files = load_project_context_files(&sub_src, agent.path());
    assert_eq!(
        context_contents(&files),
        vec!["superproject instructions", "submodule instructions"]
    );
}

#[test]
fn keeps_climbing_past_an_ordinary_repo_root() {
    let temp = TempDir::new().unwrap();
    let agent = TempDir::new().unwrap();
    let outer = temp.path().join("outer");
    let repo = outer.join("repo");
    let leaf = repo.join("src");
    fs::create_dir_all(&leaf).unwrap();
    fs::create_dir_all(repo.join(".git")).unwrap();
    write(repo.join(".git/HEAD"), "ref: refs/heads/main\n");
    write(outer.join("AGENTS.md"), "outer instructions");
    write(repo.join("AGENTS.md"), "repo instructions");
    write(leaf.join("AGENTS.md"), "leaf instructions");

    let files = load_project_context_files(&leaf, agent.path());
    assert_eq!(
        context_contents(&files),
        vec![
            "outer instructions",
            "repo instructions",
            "leaf instructions"
        ]
    );
}

#[test]
fn climbs_normally_when_the_gitdir_target_does_not_exist() {
    let temp = TempDir::new().unwrap();
    let agent = TempDir::new().unwrap();
    let repo = temp.path().join("corrupt");
    let src = repo.join("src");
    fs::create_dir_all(&src).unwrap();
    write(
        repo.join(".git"),
        "gitdir: /nonexistent/path/worktrees/feat\n",
    );
    write(repo.join("AGENTS.md"), "repo instructions");
    write(src.join("AGENTS.md"), "src instructions");

    let files = load_project_context_files(&src, agent.path());
    assert_eq!(
        context_contents(&files),
        vec!["repo instructions", "src instructions"]
    );
}
