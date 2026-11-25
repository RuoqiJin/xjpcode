//! 对应上游 `src/core/tools/bash.ts`：跑一条 shell 命令，输出留尾部、需要时落临时文件。
//!
//! 上游在 Unix 上把 shell 放进独立进程组，取消/超时时杀整组；这里一样。Windows 的 PowerShell 与
//! `commandTransport: "stdin"` 分支没搬（见 `docs/port/milestone-3.md` 差异表）。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};
use xjpcode_agent::{AgentTool, AgentToolResult, AgentToolUpdateCallback, ToolError};
use xjpcode_ai::types::ConstrainedSamplingStrict;
use xjpcode_ai::{
    AbortSignal, ConstrainedSampling, ConstrainedSamplingConfig, JsonObject, TextContent, Tool,
    ToolResultContentBlock,
};

use crate::core::tools::output_accumulator::{
    OutputAccumulator, OutputAccumulatorOptions, OutputSnapshot,
};
use crate::core::tools::truncate::{
    format_size, TruncatedBy, TruncationResult, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES,
};
use crate::core::tools::ToolSystemPromptContribution;
use crate::utils::child_process::spawn_pipe_readers;
use crate::utils::shell::{get_shell_config, get_shell_env, kill_process_tree, ShellConfig};

const MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;
/// pi: `STRUCTURED_OUTPUT_MAX_BYTES`
const STRUCTURED_OUTPUT_MAX_BYTES: usize = 1024 * 1024;
const MAX_TIMEOUT_SECONDS: f64 = MAX_TIMEOUT_MS / 1000.0;
/// pi: `BASH_UPDATE_THROTTLE_MS`
pub const BASH_UPDATE_THROTTLE_MS: u64 = 100;

/// pi: `bashToolSystemPromptContribution`
pub fn bash_tool_system_prompt_contribution() -> ToolSystemPromptContribution {
    ToolSystemPromptContribution {
        snippet: "Execute bash commands (ls, grep, find, etc.)",
        guidelines: &[
            "You can inspect PI_* environment variables for current model and session details.",
        ],
    }
}

/// pi: `resolveTimeoutMs`
fn resolve_timeout_ms(timeout: Option<f64>) -> Result<Option<u64>, ToolError> {
    let Some(timeout) = timeout else {
        return Ok(None);
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err(ToolError::from(
            "Invalid timeout: must be a finite number of seconds",
        ));
    }
    let timeout_ms = timeout * 1000.0;
    if timeout_ms > MAX_TIMEOUT_MS {
        return Err(ToolError::from(format!(
            "Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds"
        )));
    }
    Ok(Some(timeout_ms.round() as u64))
}

/// pi: `BashExecOptions`
pub struct BashExecOptions {
    pub on_data: Arc<dyn Fn(Vec<u8>) + Send + Sync>,
    pub signal: Option<AbortSignal>,
    pub timeout: Option<f64>,
    pub env: BTreeMap<String, String>,
}

/// pi: `BashExecResult`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashExecResult {
    pub exit_code: Option<i32>,
}

/// pi: `BashOperations.exec`
pub type BashExecFn = Arc<
    dyn Fn(
            String,
            PathBuf,
            BashExecOptions,
        ) -> xjpcode_ai::BoxFuture<'static, Result<BashExecResult, ToolError>>
        + Send
        + Sync,
>;

/// pi: `BashOperations`
#[derive(Clone)]
pub struct BashOperations {
    pub exec: BashExecFn,
}

enum WaitOutcome {
    Exited(std::io::Result<std::process::ExitStatus>),
    TimedOut,
    Aborted,
}

fn shell_config_for(shell_path: Option<&str>) -> Result<ShellConfig, ToolError> {
    get_shell_config(shell_path).map_err(ToolError::from)
}

async fn wait_for_abort(signal: Option<AbortSignal>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending().await,
    }
}

/// 退出码：正常退出取 code；被信号杀掉按 shell 惯例记 128 + 信号。
fn exit_code_of(status: &std::process::ExitStatus) -> Option<i32> {
    status.code().or_else(|| {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            status.signal().map(|signal| 128 + signal)
        }
        #[cfg(not(unix))]
        {
            None
        }
    })
}

