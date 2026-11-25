//! 对应 pi 里由平台提供的 `AbortSignal` / `AbortController`。
//!
//! 上游这两个类型没有源文件：它们是 Node 与浏览器自带的。Rust 没有对应物，而 agent 的循环、钩子与
//! proxy 都要读同一个取消信号，所以在这里建一份，`crates/agent` 直接用（见 `PORTING.md` 差异表 #75）。
//!
//! 语义照平台实现：`abort()` 之后 `aborted()` 恒为真，且不会回退；`cancelled()` 在取消时返回，
//! 已经取消时立即返回。与上游不同的是信号可以 `Clone`（Rust 的异步任务要求 `'static`，跨任务时
//! 克隆一份，与 `EventStream` 的句柄同一套做法）。

use tokio_util::sync::CancellationToken;

/// pi: `AbortSignal`（平台自带）。
#[derive(Debug, Clone)]
pub struct AbortSignal {
    token: CancellationToken,
}

impl AbortSignal {
    /// 一个永远不会取消的信号（上游没有这个构造，Rust 里可选参数需要它）。
    pub fn never() -> Self {
        AbortController::new().signal()
    }

    /// 上游的 `signal.aborted`。只有 `abort()` 能让它变成 `true`，之后一直是 `true`。
    pub fn aborted(&self) -> bool {
        self.token.is_cancelled()
    }

    /// 等取消发生；已经取消时立即返回。上游用 `addEventListener("abort", …)` 的位置就是这里。
    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }
}

/// pi: `AbortController`（平台自带）。
#[derive(Debug, Clone)]
pub struct AbortController {
    token: CancellationToken,
}

impl Default for AbortController {
    fn default() -> Self {
        Self::new()
    }
}

impl AbortController {
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
        }
    }

    pub fn signal(&self) -> AbortSignal {
        AbortSignal {
            token: self.token.clone(),
        }
    }

    /// 上游的 `controller.abort()`。可以重复调用。
    pub fn abort(&self) {
        self.token.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::{AbortController, AbortSignal};

    #[test]
    fn starts_aborted_only_after_abort() {
        let controller = AbortController::new();
        let signal = controller.signal();
        assert!(!signal.aborted());
        controller.abort();
        assert!(signal.aborted());
        // 克隆出来的句柄看的是同一个信号。
        assert!(signal.clone().aborted());
    }

    #[tokio::test]
    async fn waiting_returns_on_abort_and_immediately_after_it() {
        let controller = AbortController::new();
        let signal = controller.signal();
        let waiter = tokio::spawn({
            let signal = signal.clone();
            async move { signal.cancelled().await }
        });
        controller.abort();
        waiter.await.expect("waiter");
        // 已经取消时 `cancelled()` 立即返回。
        AbortSignal::cancelled(&signal).await;
    }
}
