//! Library surface for xjpcode. The CLI binary is a thin adapter over this crate.
//!
//! The interface (ratatui TUI + plain transcript) sits on top of the ported pi core:
//! `xjpcode-agent` (loop), `xjpcode-ai` (DeepSeek wire) and `xjpcode-coding-agent`
//! (tools, rules, skills, session files, retries, compaction).

pub mod build_info;
pub mod format;
pub mod limits;
pub mod login;
pub mod screens;
pub mod session_setup;
pub mod tui;
pub mod ui;
