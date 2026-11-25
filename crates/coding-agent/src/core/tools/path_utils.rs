//! 对应上游 `src/core/tools/path-utils.ts`：把工具参数里的路径解析成实际路径。
//!
//! macOS 截图文件名的几种变体（AM/PM 前的窄空格、NFD、弯引号）照上游依次尝试；这里是异步版本，
//! 与 `resolveReadPathAsync` 对应。

use std::path::{Path, PathBuf};

use crate::utils::paths::{
    nfd, path_exists, resolve_path, PathInputOptions,
};

/// pi: `expandPath`
pub fn expand_path(file_path: &str) -> String {
    crate::utils::paths::normalize_path(
        file_path,
        &PathInputOptions {
            trim: false,
            expand_tilde: true,
            home_dir: None,
            strip_at_prefix: true,
            normalize_unicode_spaces: true,
        },
    )
}

/// pi: `resolveToCwd`
pub fn resolve_to_cwd(file_path: &str, cwd: &str) -> PathBuf {
    resolve_path(
        file_path,
        cwd,
        &PathInputOptions {
            trim: false,
            expand_tilde: true,
            home_dir: None,
            strip_at_prefix: true,
            normalize_unicode_spaces: true,
        },
    )
}

fn try_macos_screenshot_path(file_path: &str) -> String {
    let characters: Vec<char> = file_path.chars().collect();
    let mut output = String::with_capacity(file_path.len());
    let mut index = 0;
    while index < characters.len() {
        let rest: String = characters[index..].iter().collect();
        let lower = rest.to_lowercase();
        if lower.starts_with(" am.") || lower.starts_with(" pm.") {
            output.push('\u{202F}');
            output.push(characters[index + 1]);
            output.push(characters[index + 2]);
            output.push('.');
            index += 4;
            continue;
        }
        output.push(characters[index]);
        index += 1;
    }
    output
}

fn try_curly_quote_variant(file_path: &str) -> String {
    file_path.replace('\'', "\u{2019}")
}

/// pi: `resolveReadPathAsync`
pub async fn resolve_read_path_async(file_path: &str, cwd: &str) -> PathBuf {
    let resolved = resolve_to_cwd(file_path, cwd);
    if path_exists(&resolved).await {
        return resolved;
    }

    let as_text = resolved.to_string_lossy().into_owned();
    let am_pm = try_macos_screenshot_path(&as_text);
    if am_pm != as_text && path_exists(Path::new(&am_pm)).await {
        return PathBuf::from(am_pm);
    }

    let nfd_variant = nfd(&as_text);
    if nfd_variant != as_text && path_exists(Path::new(&nfd_variant)).await {
        return PathBuf::from(nfd_variant);
    }

    let curly = try_curly_quote_variant(&as_text);
    if curly != as_text && path_exists(Path::new(&curly)).await {
        return PathBuf::from(curly);
    }

    let nfd_curly = try_curly_quote_variant(&nfd_variant);
    if nfd_curly != as_text && path_exists(Path::new(&nfd_curly)).await {
        return PathBuf::from(nfd_curly);
    }

    resolved
}
