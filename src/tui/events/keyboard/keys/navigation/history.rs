use crate::ui::App;
use crossterm::event::KeyCode;

pub fn handle_history(code: KeyCode, app: &mut App) {
    match code {
        KeyCode::Up => app.history_prev(),
        KeyCode::Down => app.history_next(),
        _ => {}
    }
}
