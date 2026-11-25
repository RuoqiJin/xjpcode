use crate::ui::App;

pub fn handle_char(c: char, app: &mut App) {
    app.insert_char(c);
    app.rearm_on_edit();
}
