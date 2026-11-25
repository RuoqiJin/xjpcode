//! 对应上游 `packages/schema/src/shared.ts`。
//!
//! 一个还活着的 schema 用到的正则按源码共享：工具 schema 会把少数几个 pattern 重复很多次，而一个
//! 跑过之后的 `RegExp` 占的内存不小。缓存持弱引用，正则被回收后条目就没了，所以它不吊住任何东西。

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};

use regress::Regex;

use crate::types::CompileError;

/// 一个 `pattern` 源码对应的正则，按源码共享。
///
/// 上游：pattern 是带 `u` 标志的 ECMAScript 正则；某个源码在 `u` 下不合法时（一些老 schema 会
/// 转义 `u` 不接受的字面字符），退回不带 `u` 再编译一次。Rust 侧用 `regress`——按 JS 语法解释
/// 的正则库（见 `PORTING.md` 差异表）。
pub fn shared_regex(source: &str) -> Result<Arc<Regex>, CompileError> {
    let mut regexes = REGEXES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(hit) = regexes.get(source).and_then(Weak::upgrade) {
        return Ok(hit);
    }
    let re = Arc::new(compile_regex(source)?);
    // 上游靠 FinalizationRegistry 删条目；Rust 侧在插入时顺手丢掉已经没人用的弱引用。
    regexes.retain(|_, weak| weak.strong_count() > 0);
    regexes.insert(source.to_string(), Arc::downgrade(&re));
    Ok(re)
}

fn compile_regex(source: &str) -> Result<Regex, CompileError> {
    match Regex::with_flags(source, "u") {
        Ok(re) => Ok(re),
        Err(_) => Regex::with_flags(source, "").map_err(|error| {
            CompileError::new(format!("Invalid regular expression {source}: {error}"))
        }),
    }
}

static REGEXES: LazyLock<Mutex<BTreeMap<String, Weak<Regex>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// 缓存里还有多少条（供测试）。
pub fn shared_regex_count() -> usize {
    REGEXES.lock().unwrap_or_else(|e| e.into_inner()).len()
}
