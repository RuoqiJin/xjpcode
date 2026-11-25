use crate::ui::App;
use crossterm::event::KeyCode;

pub fn handle_popup_navigation(code: KeyCode, app: &mut App) -> bool {
    if !app.has_command_popup() {
        return false;
    }

    // Esc 关弹窗的裁决收归 keyboard/router.rs;此处只管弹窗内的移动/选择。
    match code {
        KeyCode::Up => {
            app.popup_move_up();
            true
        }
        KeyCode::Down => {
            app.popup_move_down();
            true
        }
        KeyCode::Tab => {
            app.apply_selected_command();
            true
        }
        _ => false,
    }
}
