use super::keys::{
    enter::{handle_enter, EnterResult},
    navigation::handle_navigation,
    typing::handle_typing,
};
use super::router::KeyResult;
use crate::ui::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use xjpcode_coding_agent::core::agent_session::StreamingBehavior;

use crate::tui::events::context::EventContext;

pub fn dispatch_key(key: KeyEvent, app: &mut App, ctx: &mut EventContext) -> KeyResult {
    // 登录提示开着时，键盘只喂给它：钥匙、授权码一个字一个字打进来也要收。
    if app.login_prompt.is_some() {
        match key.code {
            KeyCode::Enter => {
                if let EnterResult::Exit = handle_enter(app, ctx, StreamingBehavior::Steer) {
                    return KeyResult::Exit;
                }
            }
            KeyCode::Backspace => {
                if let Some(prompt) = app.login_prompt.as_mut() {
                    prompt.input.pop();
                    prompt.notice = None;
                }
            }
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(prompt) = app.login_prompt.as_mut() {
                    prompt.input.push(c);
                    prompt.notice = None;
                }
            }
            _ => {}
        }
        return KeyResult::Continue;
    }

    match key.code {
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            if key.modifiers.contains(KeyModifiers::ALT) && is_running(ctx) {
                if let EnterResult::Exit = handle_enter(app, ctx, StreamingBehavior::FollowUp) {
                    return KeyResult::Exit;
                }
            } else {
                app.insert_char('\n');
                app.rearm_on_edit();
            }
        }
        KeyCode::Enter => {
            if let EnterResult::Exit = handle_enter(app, ctx, StreamingBehavior::Steer) {
                return KeyResult::Exit;
            }
        }
        KeyCode::Char('\n') | KeyCode::Char('\r') => {
            app.insert_char('\n');
            app.rearm_on_edit();
        }
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::Left
        | KeyCode::Right
        | KeyCode::Home
        | KeyCode::End
        | KeyCode::Delete
        | KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::Tab => {
            handle_navigation(key.code, app, &mut ctx.io.auto_scroll_bottom);
        }
        KeyCode::Char(_) | KeyCode::Backspace => {
            handle_typing(key, app);
        }
        _ => {}
    }

    KeyResult::Continue
}

pub fn is_running(ctx: &EventContext) -> bool {
    let session = ctx.io.session();
    session.is_streaming()
        || session.is_retrying()
        || session.is_compacting()
        || ctx.io.prompt_task.is_some()
}
