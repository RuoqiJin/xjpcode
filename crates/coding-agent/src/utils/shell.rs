//! 对应上游 `src/utils/shell.ts` 的 Unix 部分：找 shell、拼环境变量、杀进程树。
//!
//! Windows 的 Git Bash 查找、PowerShell 与 detached child 的登记没有搬（见
//! `docs/port/milestone-3.md` 差异表）。

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::get_bin_dir;

/// pi: `commandTransport`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandTransport {
    Argv,
    Stdin,
}

/// pi: `ShellConfig`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellConfig {
    pub shell: String,
    pub args: Vec<String>,
    pub command_transport: CommandTransport,
}

fn shell_path_exists(path: &str) -> bool {
    Path::new(path).exists()
}

fn find_executable_on_path(executable: &str) -> Option<String> {
    let output = std::process::Command::new("which")
        .arg(executable)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
}

fn get_bash_shell_config(shell: &str) -> ShellConfig {
    ShellConfig {
        shell: shell.to_string(),
        args: vec!["-c".to_string()],
        command_transport: CommandTransport::Argv,
    }
}

/// pi: `getShellConfig`
pub fn get_shell_config(custom_shell_path: Option<&str>) -> Result<ShellConfig, String> {
    if let Some(custom) = custom_shell_path {
        if shell_path_exists(custom) {
            return Ok(get_bash_shell_config(custom));
        }
        return Err(format!("Custom shell path not found: {custom}"));
    }

    if shell_path_exists("/bin/bash") {
        return Ok(get_bash_shell_config("/bin/bash"));
    }
    if let Some(bash) = find_executable_on_path("bash") {
        return Ok(get_bash_shell_config(&bash));
    }
    Ok(ShellConfig {
        shell: "sh".to_string(),
        args: vec!["-c".to_string()],
        command_transport: CommandTransport::Argv,
    })
}

/// pi: `getShellEnv`——把 xjpcode 自己的 bin 目录放到 `PATH` 前面。
pub fn get_shell_env() -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    let path_key = env
        .keys()
        .find(|key| key.eq_ignore_ascii_case("path"))
        .cloned()
        .unwrap_or_else(|| "PATH".to_string());
    let current_path = env.get(&path_key).cloned().unwrap_or_default();
    let bin_dir = get_bin_dir().to_string_lossy().into_owned();
    let entries: Vec<&str> = current_path
        .split(':')
        .filter(|entry| !entry.is_empty())
        .collect();
    if !entries.iter().any(|entry| *entry == bin_dir) {
        let updated = if current_path.is_empty() {
            bin_dir
        } else {
            format!("{bin_dir}:{current_path}")
        };
        env.insert(path_key, updated);
    }
    env
}

/// pi: `killProcessTree`
pub fn kill_process_tree(pid: i32) {
    #[cfg(unix)]
    {
        kill_process_tree_with(
            pid,
            |target, signal| unsafe { libc::kill(target, signal) } == 0,
            cfg!(target_os = "macos"),
        );
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

/// `killProcessTree` 的可测部分：信号序列照上游；`stop_before_kill` 只在 macOS 为真。
pub fn kill_process_tree_with(
    pid: i32,
    mut kill: impl FnMut(i32, i32) -> bool,
    stop_before_kill: bool,
) {
    if stop_before_kill {
        // macOS 先停住整组，减少「杀掉一个、shell 又起了下一个」的窗口（上游同义）。
        let _ = kill(-pid, signal_stop());
    }
    if !kill(-pid, signal_kill()) {
        let _ = kill(pid, signal_kill());
    }
}

#[cfg(unix)]
fn signal_stop() -> i32 {
    libc::SIGSTOP
}

#[cfg(unix)]
fn signal_kill() -> i32 {
    libc::SIGKILL
}

#[cfg(not(unix))]
fn signal_stop() -> i32 {
    0
}

#[cfg(not(unix))]
fn signal_kill() -> i32 {
    0
}
