//! 对应上游 `packages/coding-agent/src/config.ts` 的一小部分：本里程碑只用到目录与文档路径。
//!
//! 上游从安装包的位置找 `README.md`、`docs/`、`examples/`；xjpcode 还没有安装包，默认指向仓库根，
//! 也可以用环境变量 `PI_PACKAGE_DIR` 指到别处（与上游同名、同义）。用户目录用 xjpcode 自己的
//! `~/.xjpcode/agent`（上游是 `~/.pi/agent`），见 `docs/port/milestone-3.md` 差异表。

use std::path::PathBuf;

/// pi: `CONFIG_DIR_NAME`
pub const CONFIG_DIR_NAME: &str = ".xjpcode";
/// pi: `ENV_AGENT_DIR`
pub const ENV_AGENT_DIR: &str = "XJPCODE_AGENT_DIR";

/// pi: `getPackageDir()`
pub fn get_package_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("PI_PACKAGE_DIR") {
        return PathBuf::from(dir);
    }
    // 仓库根：`crates/coding-agent` 往上两级。发布以后换成可执行文件旁边。
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

/// pi: `getReadmePath()`
pub fn get_readme_path() -> String {
    get_package_dir().join("README.md").to_string_lossy().into_owned()
}

/// pi: `getDocsPath()`
pub fn get_docs_path() -> String {
    get_package_dir().join("docs").to_string_lossy().into_owned()
}

/// pi: `getExamplesPath()`
pub fn get_examples_path() -> String {
    get_package_dir()
        .join("examples")
        .to_string_lossy()
        .into_owned()
}

/// pi: `getAgentDir()`
pub fn get_agent_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(ENV_AGENT_DIR) {
        return expand_tilde(&dir.to_string_lossy());
    }
    dirs::home_dir()
        .unwrap_or_default()
        .join(CONFIG_DIR_NAME)
        .join("agent")
}

/// pi: `getBinDir()`
pub fn get_bin_dir() -> PathBuf {
    get_agent_dir().join("bin")
}

fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return dirs::home_dir().unwrap_or_default();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return dirs::home_dir().unwrap_or_default().join(rest);
    }
    PathBuf::from(path)
}