/// pi: `createLocalShellOperations`（只用 bash）
pub fn create_local_bash_operations(shell_path: Option<String>) -> BashOperations {
    BashOperations {
        exec: Arc::new(move |command, cwd, options| {
            let shell_path = shell_path.clone();
            Box::pin(async move {
                let timeout_ms = resolve_timeout_ms(options.timeout)?;
                if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
                    return Err(ToolError::from("aborted"));
                }
                let shell_config = shell_config_for(shell_path.as_deref())?;
                if tokio::fs::metadata(&cwd).await.is_err() {
                    return Err(ToolError::from(format!(
                        "Working directory does not exist: {}\nCannot execute bash commands.",
                        cwd.to_string_lossy()
                    )));
                }

                let mut child = {
                    let mut command_builder = tokio::process::Command::new(&shell_config.shell);
                    command_builder.args(&shell_config.args).arg(&command);
                    command_builder
                        .current_dir(&cwd)
                        .env_clear()
                        .envs(&options.env)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped());
                    #[cfg(unix)]
                    {
                        command_builder.process_group(0);
                    }
                    command_builder.spawn().map_err(|error| {
                        ToolError::from(crate::core::tools::edit_diff::describe_io_error(&error))
                    })?
                };
                let pid = child.id().map(|pid| pid as i32);
                let stdout = child
                    .stdout
                    .take()
                    .map(|stream| Box::new(stream) as Box<dyn tokio::io::AsyncRead + Unpin + Send>);
                let stderr = child
                    .stderr
                    .take()
                    .map(|stream| Box::new(stream) as Box<dyn tokio::io::AsyncRead + Unpin + Send>);
                let readers =
                    spawn_pipe_readers(vec![stdout, stderr], Arc::clone(&options.on_data));
                let started_at = Instant::now();
                let timeout_duration = timeout_ms.map(std::time::Duration::from_millis);
                let signal = options.signal.clone();
                let timeout_text = format!("timeout:{}", options.timeout.unwrap_or(0.0));

                let outcome = match timeout_duration {
                    Some(timeout_duration) => {
                        tokio::select! {
                            status = child.wait() => WaitOutcome::Exited(status),
                            _ = tokio::time::sleep(timeout_duration) => WaitOutcome::TimedOut,
                            _ = wait_for_abort(signal.clone()) => WaitOutcome::Aborted,
                        }
                    }
                    None => {
                        tokio::select! {
                            status = child.wait() => WaitOutcome::Exited(status),
                            _ = wait_for_abort(signal.clone()) => WaitOutcome::Aborted,
                        }
                    }
                };

                let status = match outcome {
                    WaitOutcome::Exited(status) => {
                        let status = status.map_err(|error| {
                            ToolError::from(crate::core::tools::edit_diff::describe_io_error(
                                &error,
                            ))
                        })?;
                        // shell 自己退出了；后台进程可能还握着继承来的管道在写（#11613）。
                        let shell_exited_after = started_at.elapsed();
                        let exit_code = exit_code_of(&status).unwrap_or(1);
                        // shell 已退出：把两路管道读完；超时仍然管到收完为止（后台进程可能还在写）。
                        match timeout_duration {
                            Some(timeout_duration) => {
                                let remaining =
                                    timeout_duration.saturating_sub(started_at.elapsed());
                                if tokio::time::timeout(remaining, readers.finish_after_exit())
                                    .await
                                    .is_err()
                                {
                                    if let Some(pid) = pid {
                                        kill_process_tree(pid);
                                    }
                                    let after_seconds =
                                        (shell_exited_after.as_secs_f64() * 10.0).round() / 10.0;
                                    return Err(ToolError::from(format!(
                                        "{timeout_text}\nshell_exit:{exit_code}:{after_seconds}"
                                    )));
                                }
                            }
                            None => readers.finish_after_exit().await,
                        }
                        status
                    }
                    WaitOutcome::TimedOut => {
                        if let Some(pid) = pid {
                            kill_process_tree(pid);
                        }
                        let _ = child.wait().await;
                        readers.finish_after_exit().await;
                        return Err(ToolError::from(timeout_text));
                    }
                    WaitOutcome::Aborted => {
                        if let Some(pid) = pid {
                            kill_process_tree(pid);
                        }
                        let _ = child.wait().await;
                        readers.finish_after_exit().await;
                        return Err(ToolError::from("aborted"));
                    }
                };

                if signal.as_ref().is_some_and(AbortSignal::aborted) {
                    return Err(ToolError::from("aborted"));
                }

                Ok(BashExecResult {
                    exit_code: exit_code_of(&status),
                })
            })
        }),
    }
}

/// pi: `BashSpawnContext`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashSpawnContext {
    pub command: String,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
}

