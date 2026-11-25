use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

pub fn check_interrupt(key: &KeyEvent) -> bool {
    if key.kind == KeyEventKind::Release {
        return false;
    }
    key.modifiers.contains(KeyModifiers::CONTROL)
        && (key.code == KeyCode::Char('c') || key.code == KeyCode::Char('C'))
}
