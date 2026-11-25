use crate::screens::terminal::TuiTerminal;
use crate::tui::events::context::EventContext;
use crate::tui::render::render_ui;
use crate::ui::App;
use std::time::{Duration, Instant};

/// 渲染帧预算：重绘上限 60FPS。事件合并与动画帧共用此值。
pub const FRAME_BUDGET: Duration = Duration::from_millis(16);

pub fn step(
    terminal: &mut TuiTerminal,
    app: &mut App,
    ctx: &mut EventContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let status_h = app.spinner.status_bar_height();
    let chrome_h = crate::tui::render::chrome_height(app);
    let content_height = terminal.size()?.height.saturating_sub(
        chrome_h + crate::tui::render::components::content::router::BORDER_ROWS + status_h,
    );
    app.scroll.set_viewport(content_height);
    let max_offset = if ctx.total_rendered_lines > content_height {
        ctx.total_rendered_lines.saturating_sub(content_height) as f32
    } else {
        0.0
    };

    let is_animating = app.scroll.update_animation(max_offset);

    let draw_due = ctx
        .last_draw
        .is_none_or(|last| last.elapsed() >= FRAME_BUDGET);
    if (ctx.needs_redraw || is_animating) && draw_due {
        terminal.draw(|frame| {
            ctx.total_rendered_lines = render_ui(frame, app);
        })?;
        ctx.last_draw = Some(Instant::now());
        ctx.needs_redraw = false;

        if ctx.io.auto_scroll_bottom {
            app.scroll.target = max_offset;
        }
    }

    Ok(())
}
