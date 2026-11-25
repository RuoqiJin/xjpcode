use crate::screens::terminal::{current_terminal_report, TerminalGuard, TerminalModeConfig};
use anyhow::Result;
use ratatui::widgets::{Block, Borders, Paragraph};
use std::time::Duration;

pub fn run_doctor() {
    let report = current_terminal_report();
    println!("xjpcode TUI doctor");
    println!("terminal: {}", report.name);
    println!("TERM: {}", report.term.as_deref().unwrap_or("<unset>"));
    println!(
        "TERM_PROGRAM: {}",
        report.term_program.as_deref().unwrap_or("<unset>")
    );
    println!(
        "TERM_PROGRAM_VERSION: {}",
        report.term_program_version.as_deref().unwrap_or("<unset>")
    );
    println!(
        "multiplexer: {}",
        report.multiplexer.as_deref().unwrap_or("<none>")
    );
    println!();
    println!("capabilities:");
    println!(
        "  alternate_screen: {}",
        yes_no(report.capabilities.alternate_screen)
    );
    println!(
        "  focus_events: {}",
        yes_no(report.capabilities.focus_events)
    );
    println!(
        "  mouse_capture: {}",
        yes_no(report.capabilities.mouse_capture)
    );
    println!(
        "  osc52_clipboard: {}",
        yes_no(report.capabilities.osc52_clipboard)
    );
    println!(
        "  bracketed_paste: {}",
        yes_no(report.capabilities.bracketed_paste)
    );
    if !report.notes.is_empty() {
        println!();
        println!("notes:");
        for note in report.notes {
            println!("  - {note}");
        }
    }
    let log_path = dirs::home_dir()
        .map(|home| home.join(".xjpcode").join("logs").join("tui.log"))
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<unknown>".to_string());
    println!();
    println!("debug log: {log_path}");
    println!("smoke test: xjpcode tui smoke");
}

pub fn run_smoke() -> Result<()> {
    let mut terminal = TerminalGuard::enter(TerminalModeConfig::default())?;
    terminal.log("smoke_draw", "start");
    terminal.draw(|frame| {
        let area = frame.area();
        let paragraph = Paragraph::new("xjpcode tui smoke\nterminal lifecycle guard OK")
            .block(Block::default().title("xjpcode").borders(Borders::ALL));
        frame.render_widget(paragraph, area);
    })?;
    std::thread::sleep(Duration::from_millis(80));
    terminal.log("smoke_draw", "done");
    terminal.restore();
    println!("xjpcode tui smoke ok");
    Ok(())
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}
