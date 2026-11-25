//! 对应上游 `test/suite/regressions/` 的三条 bash 回归：5208（退出后的迟到输出）、
//! 5303（退出后后台进程还在写时的收尾）、11613（超时时把 shell 的真实退出码说出来）。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use xjpcode_agent::AgentTool;
use xjpcode_coding_agent::core::tools::bash::{
    create_bash_tool, BashExecOptions, BashExecResult, BashOperations, BashToolOptions,
};
use xjpcode_coding_agent::utils::child_process::spawn_pipe_readers;

fn object(value: serde_json::Value) -> xjpcode_ai::JsonObject {
    value.as_object().unwrap().clone()
}

fn text_of(result: &xjpcode_agent::AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            xjpcode_ai::ToolResultContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 回归 #5208：`exec` 返回之后才到的输出回调不再改变结果。
#[tokio::test]
async fn ignores_output_callbacks_after_bash_operations_resolve() {
    let operations = BashOperations {
        exec: Arc::new(|_command, _cwd, options: BashExecOptions| {
            Box::pin(async move {
                let on_data = Arc::clone(&options.on_data);
                on_data(b"before\n".to_vec());
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    on_data(b"late\n".to_vec());
                });
                Ok(BashExecResult { exit_code: Some(0) })
            })
        }),
    };
    let tool = create_bash_tool(
        std::env::temp_dir(),
        BashToolOptions {
            operations: Some(operations),
            ..BashToolOptions::default()
        },
    );
    let result = tool
        .execute(
            "test-call-late-output",
            &object(json!({"command": "late-output"})),
            None,
            None,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(text_of(&result).trim(), "before");
}

/// 回归 #5303：exit 之后，管道每来一块数据就重新计时，正在写的后台进程不会被截断。
#[tokio::test]
async fn captures_output_emitted_after_exit_while_a_descendant_holds_stdout_open() {
    let (mut writer, reader) = tokio::io::duplex(64 * 1024);
    let output = Arc::new(Mutex::new(String::new()));
    let collected = Arc::clone(&output);
    let readers = spawn_pipe_readers(
        vec![Some(
            Box::new(reader) as Box<dyn tokio::io::AsyncRead + Unpin + Send>
        )],
        Arc::new(move |data| {
            collected
                .lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&data));
        }),
    );

    let writing = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        // shell 退出前写一块；退出之后每 10ms 写一块。间隔要比 100ms 的 grace 小得多：
        // 原来第一块之后睡 100ms，正好等于 grace，机器一忙就会被当成"安静了"。
        writer.write_all(b"HEAD\n").await.unwrap();
        for index in 1..=6 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            writer
                .write_all(format!("TICK{index}\n").as_bytes())
                .await
                .unwrap();
        }
    });

    // 模拟 shell 已经退出：从这里开始只等管道安静。
    readers.finish_after_exit().await;
    let captured = output.lock().unwrap().clone();
    assert!(captured.contains("HEAD"), "{captured:?}");
    assert!(captured.contains("TICK6"), "{captured:?}");
    // 让写手也能结束，避免测试之间留下任务。
    let _ = tokio::time::timeout(Duration::from_secs(1), writing).await;
}

/// 回归 #5303 的另一半：继承来的管道一直开着但很安静，等一个 grace 就返回。
#[tokio::test]
async fn resolves_after_the_grace_when_a_descendant_holds_stdout_open_but_stays_quiet() {
    let (mut writer, reader) = tokio::io::duplex(1024);
    let output = Arc::new(Mutex::new(String::new()));
    let collected = Arc::clone(&output);
    let readers = spawn_pipe_readers(
        vec![Some(
            Box::new(reader) as Box<dyn tokio::io::AsyncRead + Unpin + Send>
        )],
        Arc::new(move |data| {
            collected
                .lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&data));
        }),
    );
    {
        use tokio::io::AsyncWriteExt;
        writer.write_all(b"DONE\n").await.unwrap();
    }

    let started = Instant::now();
    readers.finish_after_exit().await;
    let elapsed = started.elapsed();
    assert!(output.lock().unwrap().contains("DONE"));
    assert!(elapsed >= Duration::from_millis(90), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    drop(writer);
}

async fn run_expecting_timeout(command: &str, timeout: f64) -> String {
    let tool = create_bash_tool(std::env::temp_dir(), BashToolOptions::default());
    tool.execute(
        "test-11613",
        &object(json!({ "command": command, "timeout": timeout })),
        None,
        None,
    )
    .await
    .expect_err("expected the command to time out")
    .message()
    .to_string()
}

/// 回归 #11613：shell 已经退出、后台进程还在写；超时时两个事实都要报。
#[cfg(unix)]
#[tokio::test]
async fn reports_the_shells_exit_code_when_background_processes_kept_writing_until_the_timeout() {
    let message = run_expecting_timeout(
        "(while true; do echo tick; sleep 0.02; done) & sleep 0.2; echo boom >&2; exit 3",
        1.0,
    )
    .await;
    assert!(message.contains("boom"), "{message}");
    assert!(
        message.contains("Command timed out after 1 seconds."),
        "{message}"
    );
    assert!(
        message.contains(". The shell exited with code 3 after 0."),
        "{message}"
    );
    assert!(
        message.ends_with("kept writing output until the timeout."),
        "{message}"
    );
}

/// 回归 #11613 的另一半：到时限时 shell 还在跑，只说超时。
#[cfg(unix)]
#[tokio::test]
async fn keeps_the_plain_timeout_message_when_the_shell_was_still_running_at_the_deadline() {
    let message = run_expecting_timeout("echo started; sleep 30", 0.3).await;
    assert!(message.contains("started"), "{message}");
    assert!(
        message.ends_with("Command timed out after 0.3 seconds"),
        "{message}"
    );
    assert!(!message.contains("The shell exited with code"), "{message}");
}
