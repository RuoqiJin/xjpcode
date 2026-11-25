//! 对应上游 `test/shell-kill-process-tree.test.ts`：杀进程树时的信号顺序与失败容忍。
//!
//! Windows 的 `taskkill /T` 那一半没搬（只做 Unix），对应的两条用例没有搬。

use xjpcode_coding_agent::utils::shell::kill_process_tree_with;

fn signal_name(signal: i32) -> &'static str {
    #[cfg(unix)]
    {
        if signal == libc::SIGSTOP {
            return "SIGSTOP";
        }
        if signal == libc::SIGKILL {
            return "SIGKILL";
        }
    }
    "OTHER"
}

fn record_calls(
    pid: i32,
    stop_before_kill: bool,
    results: impl FnMut(i32, i32) -> bool,
) -> Vec<(i32, &'static str)> {
    let mut calls = Vec::new();
    let mut results = results;
    kill_process_tree_with(
        pid,
        |target, signal| {
            calls.push((target, signal_name(signal)));
            results(target, signal)
        },
        stop_before_kill,
    );
    calls
}

#[test]
fn stops_the_macos_process_group_before_killing_it_without_resuming_it() {
    let calls = record_calls(1234, true, |_target, _signal| true);
    assert_eq!(calls, vec![(-1234, "SIGSTOP"), (-1234, "SIGKILL")]);
}

#[test]
fn still_kills_the_group_when_stopping_fails() {
    let calls = record_calls(1234, true, |target, signal| {
        if target < 0 && signal_name(signal) == "SIGSTOP" {
            return false;
        }
        true
    });
    assert_eq!(calls, vec![(-1234, "SIGSTOP"), (-1234, "SIGKILL")]);
}

#[test]
fn falls_back_to_killing_the_shell_when_the_group_kill_fails() {
    let calls = record_calls(1234, true, |target, signal| {
        !(target < 0 && signal_name(signal) == "SIGKILL")
    });
    assert_eq!(
        calls,
        vec![(-1234, "SIGSTOP"), (-1234, "SIGKILL"), (1234, "SIGKILL")]
    );
}

#[test]
fn tolerates_failure_of_both_group_signals_and_the_fallback() {
    let calls = record_calls(1234, true, |_target, _signal| false);
    assert_eq!(
        calls,
        vec![(-1234, "SIGSTOP"), (-1234, "SIGKILL"), (1234, "SIGKILL")]
    );
}

#[test]
fn keeps_linux_group_termination_unchanged() {
    let calls = record_calls(1234, false, |_target, _signal| true);
    assert_eq!(calls, vec![(-1234, "SIGKILL")]);
}
