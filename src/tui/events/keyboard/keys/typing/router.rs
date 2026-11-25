use super::backspace;
use super::char_input;
use super::clear_line;
use crate::ui::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn handle_typing(key: KeyEvent, app: &mut App) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('u') {
        clear_line::handle_clear_line(app);
        return;
    }

    match key.code {
        KeyCode::Char(c) => char_input::handle_char(c, app),
        KeyCode::Backspace => backspace::handle_backspace(app),
        _ => {}
    }
}
