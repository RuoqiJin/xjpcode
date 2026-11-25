//! 全屏界面入口：界面本体在 `crate::tui`，状态在 `crate::ui`。
//!
//! 这里保留 `screens::repl::run` 这个名字给 `main.rs`；底下接的是新内核的 `AgentSession`。

use anyhow::Result;

use crate::session_setup::{OpenedSession, SessionSetup};

pub fn run(rt: &tokio::runtime::Runtime, setup: SessionSetup, opened: OpenedSession) -> Result<()> {
    crate::tui::run(rt, setup, opened)
}
