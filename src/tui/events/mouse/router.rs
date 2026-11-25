use super::drag;
use super::scroll;
use crate::ui::App;
use crossterm::event::{MouseEvent, MouseEventKind};

pub fn handle_mouse_event(mouse: MouseEvent, app: &mut App, auto_scroll_bottom: &mut bool) {
    match mouse.kind {
        MouseEventKind::Down(_) | MouseEventKind::Drag(_) | MouseEventKind::Up(_) => {
            drag::handle_mouse_drag(mouse, app);
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            scroll::handle_scroll(mouse.kind, app, auto_scroll_bottom);
        }
        _ => {}
    }
}
