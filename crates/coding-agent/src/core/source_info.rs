//! 对应上游 `src/core/source-info.ts` 中本里程碑用到的部分：`SourceInfo` 与去重用的路径键。
//!
//! 上游的 `SourceInfo` 还带 `changelogPath`（只有包资源用）；技能、规矩文件这一层不读它，先不搬。

use std::path::Path;

use crate::utils::paths::{canonicalize_path, get_path_comparison_key};

/// pi: `SourceScope`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceScope {
    User,
    Project,
    Temporary,
}

/// pi: `SourceOrigin`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceOrigin {
    Package,
    TopLevel,
}

/// pi: `SourceInfo`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    pub path: String,
    pub source: String,
    pub scope: SourceScope,
    pub origin: SourceOrigin,
    pub base_dir: Option<String>,
}

impl SourceInfo {
    /// pi: `createSyntheticSourceInfo`
    pub fn synthetic(
        file_path: &str,
        source: &str,
        scope: Option<SourceScope>,
        base_dir: Option<&str>,
    ) -> Self {
        Self {
            path: file_path.to_string(),
            source: source.to_string(),
            scope: scope.unwrap_or(SourceScope::Temporary),
            origin: SourceOrigin::TopLevel,
            base_dir: base_dir.map(ToString::to_string),
        }
    }
}

/// pi: `isSyntheticPath`——`builtin:` 或尖括号包起来的路径不是文件。
pub fn is_synthetic_path(path: &str) -> bool {
    path.starts_with("builtin:") || path.starts_with('<')
}

/// pi: `getResourcePathKey`——合成路径原样；文件路径取 realpath 再比较。
pub fn get_resource_path_key(path: &Path) -> String {
    let text = path.to_string_lossy();
    if is_synthetic_path(&text) {
        return text.into_owned();
    }
    get_path_comparison_key(&canonicalize_path(path))
}
