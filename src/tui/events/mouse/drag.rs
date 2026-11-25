use crate::tui::render::components::content::extractor::extract_selected_text;
use crate::ui::clipboard::copy_to_clipboard;
use crate::ui::App;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

pub fn handle_mouse_drag(event: MouseEvent, app: &mut App) {
    let (col, row) = (event.column, event.row);

    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.selection.start(col, row);
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            app.selection.drag(col, row);
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if !app.selection.is_selecting {
                // 单纯点击：点状态栏复制会话编号；点卡片 ▸/▾ 折叠。
                let is_click_status_bar = app
                    .status_bar_area
                    .is_some_and(|area| row >= area.y && row < area.y + area.height);

                if is_click_status_bar {
                    if !app.session_id.is_empty() && copy_to_clipboard(&app.session_id) {
                        app.show_toast(format!("已复制会话编号 ({})", app.session_id));
                    }
                } else {
                    app.toggle_block_by_y(row, app.scroll.offset);
                }
            } else if let (Some(range), Some(area)) =
                (app.selection.get_normalized_range(), app.content_area)
            {
                // 从最近一帧渲染快照按显示列宽提取真实选中文本（与高亮同源）。
                let text =
                    extract_selected_text(&app.rendered_text_lines, area, app.scroll.offset, range);
                if copy_to_clipboard(&text) {
                    app.show_toast("已复制选中文本到剪贴板");
                }
            }
            app.selection.clear();
        }
        _ => {}
    }
}
