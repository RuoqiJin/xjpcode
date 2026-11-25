//! Plain transcript mode — one-shot answers and a line REPL over `AgentSession`.
//!
//! No alternate screen, no raw mode: the answer streams to stdout, tool calls and
//! results get a line each, and retry/compaction notices go to stderr. Ctrl-C
//! interrupts the current run without killing the process.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use tokio::runtime::Runtime;
use xjpcode_agent::AgentMessage;
use xjpcode_ai::providers::deepseek;
use xjpcode_ai::StopReason;
use xjpcode_coding_agent::core::agent_session::{AgentSession, AgentSessionEvent};

use crate::screens::session_text::{reason_name, split_command, tool_result_text};
use crate::session_setup::SessionSetup;

pub fn run_once(rt: &Runtime, session: &Arc<AgentSession>, prompt: &str) -> Result<()> {
    if let Err(error) = prompt_once(rt, session, prompt, false) {
        // The failure line is already on stderr; keep the exit code non-zero.
        return Err(anyhow::anyhow!(error));
    }
    Ok(())
}

pub fn run_interactive(
    rt: &Runtime,
    setup: &mut SessionSetup,
    session: &Arc<AgentSession>,
    resumed: bool,
) -> Result<()> {
    print_header(setup, session, resumed);
    let mut line = String::new();
    loop {
        print!("> ");
        io::stdout().flush()?;
        line.clear();
        match io::stdin().read_line(&mut line) {
            Ok(0) => {
                println!();
                break;
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!("error reading input: {error}");
                break;
            }
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('/') {
            let (cmd, args) = split_command(trimmed);
            match handle_command(rt, setup, session, cmd, args) {
                Ok(true) => continue,
                Ok(false) => break,
                Err(error) => {
                    eprintln!("error: {error:#}");
                    continue;
                }
            }
        }
        if let Err(error) = prompt_once(rt, session, trimmed, true) {
            eprintln!("error: {error}");
        }
    }
    Ok(())
}

fn print_header(setup: &SessionSetup, session: &Arc<AgentSession>, resumed: bool) {
    let model = session.model();
    println!("XJPcode {} · transcript mode", env!("CARGO_PKG_VERSION"));
    println!(
        "model: {} · {} tools · rules {} · skills {}",
        model.id,
        session.agent().state().tools.len(),
        setup.rule_count(),
        setup.skill_count()
    );
    if let Some(file) = session.session_file() {
        println!(
            "session: {}{}",
            session.session_id(),
            if resumed { " (resumed)" } else { "" }
        );
        println!("file: {}", file.display());
    }
    if resumed {
        println!(
            "↻ Resumed session {} ({} model-context messages)",
            session.session_id(),
            session.messages().len()
        );
    }
    println!(
        "Type /help for commands, /quit to exit, or run `xjpcode` for the full-screen interface.\n"
    );
}

fn handle_command(
    rt: &Runtime,
    setup: &mut SessionSetup,
    session: &Arc<AgentSession>,
    cmd: &str,
    args: &str,
) -> Result<bool> {
    match cmd {
        "/help" => println!(
            "Commands:\n  /help             Show this help\n  /tools            List tools\n  /model [id]       Show or switch the model\n  /compact          Compact the conversation now\n  /session          Show this session's file and id\n  /quit, /exit      Exit\n\nCtrl-C interrupts the current answer without leaving the program."
        ),
        "/tools" => {
            let tools = session.agent().state().tools;
            if tools.is_empty() {
                println!("No tools available.");
            } else {
                for tool in &tools {
                    let declaration = tool.declaration();
                    let summary = declaration
                        .description
                        .lines()
                        .find(|line| !line.trim().is_empty())
                        .unwrap_or("");
                    println!("  {}  {}", declaration.name, summary);
                }
            }
        }
        "/model" => {
            let requested = args.trim();
            if requested.is_empty() {
                let available = deepseek::models()
                    .into_iter()
                    .map(|model| model.id)
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("current model: {}\navailable: {available}", session.model().id);
            } else {
                match deepseek::model(requested) {
                    Some(model) => match session.set_model(model.clone()) {
                        Ok(()) => {
                            setup.remember_model(model);
                            println!("Switched to: {requested}");
                        }
                        Err(error) => println!("could not switch model: {error}"),
                    },
                    None => println!("unknown model {requested}"),
                }
            }
        }
        "/compact" => match rt.block_on(session.compact(None)) {
            Ok(result) => println!(
                "[compaction] finished: {} → {} tokens",
                result.tokens_before,
                result.estimated_tokens_after.unwrap_or(0)
            ),
            Err(error) => println!("[compaction] {error}"),
        },
        "/session" => {
            let file = session
                .session_file()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<in-memory>".to_string());
            println!("session {} · {file}", session.session_id());
        }
        "/quit" | "/exit" => return Ok(false),
        other => println!("Unknown command: {other}. Type /help for available commands."),
    }
    Ok(true)
}

/// Run one prompt with the plain projection. Failures are printed by the
/// projection; the returned message is what makes one-shot mode exit non-zero.
fn prompt_once(
    rt: &Runtime,
    session: &Arc<AgentSession>,
    prompt: &str,
    report_errors: bool,
) -> std::result::Result<(), String> {
    let wrote_text = Arc::new(AtomicBool::new(false));
    let subscription = {
        let wrote_text = Arc::clone(&wrote_text);
        session.subscribe(move |event| print_event(event, &wrote_text, report_errors))
    };
    let abort_handle = {
        let session = Arc::clone(session);
        rt.spawn(async move {
            loop {
                if tokio::signal::ctrl_c().await.is_err() {
                    break;
                }
                if session.is_streaming() || session.is_retrying() || session.is_compacting() {
                    session.request_abort();
                    eprintln!("\n[interrupted]");
                }
            }
        })
    };
    let result = rt.block_on(session.prompt(prompt, None));
    abort_handle.abort();
    drop(subscription);
    if wrote_text.load(Ordering::SeqCst) {
        println!();
    }
    result?;
    match last_assistant_failure(&session.messages()) {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

fn print_event(event: &AgentSessionEvent, wrote_text: &AtomicBool, report_errors: bool) {
    match event {
        AgentSessionEvent::MessageUpdate {
            assistant_message_event,
            ..
        } => match assistant_message_event {
            xjpcode_ai::AssistantMessageEvent::TextDelta { delta, .. } => {
                print!("{delta}");
                let _ = io::stdout().flush();
                wrote_text.store(true, Ordering::SeqCst);
            }
            xjpcode_ai::AssistantMessageEvent::ThinkingDelta { .. } => {}
            _ => {}
        },
        AgentSessionEvent::MessageEnd {
            message: AgentMessage::Assistant(assistant),
        } => {
            if !report_errors {
                return;
            }
            match assistant.stop_reason {
                StopReason::Error => eprintln!(
                    "\nerror: {}",
                    assistant.error_message.as_deref().unwrap_or("Unknown error")
                ),
                StopReason::Aborted => eprintln!("\ninterrupted."),
                StopReason::Length => {
                    eprintln!("\nwarning: response was truncated before completion")
                }
                _ => {}
            }
        }
        AgentSessionEvent::TurnEnd {
            message: AgentMessage::Assistant(assistant),
            ..
        } => {
            if assistant.usage.total_tokens > 0 {
                println!(
                    "\n✦ {} in / {} out",
                    assistant.usage.input, assistant.usage.output
                );
            }
        }
        AgentSessionEvent::ToolExecutionStart { tool_name, .. } => {
            println!("\n⏺ {tool_name}");
        }
        AgentSessionEvent::ToolExecutionEnd {
            tool_name,
            result,
            is_error,
            duration_ms,
            ..
        } => {
            let text = tool_result_text(result);
            let elapsed = duration_ms.unwrap_or(0);
            if *is_error {
                println!("  {tool_name} failed ({elapsed} ms):");
            } else {
                println!("  {tool_name} done ({elapsed} ms):");
            }
            println!("{}", indent_block(&text, "    "));
        }
        AgentSessionEvent::AutoRetryStart {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        } => eprintln!(
            "\n[retry] {attempt}/{max_attempts} in {:.1}s: {error_message}",
            *delay_ms as f64 / 1000.0
        ),
        AgentSessionEvent::AutoRetryEnd {
            success,
            attempt,
            final_error,
        } => {
            if *success {
                eprintln!("[retry] succeeded on attempt {attempt}");
            } else {
                eprintln!(
                    "[retry] gave up after {attempt} attempt(s): {}",
                    final_error.as_deref().unwrap_or("Unknown error")
                );
            }
        }
        AgentSessionEvent::CompactionStart { reason } => {
            eprintln!("[compaction] started ({})", reason_name(*reason));
        }
        AgentSessionEvent::CompactionEnd {
            reason,
            result,
            aborted,
            error_message,
            ..
        } => {
            if *aborted {
                eprintln!("[compaction] cancelled ({})", reason_name(*reason));
            } else if let Some(result) = result {
                eprintln!(
                    "[compaction] finished ({}): {} → {} tokens",
                    reason_name(*reason),
                    result.tokens_before,
                    result.estimated_tokens_after.unwrap_or(0)
                );
            } else if let Some(error_message) = error_message {
                eprintln!("[compaction] failed ({}): {error_message}", reason_name(*reason));
            }
        }
        AgentSessionEvent::SummarizationRetryScheduled {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        } => eprintln!(
            "[compaction] summary retry {attempt}/{max_attempts} in {:.1}s: {error_message}",
            *delay_ms as f64 / 1000.0
        ),
        _ => {}
    }
}

fn indent_block(text: &str, indent: &str) -> String {
    text.lines()
        .map(|line| format!("{indent}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// pi `print-mode.ts`: the last assistant turn is error/aborted → report it;
/// length → say the answer was cut off.
fn last_assistant_failure(messages: &[AgentMessage]) -> Option<String> {
    let last = messages.iter().rev().find_map(|message| match message {
        AgentMessage::Assistant(assistant) => Some(assistant),
        _ => None,
    })?;
    match last.stop_reason {
        StopReason::Error | StopReason::Aborted => {
            Some(last.error_message.clone().unwrap_or_else(|| {
                format!(
                    "Request {}",
                    match last.stop_reason {
                        StopReason::Error => "error",
                        StopReason::Aborted => "aborted",
                        _ => unreachable!(),
                    }
                )
            }))
        }
        StopReason::Length => {
            Some("Response was truncated: the model reached its output token limit.".to_string())
        }
        _ => None,
    }
}
