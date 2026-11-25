#![allow(dead_code)]
//! 移植自上游 `packages/agent/test/utils/wait-for-tick.ts`——让出一个事件循环回合。
//!
//! 上游是 `setTimeout(resolve, 0)`；Rust 侧用 tokio 定时器的 0 毫秒。

/// pi: `waitForTick`
pub async fn wait_for_tick() {
    tokio::time::sleep(std::time::Duration::from_millis(0)).await;
}
