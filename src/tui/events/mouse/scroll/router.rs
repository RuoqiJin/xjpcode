use crate::ui::App;
use crossterm::event::MouseEventKind;

pub fn handle_scroll(kind: MouseEventKind, app: &mut App, auto_scroll_bottom: &mut bool) {
    match kind {
        MouseEventKind::ScrollUp => {
            *auto_scroll_bottom = false;
            app.scroll.track_velocity(-1.0);
            let step = app.scroll.adaptive_step();
            app.scroll.nudge(-step);
        }
        MouseEventKind::ScrollDown => {
            app.scroll.track_velocity(1.0);
            let step = app.scroll.adaptive_step();
            app.scroll.nudge(step);
        }
        _ => {}
    }
}
