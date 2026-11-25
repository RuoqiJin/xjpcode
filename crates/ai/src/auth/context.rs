//! 对应上游 `packages/ai/src/auth/context.ts`。
//!
//! 缺省的 `AuthContext`：环境变量来自进程环境（去掉首尾空白后为空按没有算），文件是否存在
//! 走 `std::fs`；路径开头的 `~` 换成本机家目录。

use std::path::PathBuf;

use crate::auth::types::AuthContext;
use crate::types::BoxFuture;

/// pi: `defaultProviderAuthContext`
#[derive(Debug, Clone, Default)]
pub struct DefaultAuthContext;

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix('~') {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest.trim_start_matches('/'));
        }
    }
    PathBuf::from(path)
}

impl AuthContext for DefaultAuthContext {
    fn env<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
    }

    fn file_exists<'a>(&'a self, path: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move { std::fs::metadata(expand_home(path)).is_ok() })
    }
}

/// 一份固定取值的上下文：测试里用。
#[derive(Debug, Clone, Default)]
pub struct StaticAuthContext {
    pub env: std::collections::BTreeMap<String, String>,
    pub files: std::collections::BTreeSet<String>,
}

impl AuthContext for StaticAuthContext {
    fn env<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Option<String>> {
        let value = self
            .env
            .get(name)
            .cloned()
            .filter(|value| !value.trim().is_empty());
        Box::pin(async move { value })
    }

    fn file_exists<'a>(&'a self, path: &'a str) -> BoxFuture<'a, bool> {
        let exists = self.files.contains(path);
        Box::pin(async move { exists })
    }
}
