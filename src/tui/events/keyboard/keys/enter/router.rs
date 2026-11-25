use crate::session_setup::SessionChoice;
use crate::tui::events::commands::{self, CommandOutcome};
use crate::tui::events::context::EventContext;
use crate::ui::command;
use crate::ui::picker::PickerKind;
use crate::ui::skills;
use crate::ui::App;
use xjpcode_coding_agent::core::agent_session::StreamingBehavior;

pub enum EnterResult {
    Continue,
    Exit,
}

fn submit_chat(text: String, behavior: StreamingBehavior, app: &mut App, ctx: &mut EventContext) {
    let session = ctx.io.session();
    if session.is_compacting() {
        app.add_system_notice("发送", "正在压缩上下文；等压缩完再发，或按 Esc 取消。");
        return;
    }
    if session.is_streaming() || session.is_retrying() {
        let result = match behavior {
            StreamingBehavior::Steer => session.steer(&text),
            StreamingBehavior::FollowUp => session.follow_up(&text),
        };
        if let Err(error) = result {
            app.add_system_notice("排队失败", error);
        }
        return;
    }
    ctx.io.spawn_prompt(text, app);
}

fn commit_input(app: &mut App) -> Option<String> {
    let text = app.input.trim().to_string();
    if text.is_empty() {
        return None;
    }
    app.remember_input(&text);
    app.take_input();
    Some(text)
}

fn confirm_picker(app: &mut App, ctx: &mut EventContext) {
    let kind = match app.picker_kind() {
        Some(kind) => kind,
        None => return,
    };
    let selected = app.selected_picker_item().map(str::to_string);
    app.close_picker();
    let Some(selected) = selected else {
        return;
    };
    match kind {
        PickerKind::Model => match ctx.io.setup.resolve_model(&selected) {
            Ok(model) => commands::switch_model(app, ctx, model),
            Err(error) => {
                app.add_system_notice("模型切换失败", error);
            }
        },
        PickerKind::Effort => commands::set_effort(app, ctx, &selected),
        PickerKind::Skill => {
            app.input = format!("/skill {selected} ");
            app.cursor_pos = app.input.len();
            app.popup_dismissed = true;
        }
        PickerKind::Resume => match ctx.open_session(app, SessionChoice::Open(selected)) {
            Ok(true) => {}
            Ok(false) => {
                app.add_system_notice("恢复会话", "这场会话是空的，已经打开。");
            }
            Err(error) => {
                app.add_system_notice("恢复会话失败", error);
            }
        },
        PickerKind::Login => commands::start_login(app, ctx, &selected),
        PickerKind::Logout => commands::logout(app, ctx, &selected),
    }
}

pub fn handle_enter(
    app: &mut App,
    ctx: &mut EventContext,
    behavior: StreamingBehavior,
) -> EnterResult {
    if app.has_picker() {
        confirm_picker(app, ctx);
        return EnterResult::Continue;
    }

    // 登录提示独占 Enter：贴在里面的钥匙或回调链接交回登录流程。
    if app.login_prompt.is_some() {
        let value = app
            .login_prompt
            .as_ref()
            .map(|prompt| prompt.input.clone())
            .unwrap_or_default();
        if value.trim().is_empty() {
            if let Some(prompt) = app.login_prompt.as_mut() {
                prompt.notice = Some("贴回授权码或回调链接；不想登录按 Esc。".to_string());
            }
        } else {
            if let Some(login) = &ctx.io.login {
                login.answer(value);
            }
            app.login_prompt = None;
        }
        return EnterResult::Continue;
    }

    // 命令补全弹窗：Enter 选中，不直接执行。
    if let Some(command) = app.selected_command() {
        if app.input != command.name {
            app.apply_selected_command();
            return EnterResult::Continue;
        }
    }

    let Some(user_input) = commit_input(app) else {
        return EnterResult::Continue;
    };

    // pi 的 `/skill:名字` 与 xj 的 `/skill 名字` 都先展开成技能正文。
    let expanded = skills::expand_skill_command(&user_input, ctx.io.setup.skills());
    if expanded != user_input {
        submit_chat(expanded, behavior, app, ctx);
        return EnterResult::Continue;
    }

    if let Some(slash) = command::resolve(&user_input) {
        return match commands::execute(slash, &user_input, app, ctx) {
            CommandOutcome::Continue => EnterResult::Continue,
            CommandOutcome::Exit => EnterResult::Exit,
        };
    }

    submit_chat(user_input, behavior, app, ctx);
    EnterResult::Continue
}
