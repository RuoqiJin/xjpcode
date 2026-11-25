use crate::ui::App;

pub fn handle_backspace(app: &mut App) {
    app.backspace();
    app.popup_dismissed = false;
}
