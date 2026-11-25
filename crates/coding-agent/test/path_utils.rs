//! 对应上游 `test/path-utils.test.ts`：`~` 展开、相对路径与 macOS 文件名的几种变体。

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use xjpcode_coding_agent::core::tools::path_utils::{
    expand_path, resolve_read_path_async, resolve_to_cwd,
};

fn write_file(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).unwrap();
}

#[test]
fn expands_tilde_to_home_directory() {
    let result = expand_path("~");
    assert!(!result.contains('~'));
}

#[test]
fn expands_tilde_slash_path_to_home_directory() {
    let result = expand_path("~/Documents/file.txt");
    assert!(!result.contains("~/"));
}

#[test]
fn keeps_tilde_prefixed_filenames_literal() {
    assert_eq!(expand_path("~draft.md"), "~draft.md");
    assert_eq!(expand_path("@~draft.md"), "~draft.md");
}

#[test]
fn normalizes_unicode_spaces() {
    assert_eq!(expand_path("file\u{00A0}name.txt"), "file name.txt");
}

#[test]
fn resolves_absolute_paths_as_is() {
    let absolute_path = Path::new("/tmp").join("absolute").join("path").join("file.txt");
    let result = resolve_to_cwd(&absolute_path.to_string_lossy(), "/tmp/some/cwd");
    assert_eq!(result, absolute_path);
}

#[test]
fn resolves_relative_paths_against_cwd() {
    let result = resolve_to_cwd("relative/file.txt", "/some/cwd");
    assert_eq!(result, PathBuf::from("/some/cwd/relative/file.txt"));
}

#[test]
fn resolves_tilde_prefixed_filenames_against_cwd() {
    let cwd = PathBuf::from("/tmp/pi-path-utils-cwd");
    assert_eq!(
        resolve_to_cwd("~draft.md", &cwd.to_string_lossy()),
        cwd.join("~draft.md")
    );
    assert_eq!(
        resolve_to_cwd("@~draft.md", &cwd.to_string_lossy()),
        cwd.join("~draft.md")
    );
}

#[tokio::test]
async fn resolves_existing_file_path() {
    let dir = TempDir::new().unwrap();
    write_file(dir.path(), "test-file.txt", "content");
    let result = resolve_read_path_async("test-file.txt", &dir.path().to_string_lossy()).await;
    assert_eq!(result, dir.path().join("test-file.txt"));
}

#[tokio::test]
async fn handles_nfc_vs_nfd_unicode_normalization() {
    let dir = TempDir::new().unwrap();
    // macOS 常把文件名存成 NFD：e + 组合重音。
    let nfd_file_name = "filee\u{0301}.txt";
    let nfc_file_name = "file\u{00E9}.txt";
    assert_ne!(nfd_file_name, nfc_file_name);
    write_file(dir.path(), nfd_file_name, "content");
    let result = resolve_read_path_async(nfc_file_name, &dir.path().to_string_lossy()).await;
    assert!(result.starts_with(dir.path()));
    assert!(result.to_string_lossy().contains("file"));
}

#[tokio::test]
async fn handles_curly_quotes_vs_straight_quotes() {
    let dir = TempDir::new().unwrap();
    let curly = "Capture d\u{2019}cran.txt";
    let straight = "Capture d'cran.txt";
    write_file(dir.path(), curly, "content");
    let result = resolve_read_path_async(straight, &dir.path().to_string_lossy()).await;
    assert_eq!(result, dir.path().join(curly));
}

#[tokio::test]
async fn handles_combined_nfc_and_curly_quote() {
    let dir = TempDir::new().unwrap();
    let nfc_curly = "Capture d\u{2019}\u{00E9}cran.txt";
    let nfc_straight = "Capture d'\u{00E9}cran.txt";
    write_file(dir.path(), nfc_curly, "content");
    let result = resolve_read_path_async(nfc_straight, &dir.path().to_string_lossy()).await;
    assert_eq!(result, dir.path().join(nfc_curly));
}

#[tokio::test]
async fn handles_macos_screenshot_am_pm_with_narrow_no_break_space() {
    let dir = TempDir::new().unwrap();
    let macos_name = "Screenshot 2024-01-01 at 10.00.00\u{202F}AM.png";
    let user_name = "Screenshot 2024-01-01 at 10.00.00 AM.png";
    write_file(dir.path(), macos_name, "content");
    let result = resolve_read_path_async(user_name, &dir.path().to_string_lossy()).await;
    assert_eq!(result, dir.path().join(macos_name));
}

#[tokio::test]
async fn handles_macos_screenshot_lowercase_am_pm() {
    let dir = TempDir::new().unwrap();
    let macos_name = "Screenshot 2024-01-01 at 10.00.00\u{202F}am.png";
    let user_name = "Screenshot 2024-01-01 at 10.00.00 am.png";
    write_file(dir.path(), macos_name, "content");
    let result = resolve_read_path_async(user_name, &dir.path().to_string_lossy()).await;
    assert_eq!(result, dir.path().join(macos_name));
}
