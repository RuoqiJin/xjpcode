//! `xjpcode` 的入口：默认进全屏界面，一句话模式、朴素模式和接着聊各占一个子命令。
//!
//! 界面背后是新内核的 `AgentSession`（`crates/coding-agent`）：规矩与技能、四样工具、
//! 会话文件、接着聊、自动压缩、自动重试、中途插话都在那一层。

use clap::{Parser, Subcommand};
use xjpcode::build_info;
use xjpcode::screens;
use xjpcode::session_setup::{SessionChoice, SessionSetup};

#[derive(Parser)]
#[command(
    version = build_info::CLI_VERSION,
    long_version = build_info::CLI_VERSION,
    about = "xjpcode — Rust CLI AI assistant"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,

    /// DeepSeek model id for a new session (deepseek-flash or deepseek-v4-pro)
    #[arg(long, value_name = "MODEL")]
    model: Option<String>,

    /// Run one prompt in plain output mode and exit
    #[arg(
        value_name = "PROMPT",
        num_args = 1..,
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    prompt: Vec<String>,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Start or inspect the full-screen terminal UI
    Tui {
        #[command(subcommand)]
        command: Option<TuiCommand>,
    },

    /// Start the plain transcript interface without alternate-screen TUI
    Transcript,

    /// Resume a session and continue appending to the same file
    Resume {
        /// Session id (or unique prefix), or a session file; omitted = latest here
        session: Option<String>,

        /// Continue in the plain transcript interface instead of the full-screen TUI
        #[arg(long)]
        transcript: bool,
    },
}

#[derive(Debug, Subcommand)]
enum TuiCommand {
    /// Print terminal capability diagnostics
    Doctor,

    /// Run a short terminal lifecycle smoke test
    Smoke,
}

/// What the interface does for this invocation.
enum Surface {
    Tui,
    Transcript,
    OneShot,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let rt = tokio::runtime::Runtime::new()?;

    if let Some(CliCommand::Tui { command }) = &cli.command {
        match command {
            Some(TuiCommand::Doctor) => {
                screens::tui_doctor::run_doctor();
                return Ok(());
            }
            Some(TuiCommand::Smoke) => {
                screens::tui_doctor::run_smoke()?;
                return Ok(());
            }
            None => {}
        }
    }

    let (choice, surface) = match &cli.command {
        Some(CliCommand::Transcript) => (SessionChoice::New, Surface::Transcript),
        Some(CliCommand::Resume {
            session,
            transcript,
        }) => {
            let choice = match session {
                Some(value) => SessionChoice::Open(value.clone()),
                None => SessionChoice::Continue,
            };
            (
                choice,
                if *transcript {
                    Surface::Transcript
                } else {
                    Surface::Tui
                },
            )
        }
        Some(CliCommand::Tui { .. }) => (SessionChoice::New, Surface::Tui),
        None => {
            if cli.prompt.is_empty() {
                (SessionChoice::New, Surface::Tui)
            } else {
                (SessionChoice::New, Surface::OneShot)
            }
        }
    };

    let mut setup = SessionSetup::load(cli.model.as_deref()).map_err(anyhow::Error::msg)?;
    let opened = setup.open(&choice).map_err(anyhow::Error::msg)?;

    match surface {
        Surface::Tui => screens::repl::run(&rt, setup, opened)?,
        Surface::Transcript => {
            screens::transcript::run_interactive(&rt, &mut setup, &opened.session, opened.resumed)?
        }
        Surface::OneShot => {
            let prompt = cli.prompt.join(" ");
            screens::transcript::run_once(&rt, &opened.session, &prompt)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_supports_plain_prompt() {
        let cli = Cli::try_parse_from(["xjpcode", "hello", "world"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.prompt, vec!["hello", "world"]);
        assert!(cli.model.is_none());
    }

    #[test]
    fn cli_supports_model_for_a_new_session() {
        let cli =
            Cli::try_parse_from(["xjpcode", "--model", "deepseek-v4-pro", "hi"]).unwrap();
        assert_eq!(cli.model.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(cli.prompt, vec!["hi"]);
    }

    #[test]
    fn cli_supports_tui_subcommand() {
        let cli = Cli::try_parse_from(["xjpcode", "tui"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Tui { command: None })
        ));
        assert!(cli.prompt.is_empty());
    }

    #[test]
    fn cli_supports_tui_doctor_subcommand() {
        let cli = Cli::try_parse_from(["xjpcode", "tui", "doctor"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Tui {
                command: Some(TuiCommand::Doctor)
            })
        ));
        assert!(cli.prompt.is_empty());
    }

    #[test]
    fn cli_supports_tui_smoke_subcommand() {
        let cli = Cli::try_parse_from(["xjpcode", "tui", "smoke"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Tui {
                command: Some(TuiCommand::Smoke)
            })
        ));
        assert!(cli.prompt.is_empty());
    }

    #[test]
    fn cli_supports_transcript_subcommand() {
        let cli = Cli::try_parse_from(["xjpcode", "transcript"]).unwrap();
        assert!(matches!(cli.command, Some(CliCommand::Transcript)));
    }

    #[test]
    fn cli_supports_resume_latest_in_tui() {
        let cli = Cli::try_parse_from(["xjpcode", "resume"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Resume {
                session: None,
                transcript: false
            })
        ));
    }

    #[test]
    fn cli_supports_resume_session_in_plain_mode() {
        let cli =
            Cli::try_parse_from(["xjpcode", "resume", "1784471366_b51a", "--transcript"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Resume {
                session: Some(ref id),
                transcript: true
            }) if id == "1784471366_b51a"
        ));
        assert!(cli.prompt.is_empty());
    }

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn cli_version_renders_the_full_build_identity() {
        let rendered = match Cli::try_parse_from(["xjpcode", "--version"]) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("--version should render and exit"),
        };
        assert!(rendered.contains(build_info::CLI_VERSION));
        assert!(rendered.contains(build_info::GIT_SHA));
    }
}
