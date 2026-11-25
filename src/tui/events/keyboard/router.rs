use super::dispatch;
use super::interrupt;
use crate::tui::events::context::EventContext;
use crate::ui::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};

pub enum KeyResult {
    Continue,
    Exit,
}

/// 空闲状态下双击 Ctrl+C 退出的确认窗口。
const CTRL_C_EXIT_WINDOW: Duration = Duration::from_secs(2);

fn running(ctx: &EventContext) -> bool {
    let session = ctx.io.session();
    session.is_streaming()
        || session.is_retrying()
        || session.is_compacting()
        || ctx.io.prompt_task.is_some()
        || ctx.io.compaction_task.is_some()
}

fn cancel(app: &mut App, ctx: &mut EventContext, message: &str) {
    // 先让会话停下，再把还没投递的排队收回输入框；顺序反过来会把已经投递的那句也收回来。
    ctx.io.session().request_abort();
    let restored = ctx.io.restore_queued(app);
    if let Some(login) = &ctx.io.login {
        login.cancel();
    }
    ctx.io.login = None;
    app.clear_login();
    // 不在这里 abort 运行时任务：工具要自己收尾、把子进程停掉。等它报 settled。
    app.spinner.set_verb("正在打断...");
    if restored > 0 {
        app.show_toast(format!("已打断；{restored} 条排队的话已收回输入框"));
    } else {
        app.show_toast(message);
    }
}

pub fn handle_keyboard_event(key: KeyEvent, app: &mut App, ctx: &mut EventContext) -> KeyResult {
    // Ctrl+C：进行中 → 打断并收回排队的话；空闲 → 双击（2 秒窗口）防误触退出。
    if interrupt::check_interrupt(&key) {
        if running(ctx) {
            cancel(app, ctx, "已打断当前任务");
            app.last_ctrl_c = None;
            return KeyResult::Continue;
        }
        let now = Instant::now();
        if let Some(last_time) = app.last_ctrl_c {
            if now.duration_since(last_time) <= CTRL_C_EXIT_WINDOW {
                return KeyResult::Exit;
            }
        }
        app.last_ctrl_c = Some(now);
        app.show_toast("再按一次 Ctrl+C 退出程序");
        return KeyResult::Continue;
    }

    // Esc：关登录提示 > 关选择菜单 > 关命令弹窗 > 打断进行中的回复。
    if key.code == KeyCode::Esc {
        if app.login_prompt.is_some() {
            if let Some(login) = &ctx.io.login {
                login.cancel();
            }
            ctx.io.login = None;
            app.clear_login();
            app.add_system_notice("登录", "已取消登录。");
        } else if app.has_picker() {
            app.close_picker();
        } else if app.has_command_popup() {
            app.dismiss_popup();
        } else if running(ctx) {
            cancel(app, ctx, "已打断当前任务");
        }
        return KeyResult::Continue;
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('d') | KeyCode::Char('D') => return KeyResult::Exit,
            KeyCode::Char('l') | KeyCode::Char('L') => {
                ctx.force_clear = true;
                return KeyResult::Continue;
            }
            _ => {}
        }
    }

    dispatch::dispatch_key(key, app, ctx)
}
