use crate::ui::App;
use crossterm::event::KeyCode;

/// 模态选择菜单激活时的导航键拦截（Up/Down 循环移动选中项；Esc 按键在 keyboard/router.rs 中统一裁决独占处理）
pub fn handle_picker_navigation(code: KeyCode, app: &mut App) -> bool {
    if !app.has_picker() {
        return false;
    }

    match code {
        KeyCode::Up => {
            app.picker_move_up();
            true
        }
        KeyCode::Down => {
            app.picker_move_down();
            true
        }
        _ => false,
    }
}
