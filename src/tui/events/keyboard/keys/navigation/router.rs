use super::history;
use super::picker_nav;
use super::popup_nav;
use super::scroll;
use crate::ui::App;
use crossterm::event::KeyCode;

pub fn handle_navigation(code: KeyCode, app: &mut App, auto_scroll_bottom: &mut bool) {
    if picker_nav::handle_picker_navigation(code, app) {
        return;
    }

    if popup_nav::handle_popup_navigation(code, app) {
        return;
    }

    match code {
        KeyCode::Up | KeyCode::Down => history::handle_history(code, app),
        KeyCode::PageUp | KeyCode::PageDown => scroll::handle_scroll(code, app, auto_scroll_bottom),
        KeyCode::Left => app.cursor_left(),
        KeyCode::Right => app.cursor_right(),
        KeyCode::Home => app.cursor_home(),
        KeyCode::End => app.cursor_end(),
        KeyCode::Delete => app.delete_char(),
        _ => {}
    }
}
