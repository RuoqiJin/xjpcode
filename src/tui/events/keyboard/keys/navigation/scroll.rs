use crate::ui::App;
use crossterm::event::KeyCode;

pub fn handle_scroll(code: KeyCode, app: &mut App, auto_scroll_bottom: &mut bool) {
    match code {
        KeyCode::PageUp => {
            *auto_scroll_bottom = false;
            let step = app.scroll.page_step();
            app.scroll.nudge(-step);
        }
        KeyCode::PageDown => {
            let step = app.scroll.page_step();
            app.scroll.nudge(step);
        }
        _ => {}
    }
}
