//! 对应上游 `test/paths.test.ts`：路径比较、`~` 展开、cwd 相对路径与本地路径判断。
//!
//! Windows 专属的用例（大小写折叠、`normalizeWindowsShellPath`、Windows file URL）没搬，
//! 见 `docs/port/milestone-4.md`；`file:` URL 的非法转义在上游会抛错，这里容错，也没有搬那条。

use std::fs;
use std::path::Path;

use tempfile::TempDir;
use xjpcode_coding_agent::utils::paths::{
    canonicalize_path, get_cwd_relative_path, is_local_path, is_path_within, normalize_path,
    paths_equal, resolve_path, shorten_home_path_with, PathInputOptions,
};

fn create_temp_dir() -> TempDir {
    TempDir::new().unwrap()
}

#[test]
fn is_path_within_matches_whole_path_segments() {
    assert!(is_path_within(Path::new("/home/me"), Path::new("/home/me")));
    assert!(is_path_within(
        Path::new("/home/me"),
        Path::new("/home/me/project")
    ));
    assert!(is_path_within(
        Path::new("/home/me/"),
        Path::new("/home/me/project")
    ));
    assert!(!is_path_within(
        Path::new("/home/me"),
        Path::new("/home/me2")
    ));
    assert!(is_path_within(Path::new("/"), Path::new("/home")));
    assert!(!paths_equal(Path::new("/home/me"), Path::new("/home/Me")));
}

#[test]
fn shorten_home_path_replaces_only_a_leading_home_directory() {
    let home = Path::new("/home/me");
    assert_eq!(
        shorten_home_path_with(Path::new("/home/me"), Some(home)),
        "~"
    );
    assert_eq!(
        shorten_home_path_with(Path::new("/home/me/project"), Some(home)),
        "~/project"
    );
    assert_eq!(
        shorten_home_path_with(Path::new("/home/me2/project"), Some(home)),
        "/home/me2/project"
    );
    assert_eq!(
        shorten_home_path_with(Path::new("/home/Me/project"), Some(home)),
        "/home/Me/project"
    );
    assert_eq!(
        shorten_home_path_with(Path::new("/etc/passwd"), Some(Path::new("/"))),
        "~/etc/passwd"
    );
}

#[test]
fn canonicalize_returns_the_real_path_for_regular_files() {
    let dir = create_temp_dir();
    let file = dir.path().join("file.txt");
    fs::write(&file, "hello").unwrap();
    assert_eq!(canonicalize_path(&file), fs::canonicalize(&file).unwrap());
}

#[test]
fn canonicalize_resolves_symlinks_to_their_targets() {
    let dir = create_temp_dir();
    let target = dir.path().join("target.txt");
    let link = dir.path().join("link.txt");
    fs::write(&target, "hello").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert_eq!(canonicalize_path(&link), fs::canonicalize(&target).unwrap());
}

#[test]
fn canonicalize_falls_back_to_the_raw_path_when_the_target_does_not_exist() {
    let dir = create_temp_dir();
    let nonexistent = dir.path().join("no-such-file");
    assert_eq!(canonicalize_path(&nonexistent), nonexistent);
}

#[cfg(unix)]
#[test]
fn canonicalize_falls_back_to_the_raw_path_for_a_dangling_symlink() {
    let dir = create_temp_dir();
    let target = dir.path().join("target.txt");
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert_eq!(canonicalize_path(&link), link);
}

#[test]
fn keeps_cwd_relative_names_that_start_with_dots() {
    let cwd = Path::new("/tmp/pi-paths-cwd");
    assert_eq!(
        get_cwd_relative_path(
            "/tmp/pi-paths-cwd/..config/AGENTS.md",
            &cwd.to_string_lossy()
        ),
        Some("..config/AGENTS.md".to_string())
    );
}

#[test]
fn rejects_parent_directory_traversals() {
    let cwd = Path::new("/tmp/pi-paths-cwd");
    assert_eq!(
        get_cwd_relative_path("/tmp/pi-paths-cwd/../AGENTS.md", &cwd.to_string_lossy()),
        None
    );
}

#[test]
fn expands_only_home_tilde_shortcuts() {
    let cwd = "/tmp/pi-paths-cwd";
    let home = dirs::home_dir().unwrap();
    assert_eq!(
        normalize_path("~", &PathInputOptions::with_defaults()),
        home.to_string_lossy()
    );
    assert_eq!(
        normalize_path("~/file.txt", &PathInputOptions::with_defaults()),
        home.join("file.txt").to_string_lossy()
    );
    assert_eq!(
        resolve_path("~draft.md", cwd, &PathInputOptions::with_defaults()),
        Path::new(cwd).join("~draft.md")
    );
    assert_eq!(
        normalize_path("~draft.md", &PathInputOptions::with_defaults()),
        "~draft.md"
    );
}

#[test]
fn resolves_relative_paths_against_the_base_directory() {
    let cwd = "/tmp/pi-paths-cwd";
    assert_eq!(
        resolve_path("subdir/file.txt", cwd, &PathInputOptions::with_defaults()),
        Path::new(cwd).join("subdir/file.txt")
    );
}

#[test]
fn accepts_file_urls() {
    let dir = create_temp_dir();
    let file = dir.path().join("file with spaces.txt");
    let url = format!("file://{}", file.display());
    assert_eq!(
        resolve_path(
            &url,
            &dir.path().join("base").to_string_lossy(),
            &PathInputOptions::with_defaults()
        ),
        file
    );
}

#[test]
fn preserves_posix_absolute_paths_with_literal_percent_sequences() {
    let dir = create_temp_dir();
    for file in [
        dir.path().join("report%2026.md"),
        dir.path().join("foo%2Fbar"),
        dir.path().join("malformed%A.md"),
    ] {
        assert_eq!(
            resolve_path(
                &file.to_string_lossy(),
                &dir.path().join("base").to_string_lossy(),
                &PathInputOptions::with_defaults()
            ),
            file
        );
    }
}

#[test]
fn detects_local_paths() {
    assert!(is_local_path("my-package"));
    assert!(is_local_path("./foo"));
    assert!(is_local_path("file:///tmp/foo"));
    assert!(!is_local_path("npm:package"));
    assert!(!is_local_path("git://repo"));
    assert!(!is_local_path("https://example.com"));
}