/// pi: `BashSpawnHook`
pub type BashSpawnHook = Arc<dyn Fn(BashSpawnContext) -> BashSpawnContext + Send + Sync>;

/// pi: `BashToolOptions`
#[derive(Clone, Default)]
pub struct BashToolOptions {
    pub operations: Option<BashOperations>,
    /// 每条命令前面拼的一段（例如 shell 初始化）。
    pub command_prefix: Option<String>,
    /// 指定 shell（找不到时报错）。
    pub shell_path: Option<String>,
    /// 把当前会话的信息放进 `PI_*` 环境变量（上游缺省开；这里缺省关，CLI 显式打开）。
    pub expose_session_environment: Option<bool>,
    /// 会话信息（`PI_SESSION_ID`、`PI_SESSION_FILE`、`PI_PROVIDER`、`PI_MODEL`、
    /// `PI_REASONING_LEVEL`）；`expose_session_environment` 打开时填进去。
    pub session_environment: Option<BTreeMap<String, String>>,
    pub spawn_hook: Option<BashSpawnHook>,
}

/// pi: `BashToolDetails`
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BashToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
}

/// pi: `createBashTool`
pub struct BashTool {
    cwd: PathBuf,
    operations: BashOperations,
    command_prefix: Option<String>,
    expose_session_environment: bool,
    session_environment: Option<BTreeMap<String, String>>,
    spawn_hook: Option<BashSpawnHook>,
}

/// pi: `createBashTool`
pub fn create_bash_tool(cwd: impl Into<PathBuf>, options: BashToolOptions) -> BashTool {
    let shell_path = options.shell_path.clone();
    BashTool {
        cwd: cwd.into(),
        operations: options
            .operations
            .unwrap_or_else(|| create_local_bash_operations(shell_path)),
        command_prefix: options.command_prefix,
        expose_session_environment: options.expose_session_environment.unwrap_or(false),
        session_environment: options.session_environment,
        spawn_hook: options.spawn_hook,
    }
}

impl BashTool {
    /// 上游工具定义上的 `promptSnippet`。
    pub fn prompt_snippet(&self) -> &'static str {
        bash_tool_system_prompt_contribution().snippet
    }

    /// 上游的 `promptGuidelines`：只有暴露会话环境时才有（`exposeSessionEnvironment`）。
    pub fn prompt_guidelines(&self) -> Option<&'static [&'static str]> {
        if self.expose_session_environment {
            Some(bash_tool_system_prompt_contribution().guidelines)
        } else {
            None
        }
    }

    /// pi: `resolveSpawnContext`
    fn resolve_spawn_context(&self, command: String) -> BashSpawnContext {
        let mut env = get_shell_env();
        for key in [
            "PI_SESSION_ID",
            "PI_SESSION_FILE",
            "PI_PROVIDER",
            "PI_MODEL",
            "PI_REASONING_LEVEL",
        ] {
            env.remove(key);
        }
        if self.expose_session_environment {
            if let Some(session_environment) = &self.session_environment {
                for (key, value) in session_environment {
                    env.insert(key.clone(), value.clone());
                }
            }
        }
        let base = BashSpawnContext {
            command,
            cwd: self.cwd.clone(),
            env,
        };
        match &self.spawn_hook {
            Some(hook) => hook(base),
            None => base,
        }
    }
}

/// 一次执行里要共享的累加器与 update 节流状态。
///
/// 读取任务只把数据交给累加器（它会自己落临时文件），要发出去的 update 先放进 `pending`，由执行
/// 循环取出来按顺序 await。这样通道不会把整段输出堆在内存里。
struct OutputState {
    output: OutputAccumulator,
    update_dirty: bool,
    last_update_at: Option<Instant>,
    pending_updates: Vec<AgentToolResult>,
}

fn snapshot_details(snapshot: &OutputSnapshot) -> BashToolDetails {
    BashToolDetails {
        truncation: snapshot
            .truncation
            .truncated
            .then(|| snapshot.truncation.clone()),
        full_output_path: snapshot
            .full_output_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
    }
}

/// pi: 每 100 毫秒最多一次 output update；`force` 时必发（收尾）。
fn build_update(state: &mut OutputState, force: bool) -> Option<AgentToolResult> {
    if !force && !state.update_dirty {
        return None;
    }
    let due = state
        .last_update_at
        .is_none_or(|last| last.elapsed().as_millis() as u64 >= BASH_UPDATE_THROTTLE_MS);
    if !force && !due {
        return None;
    }
    state.update_dirty = false;
    state.last_update_at = Some(Instant::now());
    let snapshot = state.output.snapshot(true);
    let details = serde_json::to_value(snapshot_details(&snapshot))
        .unwrap_or_else(|_| Value::Object(JsonObject::new()));
    Some(AgentToolResult {
        content: vec![ToolResultContentBlock::Text(TextContent::new(
            snapshot.content.clone(),
        ))],
        details: Some(details),
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    })
}

