use anyhow::{Context, Result};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::ops::{Deref, DerefMut};
use std::panic;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Once};
use std::time::{SystemTime, UNIX_EPOCH};

pub type TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

static PANIC_RESTORE_HOOK: Once = Once::new();

#[derive(Clone, Copy, Debug)]
pub struct TerminalModeConfig {
    pub mouse_capture: bool,
    pub focus_events: bool,
}

impl Default for TerminalModeConfig {
    fn default() -> Self {
        Self {
            mouse_capture: true,
            focus_events: true,
        }
    }
}

pub struct TerminalGuard {
    terminal: TuiTerminal,
    mouse_capture_enabled: bool,
    focus_events_enabled: bool,
    restored: bool,
    log: TuiDebugLog,
}

impl TerminalGuard {
    pub fn enter(config: TerminalModeConfig) -> Result<Self> {
        install_panic_restore_hook();

        let log = TuiDebugLog::open();
        log.write("enter_start", current_terminal_report().summary_line());

        enable_raw_mode().context("enable terminal raw mode")?;
        if let Err(err) = enter_terminal_modes(config) {
            force_restore_terminal();
            return Err(err);
        }

        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend).context("create ratatui terminal")?;
        terminal.clear().context("clear initial TUI frame")?;

        log.write(
            "enter_done",
            format!(
                "mouse_capture={} focus_events={}",
                config.mouse_capture, config.focus_events
            ),
        );

        Ok(Self {
            terminal,
            mouse_capture_enabled: config.mouse_capture,
            focus_events_enabled: config.focus_events,
            restored: false,
            log,
        })
    }

    pub fn set_mouse_capture(&mut self, enabled: bool) -> Result<()> {
        if enabled == self.mouse_capture_enabled {
            return Ok(());
        }
        if enabled {
            execute!(io::stdout(), EnableMouseCapture).context("enable mouse capture")?;
        } else {
            execute!(io::stdout(), DisableMouseCapture).context("disable mouse capture")?;
        }
        self.mouse_capture_enabled = enabled;
        self.log("mouse_capture", format!("enabled={enabled}"));
        Ok(())
    }

    pub fn log(&self, event: impl AsRef<str>, detail: impl AsRef<str>) {
        self.log.write(event, detail);
    }

    pub fn restore(&mut self) {
        if self.restored {
            return;
        }
        self.restored = true;
        self.log.write("restore_start", "");
        let _ = self.terminal.show_cursor();
        force_restore_terminal();
        self.log.write(
            "restore_done",
            format!(
                "mouse_capture={} focus_events={}",
                self.mouse_capture_enabled, self.focus_events_enabled
            ),
        );
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

impl Deref for TerminalGuard {
    type Target = TuiTerminal;

    fn deref(&self) -> &Self::Target {
        &self.terminal
    }
}

impl DerefMut for TerminalGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.terminal
    }
}

fn enter_terminal_modes(config: TerminalModeConfig) -> Result<()> {
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        Hide,
        EnableBracketedPaste
    )
    .context("enter alternate screen and enable bracketed paste")?;
    if config.focus_events {
        execute!(io::stdout(), EnableFocusChange).context("enable focus events")?;
    }
    if config.mouse_capture {
        execute!(io::stdout(), EnableMouseCapture).context("enable mouse capture")?;
    }
    Ok(())
}

fn install_panic_restore_hook() {
    PANIC_RESTORE_HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |panic_info| {
            force_restore_terminal();
            previous(panic_info);
        }));
    });
}

pub fn force_restore_terminal() {
    let _ = execute!(
        io::stdout(),
        DisableMouseCapture,
        DisableFocusChange,
        DisableBracketedPaste,
        Show,
        LeaveAlternateScreen
    );
    let _ = disable_raw_mode();
}

#[derive(Clone)]
pub struct TuiDebugLog {
    file: Option<Arc<Mutex<File>>>,
}

