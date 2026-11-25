//! 对应上游 `src/utils/paths.ts` 中用到的部分：路径规整、`~` 展开与本仓的路径比较规则。
//!
//! 上游的 Windows 分支（Git Bash / MSYS 路径转换、大小写折叠）没有搬：这次只在 Unix 上跑，
//! 见 `docs/port/milestone-3.md` 差异表。

use std::path::{Component, Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

/// pi: `PathInputOptions`
#[derive(Debug, Clone, Default)]
pub struct PathInputOptions {
    /// 先去掉首尾空白。
    pub trim: bool,
    /// 展开开头的 `~`（上游默认 `true`）。
    pub expand_tilde: bool,
    /// 展开 `~` 用的家目录；不给就用 `os.homedir()`。
    pub home_dir: Option<String>,
    /// 去掉开头的 `@`（命令行 `@file` 写法）。
    pub strip_at_prefix: bool,
    /// 把各种 Unicode 空格换成普通空格。
    pub normalize_unicode_spaces: bool,
}

impl PathInputOptions {
    /// 上游 `expandTilde` 缺省为 `true`：用这个构造器拿到缺省值。
    pub fn with_defaults() -> Self {
        Self {
            trim: false,
            expand_tilde: true,
            home_dir: None,
            strip_at_prefix: false,
            normalize_unicode_spaces: false,
        }
    }
}

fn is_unicode_space(character: char) -> bool {
    matches!(
        character,
        '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// pi: `normalizePath`
pub fn normalize_path(input: &str, options: &PathInputOptions) -> String {
    let mut normalized = if options.trim {
        input.trim().to_string()
    } else {
        input.to_string()
    };
    if options.normalize_unicode_spaces {
        normalized = normalized
            .chars()
            .map(|character| {
                if is_unicode_space(character) {
                    ' '
                } else {
                    character
                }
            })
            .collect();
    }
    if options.strip_at_prefix {
        if let Some(rest) = normalized.strip_prefix('@') {
            normalized = rest.to_string();
        }
    }
    if options.expand_tilde {
        let home = options
            .home_dir
            .clone()
            .or_else(|| dirs::home_dir().map(|home| home.to_string_lossy().into_owned()))
            .unwrap_or_default();
        if normalized == "~" {
            return home;
        }
        if let Some(rest) = normalized.strip_prefix("~/") {
            return join_path(&home, rest);
        }
    }
    if let Some(rest) = normalized.strip_prefix("file://") {
        // 上游用 `fileURLToPath`；这里只处理不带百分号转义的常见写法。
        return percent_decode(rest);
    }
    normalized
}

/// 与 `node:path.join` 同义的拼接：规整 `.`/`..` 与重复的分隔符。
fn join_path(base: &str, rest: &str) -> String {
    lexical_normalize(&Path::new(base).join(rest))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                output.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// pi: `getCwdRelativePath`——在 cwd 里面就给相对路径（`.` 表示自身），否则 `None`。
pub fn get_cwd_relative_path(file_path: &str, cwd: &str) -> Option<String> {
    let resolved_cwd = resolve_path(
        cwd,
        &std::env::current_dir()
            .unwrap_or_default()
            .to_string_lossy(),
        &PathInputOptions::with_defaults(),
    );
    let resolved_path = resolve_path(
        file_path,
        &resolved_cwd.to_string_lossy(),
        &PathInputOptions::with_defaults(),
    );
    if !is_path_within(&resolved_cwd, &resolved_path) {
        return None;
    }
    let relative = resolved_path
        .strip_prefix(&resolved_cwd)
        .unwrap_or(Path::new(""));
    if relative.as_os_str().is_empty() {
        Some(".".to_string())
    } else {
        Some(relative.to_string_lossy().into_owned())
    }
}

/// pi: `isLocalPath`——`npm:`、`git:`、`http(s):`、`ssh:`、`builtin:` 不是本地路径。
pub fn is_local_path(value: &str) -> bool {
    let trimmed = value.trim();
    ![
        "npm:", "git:", "github:", "http:", "https:", "ssh:", "builtin:",
    ]
    .iter()
    .any(|prefix| trimmed.starts_with(prefix))
}

/// `node:path.resolve` 同样的规整：消掉 `.`、`..`、重复分隔符。
fn lexical_normalize(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut absolute = false;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                parts.push(prefix.as_os_str().to_string_lossy().into_owned())
            }
            Component::RootDir => absolute = true,
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(parts.last().map(String::as_str), Some(part) if part != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..".to_string());
                }
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
        }
    }
    let joined = parts.join(std::path::MAIN_SEPARATOR_STR);
    if absolute {
        format!("{}{joined}", std::path::MAIN_SEPARATOR)
    } else {
        joined
    }
}

/// pi: `resolvePath`
pub fn resolve_path(input: &str, base_dir: &str, options: &PathInputOptions) -> PathBuf {
    let normalized = normalize_path(input, options);
    let normalized_base = normalize_path(base_dir, &PathInputOptions::with_defaults());
    let path = Path::new(&normalized);
    if path.is_absolute() {
        PathBuf::from(lexical_normalize(path))
    } else {
        PathBuf::from(lexical_normalize(&Path::new(&normalized_base).join(path)))
    }
}

/// pi: `canonicalizePath`——解析失败时原样返回。
pub fn canonicalize_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// pi: `getPathComparisonKey`（Unix：规整后的绝对路径）。
pub fn get_path_comparison_key(path: &Path) -> String {
    lexical_normalize(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
}

/// pi: `pathsEqual`
pub fn paths_equal(left: &Path, right: &Path) -> bool {
    get_path_comparison_key(left) == get_path_comparison_key(right)
}

/// pi: `isPathWithin`
pub fn is_path_within(parent: &Path, child: &Path) -> bool {
    let parent_key = get_path_comparison_key(parent);
    let child_key = get_path_comparison_key(child);
    if child_key == parent_key {
        return true;
    }
    let separator = std::path::MAIN_SEPARATOR;
    let prefix = if parent_key.ends_with(separator) {
        parent_key
    } else {
        format!("{parent_key}{separator}")
    };
    child_key.starts_with(&prefix)
}

/// pi: `shortenHomePath`——家目录换成 `~`。
pub fn shorten_home_path(path: &Path) -> String {
    shorten_home_path_with(path, dirs::home_dir().as_deref())
}

/// pi: `shortenHomePath(path, homeDir)`——测试可指定家目录。
pub fn shorten_home_path_with(path: &Path, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return path.to_string_lossy().into_owned();
    };
    if !path.is_absolute() || !is_path_within(home, path) {
        return path.to_string_lossy().into_owned();
    }
    let relative = path.strip_prefix(home).unwrap_or(Path::new(""));
    if relative.as_os_str().is_empty() {
        "~".to_string()
    } else {
        format!(
            "~{}{}",
            std::path::MAIN_SEPARATOR,
            relative.to_string_lossy()
        )
    }
}

/// 上游 `pathExists`
pub async fn path_exists(path: &Path) -> bool {
    tokio::fs::metadata(path).await.is_ok()
}

/// NFD（macOS 文件名常见的分解形式）。
pub fn nfd(text: &str) -> String {
    text.nfd().collect()
}