fn take_pending_updates(state: &std::sync::Mutex<OutputState>) -> Vec<AgentToolResult> {
    std::mem::take(&mut state.lock().expect("bash output state").pending_updates)
}

fn push_update(
    state: &std::sync::Mutex<OutputState>,
    callback: Option<&AgentToolUpdateCallback>,
    force: bool,
) -> Option<AgentToolResult> {
    if callback.is_none() {
        return None;
    }
    let mut state = state.lock().expect("bash output state");
    build_update(&mut state, force)
}

/// pi: `formatOutput`
fn format_output(
    snapshot: &OutputSnapshot,
    output: &OutputAccumulator,
    empty_text: &str,
) -> (String, Option<Value>) {
    let truncation = &snapshot.truncation;
    let mut text = if snapshot.content.is_empty() {
        empty_text.to_string()
    } else {
        snapshot.content.clone()
    };
    let mut details = None;
    if truncation.truncated {
        details = serde_json::to_value(snapshot_details(snapshot)).ok();
        let start_line = truncation.total_lines - truncation.output_lines + 1;
        let end_line = truncation.total_lines;
        let full_output_path = snapshot
            .full_output_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        if truncation.last_line_partial {
            let last_line_size = format_size(output.get_last_line_bytes());
            text.push_str(&format!(
                "\n\n[Showing last {} of line {start_line} (line is {last_line_size}). Full output: {full_output_path}]",
                format_size(truncation.output_bytes)
            ));
        } else if truncation.truncated_by == Some(TruncatedBy::Lines) {
            text.push_str(&format!(
                "\n\n[Showing lines {start_line}-{end_line} of {}. Full output: {full_output_path}]",
                truncation.total_lines
            ));
        } else {
            text.push_str(&format!(
                "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit). Full output: {full_output_path}]",
                truncation.total_lines,
                format_size(DEFAULT_MAX_BYTES)
            ));
        }
    }
    (text, details)
}

fn append_status(text: &str, status: &str) -> String {
    if text.is_empty() {
        status.to_string()
    } else {
        format!("{text}\n\n{status}")
    }
}

