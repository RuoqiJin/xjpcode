//! 对应上游 `packages/agent/src/stream-fn.ts`。

use std::sync::{LazyLock, Mutex};

use crate::types::{AgentError, StreamFn};

/// pi 的模块级变量 `defaultStreamFn`。
///
/// 上游是单线程里的一个变量；Rust 侧 loop 可能在别的线程上跑，所以放进程级、加锁（见
/// `PORTING.md` 差异表 #75）。
static DEFAULT_STREAM_FN: LazyLock<Mutex<Option<StreamFn>>> = LazyLock::new(|| Mutex::new(None));

/// pi: `setDefaultStreamFn`
pub fn set_default_stream_fn(stream_fn: Option<StreamFn>) {
    *DEFAULT_STREAM_FN.lock().expect("default stream fn") = stream_fn;
}

/// pi: `getDefaultStreamFn`——没设置时给上游同一句话的 `Err`。
pub fn get_default_stream_fn() -> Result<StreamFn, AgentError> {
    DEFAULT_STREAM_FN
        .lock()
        .expect("default stream fn")
        .clone()
        .ok_or_else(|| {
            AgentError::new(
                "No default stream function configured. Pass streamFn explicitly or call setDefaultStreamFn().",
            )
        })
}