impl TuiDebugLog {
    fn open() -> Self {
        let path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".xjpcode")
            .join("logs")
            .join("tui.log");
        let file = path.parent().and_then(|parent| {
            std::fs::create_dir_all(parent).ok()?;
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .ok()
        });
        Self {
            file: file.map(|f| Arc::new(Mutex::new(f))),
        }
    }

    pub fn write(&self, event: impl AsRef<str>, detail: impl AsRef<str>) {
        let Some(file) = &self.file else {
            return;
        };
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        let line = format!(
            "{} event={} {}\n",
            now_ms,
            sanitize_log_field(event.as_ref()),
            sanitize_log_field(detail.as_ref())
        );
        if let Ok(mut file) = file.lock() {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

fn sanitize_log_field(value: &str) -> String {
    value.replace(['\n', '\r', '\t'], " ")
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TerminalReport {
    pub name: String,
    pub term: Option<String>,
    pub term_program: Option<String>,
    pub term_program_version: Option<String>,
    pub multiplexer: Option<String>,
    pub capabilities: TerminalCapabilities,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TerminalCapabilities {
    pub alternate_screen: bool,
    pub focus_events: bool,
    pub mouse_capture: bool,
    pub osc52_clipboard: bool,
    pub bracketed_paste: bool,
}

impl TerminalReport {
    pub fn summary_line(&self) -> String {
        format!(
            "name={} term={:?} term_program={:?} multiplexer={:?} alt={} focus={} mouse={} osc52={} bracketed_paste={}",
            self.name,
            self.term,
            self.term_program,
            self.multiplexer,
            self.capabilities.alternate_screen,
            self.capabilities.focus_events,
            self.capabilities.mouse_capture,
            self.capabilities.osc52_clipboard,
            self.capabilities.bracketed_paste
        )
    }
}

pub fn current_terminal_report() -> TerminalReport {
    detect_terminal_report(|key| std::env::var(key).ok())
}

fn detect_terminal_report(mut env: impl FnMut(&str) -> Option<String>) -> TerminalReport {
    let term = non_empty(env("TERM"));
    let term_program = non_empty(env("TERM_PROGRAM"));
    let term_program_version = non_empty(env("TERM_PROGRAM_VERSION"));
    let multiplexer = if env("TMUX").is_some() {
        Some("tmux".to_string())
    } else if env("ZELLIJ").is_some() || env("ZELLIJ_SESSION_NAME").is_some() {
        Some("zellij".to_string())
    } else {
        None
    };

    let name = if let Some(program) = term_program.as_deref() {
        match program.to_ascii_lowercase().as_str() {
            "apple_terminal" | "appleterminal" => "Apple Terminal",
            "iterm.app" | "iterm2" => "iTerm2",
            "vscode" => "VS Code",
            "warpterminal" | "warp" => "Warp",
            "wezterm" => "WezTerm",
            other => other,
        }
        .to_string()
    } else if env("WEZTERM_VERSION").is_some() {
        "WezTerm".to_string()
    } else if env("KITTY_WINDOW_ID").is_some() {
        "kitty".to_string()
    } else if env("ITERM_SESSION_ID").is_some() {
        "iTerm2".to_string()
    } else if env("TERM_SESSION_ID").is_some() {
        "Apple Terminal".to_string()
    } else if env("VSCODE_PID").is_some() {
        "VS Code".to_string()
    } else if term.as_deref() == Some("dumb") {
        "dumb".to_string()
    } else {
        "unknown".to_string()
    };

    let is_dumb = term.as_deref() == Some("dumb") || name == "dumb";
    let mut notes = Vec::new();
    if is_dumb {
        notes.push("TERM=dumb: full-screen TUI features are disabled or unreliable".to_string());
    }
    if multiplexer.as_deref() == Some("tmux") {
        notes.push("tmux: OSC52 clipboard may require set-clipboard/allow-passthrough".to_string());
    }
    if name == "Apple Terminal" {
        notes.push("Apple Terminal: prefer pbcopy fallback for clipboard".to_string());
    }

    TerminalReport {
        name,
        term,
        term_program,
        term_program_version,
        multiplexer,
        capabilities: TerminalCapabilities {
            alternate_screen: !is_dumb,
            focus_events: !is_dumb,
            mouse_capture: !is_dumb,
            osc52_clipboard: !is_dumb,
            bracketed_paste: !is_dumb,
        },
        notes,
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

#[cfg(test)]
pub(crate) fn detect_terminal_report_from_pairs(pairs: &[(&str, &str)]) -> TerminalReport {
    use std::collections::BTreeMap;

    let map: BTreeMap<&str, &str> = pairs.iter().copied().collect();
    detect_terminal_report(|key| map.get(key).map(|value| (*value).to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_codex_desktop_like_vscode_terminal() {
        let report = detect_terminal_report_from_pairs(&[
            ("TERM_PROGRAM", "vscode"),
            ("TERM", "xterm-256color"),
        ]);
        assert_eq!(report.name, "VS Code");
        assert!(report.capabilities.alternate_screen);
        assert!(report.capabilities.focus_events);
        assert!(report.capabilities.bracketed_paste);
    }

    #[test]
    fn detects_dumb_terminal_as_unsafe_for_tui() {
        let report = detect_terminal_report_from_pairs(&[("TERM", "dumb")]);
        assert_eq!(report.name, "dumb");
        assert!(!report.capabilities.alternate_screen);
        assert!(!report.capabilities.mouse_capture);
        assert!(!report.capabilities.bracketed_paste);
    }

    #[test]
    fn reports_tmux_clipboard_note() {
        let report = detect_terminal_report_from_pairs(&[
            ("TERM", "screen-256color"),
            ("TMUX", "/tmp/tmux-501/default,123,0"),
        ]);
        assert_eq!(report.multiplexer.as_deref(), Some("tmux"));
        assert!(report.notes.iter().any(|note| note.contains("OSC52")));
    }
}