impl AgentTool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn label(&self) -> &str {
        "bash"
    }

    fn declaration(&self) -> Tool {
        Tool {
            name: self.name().to_string(),
            description: format!(
                "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
                DEFAULT_MAX_BYTES / 1024
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute" },
                    "timeout": { "type": "number", "description": "Timeout in seconds (optional, no default timeout)" }
                },
                "required": ["command"]
            }),
            constrained_sampling: Some(ConstrainedSampling::Config(
                ConstrainedSamplingConfig::JsonSchema {
                    strict: ConstrainedSamplingStrict::Prefer,
                },
            )),
        }
    }

    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: &'a JsonObject,
        signal: Option<&'a AbortSignal>,
        on_update: Option<&'a AgentToolUpdateCallback>,
    ) -> xjpcode_ai::BoxFuture<'a, Result<AgentToolResult, ToolError>> {
        Box::pin(async move {
            let command = params
                .get("command")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::from("bash requires a command"))?
                .to_string();
            let timeout = params.get("timeout").and_then(Value::as_f64);
            let resolved_command = match &self.command_prefix {
                Some(prefix) => format!("{prefix}\n{command}"),
                None => command,
            };
            let spawn_context = self.resolve_spawn_context(resolved_command);

            let state = Arc::new(std::sync::Mutex::new(OutputState {
                output: OutputAccumulator::new(OutputAccumulatorOptions {
                    temp_file_prefix: Some("pi-bash".to_string()),
                    ..OutputAccumulatorOptions::default()
                }),
                update_dirty: false,
                last_update_at: None,
                pending_updates: Vec::new(),
            }));
            let callback = on_update.cloned();

            if let Some(callback) = &callback {
                callback(AgentToolResult {
                    content: Vec::new(),
                    details: None,
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
                .await;
            }

            let on_data: Arc<dyn Fn(Vec<u8>) + Send + Sync> = {
                let state = Arc::clone(&state);
                let callback = callback.clone();
                Arc::new(move |data: Vec<u8>| {
                    let mut guard = state.lock().expect("bash output state");
                    guard.output.append(&data);
                    guard.update_dirty = true;
                    if callback.is_some() {
                        if let Some(update) = build_update(&mut guard, false) {
                            guard.pending_updates.push(update);
                        }
                    }
                })
            };

            let started_at = Instant::now();
            let mut exec_future = (self.operations.exec)(
                spawn_context.command,
                spawn_context.cwd,
                BashExecOptions {
                    on_data,
                    signal: signal.cloned(),
                    timeout,
                    env: spawn_context.env,
                },
            );
            let exec_result = loop {
                tokio::select! {
                    result = &mut exec_future => break result,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {
                        for update in take_pending_updates(&state) {
                            if let Some(callback) = &callback {
                                callback(update).await;
                            }
                        }
                    }
                }
            };
            for update in take_pending_updates(&state) {
                if let Some(callback) = &callback {
                    callback(update).await;
                }
            }

            let snapshot = {
                let mut guard = state.lock().expect("bash output state");
                guard.output.finish();
                guard.update_dirty = true;
                guard.output.snapshot(true)
            };
            if let Some(update) = push_update(&state, callback.as_ref(), true) {
                if let Some(callback) = &callback {
                    callback(update).await;
                }
            }
            state
                .lock()
                .expect("bash output state")
                .output
                .close_temp_file();

            match exec_result {
                Ok(result) => {
                    let (output_text, details) = {
                        let guard = state.lock().expect("bash output state");
                        format_output(&snapshot, &guard.output, "(no output)")
                    };
                    let Some(exit_code) = result.exit_code else {
                        return Err(ToolError::from(append_status(
                            &output_text,
                            "Command terminated without an exit code",
                        )));
                    };
                    let wall_time_seconds =
                        (started_at.elapsed().as_secs_f64() * 10.0).round() / 10.0;
                    let full_output = state
                        .lock()
                        .expect("bash output state")
                        .output
                        .read_full_output(STRUCTURED_OUTPUT_MAX_BYTES);
                    let mut structured = JsonObject::new();
                    structured.insert("output".to_string(), Value::String(full_output.content));
                    structured.insert("truncated".to_string(), Value::Bool(full_output.truncated));
                    if full_output.truncated {
                        if let Some(path) = &snapshot.full_output_path {
                            structured.insert(
                                "full_output_path".to_string(),
                                Value::String(path.to_string_lossy().into_owned()),
                            );
                        }
                    }
                    structured.insert("exit_code".to_string(), Value::from(exit_code));
                    structured.insert(
                        "wall_time_seconds".to_string(),
                        Value::from(wall_time_seconds),
                    );
                    if exit_code != 0 {
                        return Ok(AgentToolResult {
                            content: vec![ToolResultContentBlock::Text(TextContent::new(
                                append_status(
                                    &output_text,
                                    &format!("Command exited with code {exit_code}"),
                                ),
                            ))],
                            details,
                            structured_content: Some(Value::Object(structured)),
                            usage: None,
                            is_error: Some(true),
                            terminate: None,
                        });
                    }
                    Ok(AgentToolResult {
                        content: vec![ToolResultContentBlock::Text(TextContent::new(output_text))],
                        details,
                        structured_content: Some(Value::Object(structured)),
                        usage: None,
                        is_error: None,
                        terminate: None,
                    })
                }
                Err(error) => {
                    let guard = state.lock().expect("bash output state");
                    let (text, _) = format_output(&snapshot, &guard.output, "");
                    if error.message() == "aborted" {
                        return Err(ToolError::from(append_status(&text, "Command aborted")));
                    }
                    if let Some(rest) = error.message().strip_prefix("timeout:") {
                        let mut lines = rest.split('\n');
                        let seconds = lines.next().unwrap_or("0");
                        let mut status = format!("Command timed out after {seconds} seconds");
                        if let Some(shell_exit) =
                            lines.find_map(|line| line.strip_prefix("shell_exit:"))
                        {
                            let mut parts = shell_exit.splitn(2, ':');
                            let exit_code = parts.next().unwrap_or("0");
                            let after = parts.next().unwrap_or("0");
                            // 两件事都要说：命令真正的结果，以及超时那一刻后台进程还在写（#11613）。
                            status.push_str(&format!(
                                ". The shell exited with code {exit_code} after {after} seconds, but background processes it started kept writing output until the timeout."
                            ));
                        }
                        return Err(ToolError::from(append_status(&text, &status)));
                    }
                    Err(error)
                }
            }
        })
    }
}
