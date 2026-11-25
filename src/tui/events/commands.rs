//! 斜杠命令的落地：xj 的命令表 + xjpcode 自己的登录/会话命令，接到 `AgentSession` 上。

use xjpcode_ai::auth::AuthType;
use xjpcode_ai::Model;
use xjpcode_coding_agent::core::model_setup::{
    describe_auth_error, provider_label, thinking_level_from_name, thinking_level_name,
    DEEPSEEK_PROVIDER, OPENAI_CODEX_PROVIDER,
};

use crate::login::LoginHandle;
use crate::session_setup::SessionChoice;
use crate::ui::command::{self, CommandKind, SlashCommand};
use crate::ui::effort_picker::build_effort_picker;
use crate::ui::model_picker::build_model_picker;
use crate::ui::picker::{PickerItem, PickerKind, PickerState};
use crate::ui::session_info;
use crate::ui::sessions::{build_session_picker, list_sessions};
use crate::ui::skills;
use crate::ui::App;

use super::context::EventContext;

pub enum CommandOutcome {
    Continue,
    Exit,
}

fn busy(ctx: &EventContext) -> bool {
    let session = ctx.io.session();
    session.is_streaming() || session.is_retrying() || session.is_compacting()
}

fn busy_notice(app: &mut App, action: &str) {
    app.show_toast(format!("{action}：等这一轮跑完，或按 Esc 打断"));
}

fn provider_args(arg: &str) -> Option<&'static str> {
    match arg.trim().to_ascii_lowercase().as_str() {
        "deepseek" | "deepseek-flash" | "deepseek-v4-pro" => Some(DEEPSEEK_PROVIDER),
        "openai-codex" | "codex" | "gpt" | "openai" => Some(OPENAI_CODEX_PROVIDER),
        _ => None,
    }
}

pub fn execute(
    command: &SlashCommand,
    prompt: &str,
    app: &mut App,
    ctx: &mut EventContext,
) -> CommandOutcome {
    match command.kind {
        CommandKind::Exit => return CommandOutcome::Exit,
        CommandKind::Model => handle_model(prompt, app, ctx),
        CommandKind::Effort | CommandKind::Thinking => handle_effort(prompt, app, ctx),
        CommandKind::Compact => handle_compact(app, ctx),
        CommandKind::Clear => {
            if busy(ctx) {
                busy_notice(app, "清空");
                return CommandOutcome::Continue;
            }
            if let Err(error) = ctx.open_session(app, SessionChoice::New) {
                app.add_system_notice("新会话失败", error);
            } else {
                app.add_system_notice("已清除", "已清空屏幕并开一场新会话。");
                app.show_toast("已清除上下文并清屏");
            }
        }
        CommandKind::New => {
            if let Err(error) = ctx.open_session(app, SessionChoice::New) {
                app.add_system_notice("新会话失败", error);
            } else {
                app.add_system_notice("新会话", format!("已开一场新会话 [{}]。", app.session_id));
            }
        }
        CommandKind::Resume => handle_resume(prompt, app, ctx),
        CommandKind::Skill => handle_skill(prompt, app, ctx),
        CommandKind::Login => handle_login(prompt, app, ctx),
        CommandKind::Logout => handle_logout(prompt, app, ctx),
        CommandKind::Session => {
            let file = app
                .session_file
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<内存中>".to_string());
            app.add_system_notice(
                "当前会话",
                format!("编号: {}\n文件: {file}", app.session_id),
            );
        }
        CommandKind::Copy => handle_copy(prompt, app),
        CommandKind::Mouse => handle_mouse(prompt, app),
        CommandKind::Tools => {
            let tools = ctx.io.session().agent().state().tools;
            if tools.is_empty() {
                app.add_system_notice("工具", "没有可用的工具。");
            } else {
                let mut text = String::new();
                for tool in &tools {
                    let declaration = tool.declaration();
                    let summary = declaration
                        .description
                        .lines()
                        .find(|line| !line.trim().is_empty())
                        .unwrap_or("");
                    text.push_str(&format!("  {}  {}\n", declaration.name, summary));
                }
                app.add_system_notice("可用工具", text.trim_end().to_string());
            }
        }
        CommandKind::Help => {
            let body = command::ALL_COMMANDS
                .iter()
                .map(|command| {
                    let aliases = if command.aliases.is_empty() {
                        String::new()
                    } else {
                        format!("（{}）", command.aliases.join("、"))
                    };
                    format!("{}{} - {}", command.name, aliases, command.description)
                })
                .collect::<Vec<_>>()
                .join("\n");
            app.add_system_notice(
                "快捷帮助",
                format!(
                    "{body}\n\n空格: 输入\nEnter: 发送；跑着时插话\nAlt+Enter: 跑着时排队（停下后再发）\nShift+Enter: 换行\nEsc: 关菜单 / 打断\nCtrl+C: 打断；空闲时连按两次退出\nPageUp/PageDown: 翻页\n鼠标拖选自动复制；点 ▸/▾ 折叠卡片"
                ),
            );
        }
    }
    CommandOutcome::Continue
}

fn available_models(ctx: &EventContext) -> Result<Vec<Model>, String> {
    ctx.block_on(ctx.io.models.get_available(None))
        .map_err(|error| describe_auth_error(&error))
}

fn handle_model(prompt: &str, app: &mut App, ctx: &mut EventContext) {
    let models = match available_models(ctx) {
        Ok(models) => models,
        Err(error) => {
            app.add_system_notice("模型", error);
            return;
        }
    };
    if models.is_empty() {
        app.add_system_notice(
            "模型",
            "还没有可用的模型。先用 /login 登录 DeepSeek 或 GPT 订阅。",
        );
        return;
    }
    match command::first_arg(prompt) {
        None => {
            let current = ctx.io.session().model();
            app.open_picker(build_model_picker(&models, &current.provider, &current.id));
        }
        Some(arg) => {
            let target = if let Ok(index) = arg.parse::<usize>() {
                if index > 0 && index <= models.len() {
                    Some(models[index - 1].clone())
                } else {
                    None
                }
            } else {
                ctx.io.setup.resolve_model(arg).ok().filter(|model| {
                    models.iter().any(|candidate| {
                        candidate.provider == model.provider && candidate.id == model.id
                    })
                })
            };
            match target {
                Some(model) => switch_model(app, ctx, model),
                None => {
                    let list = models
                        .iter()
                        .enumerate()
                        .map(|(index, model)| {
                            format!("{}. {}/{}", index + 1, model.provider, model.id)
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    app.add_system_notice(
                        "模型切换说明",
                        format!(
                            "未找到模型 \"{arg}\"。可用的模型：\n{list}\n\n输入 /model 可以弹出选择菜单。"
                        ),
                    );
                }
            }
        }
    }
}

/// 换模型前先看窗口：上下文比新窗口大就不换，先压缩。owner 要的、比 pi 多做的一步。
pub fn model_window_too_small(context_tokens: u64, context_window: u64) -> bool {
    context_window > 0 && context_tokens > context_window
}

pub fn switch_model(app: &mut App, ctx: &mut EventContext, model: Model) {
    let current_tokens = app.context_prompt_tokens as u64;
    if model_window_too_small(current_tokens, model.context_window) {
        app.add_system_notice(
            "换模型前先说一声",
            format!(
                "当前上下文约 {} tokens，超过 {}/{} 的窗口 {}。\n先 /compact 压缩，再换到这个模型。",
                current_tokens,
                model.provider,
                model.id,
                crate::format::numbers::humanize_count(model.context_window)
            ),
        );
        return;
    }
    match ctx.io.session().set_model(model.clone()) {
        Ok(()) => {
            ctx.io.setup.remember_model(model.clone());
            session_info::sync_session(app, ctx.io.session());
            app.add_system_notice(
                "切换模型",
                format!(
                    "已切换为 {}/{}（Model: `{}`，思考档 {}）",
                    model.provider, model.id, model.name, app.thinking
                ),
            );
        }
        Err(error) => app.add_system_notice("切换模型失败", error),
    }
}

fn effort_levels(ctx: &EventContext) -> Vec<String> {
    ctx.io
        .session()
        .available_thinking_levels()
        .into_iter()
        .map(|level| thinking_level_name(level).to_string())
        .collect()
}

fn handle_effort(prompt: &str, app: &mut App, ctx: &mut EventContext) {
    let levels = effort_levels(ctx);
    match command::first_arg(prompt) {
        None => {
            if levels.is_empty() {
                app.add_system_notice("思考档位", "当前模型没有可选的思考档位。");
            } else {
                let current = thinking_level_name(ctx.io.session().thinking_level());
                app.open_picker(build_effort_picker(&levels, current));
            }
        }
        Some(level) => {
            let level = level.to_ascii_lowercase();
            if !levels.iter().any(|candidate| candidate == &level) {
                app.add_system_notice(
                    "思考档位说明",
                    format!(
                        "未知思考档位 \"{level}\"。当前模型可用：{}\n\n输入 /effort 可以弹出选择菜单。",
                        levels.join("、")
                    ),
                );
                return;
            }
            set_effort(app, ctx, &level);
        }
    }
}

pub fn set_effort(app: &mut App, ctx: &mut EventContext, level: &str) {
    let level = thinking_level_from_name(level);
    match ctx.io.session().set_thinking_level(level) {
        Ok(Some(effective)) => {
            session_info::sync_session(app, ctx.io.session());
            app.add_system_notice(
                "思考档位",
                format!("已将思考档位切换为 {}", thinking_level_name(effective)),
            );
        }
        Ok(None) => app.add_system_notice(
            "思考档位",
            format!(
                "思考档位已经是 {}。当前模型支持：{}",
                thinking_level_name(ctx.io.session().thinking_level()),
                effort_levels(ctx).join("、")
            ),
        ),
        Err(error) => app.add_system_notice("切换思考档位失败", error),
    }
}

fn handle_compact(app: &mut App, ctx: &mut EventContext) {
    if busy(ctx) {
        busy_notice(app, "压缩");
        return;
    }
    ctx.io.spawn_compaction(app);
}

fn handle_resume(prompt: &str, app: &mut App, ctx: &mut EventContext) {
    if busy(ctx) {
        busy_notice(app, "恢复会话");
        return;
    }
    if let Some(target) = command::first_arg(prompt) {
        match ctx.open_session(app, SessionChoice::Open(target.to_string())) {
            Ok(true) => {}
            Ok(false) => app.add_system_notice("恢复会话", "这场会话是空的，已经打开。"),
            Err(error) => app.add_system_notice("恢复会话失败", error),
        }
        return;
    }
    let sessions = list_sessions(ctx.io.setup.session_dir());
    let Some(picker) = build_session_picker(&sessions, &app.session_id) else {
        app.add_system_notice("恢复会话", "这个目录里还没有历史会话。");
        return;
    };
    app.open_picker(picker);
}

fn handle_skill(prompt: &str, app: &mut App, ctx: &mut EventContext) {
    let skills = ctx.io.setup.skills();
    let trimmed = prompt.trim_start();
    let after_command = trimmed
        .split_once(char::is_whitespace)
        .map(|(_, rest)| rest.trim())
        .unwrap_or("");
    if after_command.is_empty() {
        if skills.is_empty() {
            app.add_system_notice("可用的技能", skills::list_text(skills));
            return;
        }
        let items = skills
            .iter()
            .map(|skill| PickerItem {
                id: skill.name.clone(),
                label: skill.name.clone(),
                detail: skill.description.clone(),
                active: false,
            })
            .collect();
        app.open_picker(PickerState {
            kind: PickerKind::Skill,
            title: "选择技能 (↑/↓ 切换，Enter 填入输入框)",
            items,
            index: 0,
        });
        return;
    }
    let (name, args) = match after_command.find(char::is_whitespace) {
        Some(index) => (&after_command[..index], after_command[index..].trim()),
        None => (after_command, ""),
    };
    match skills::expand_named(skills, name, args) {
        Ok(Some(text)) => submit_chat(text, app, ctx),
        Ok(None) => app.add_system_notice(
            "技能未找到",
            format!(
                "未找到名称为 \"{name}\" 的技能。\n\n{}",
                skills::list_text(skills)
            ),
        ),
        Err(error) => app.add_system_notice("技能读取失败", error),
    }
}

fn handle_login(prompt: &str, app: &mut App, ctx: &mut EventContext) {
    if let Some(arg) = command::first_arg(prompt) {
        match provider_args(arg) {
            Some(provider) => start_login(app, ctx, provider),
            None => app.add_system_notice(
                "登录",
                format!("/login: 只认 deepseek 或 openai-codex，不认 \"{arg}\"。"),
            ),
        }
        return;
    }
    let mut items = Vec::new();
    for provider in [DEEPSEEK_PROVIDER, OPENAI_CODEX_PROVIDER] {
        let detail = match ctx.block_on(ctx.io.models.check_auth(provider)) {
            Ok(Some(check)) => check.source.unwrap_or_else(|| "已配置".to_string()),
            _ => "未配置".to_string(),
        };
        items.push(PickerItem {
            id: provider.to_string(),
            label: provider_label(&ctx.io.models, provider),
            detail,
            active: false,
        });
    }
    app.open_picker(PickerState {
        kind: PickerKind::Login,
        title: "登录哪一家 (↑/↓ 切换，Enter 确认)",
        items,
        index: 0,
    });
}

pub fn start_login(app: &mut App, ctx: &mut EventContext, provider_id: &str) {
    if ctx.io.login.is_some() {
        app.add_system_notice("登录", "已经有一个登录在进行中（Esc 取消）。");
        return;
    }
    app.clear_login();
    app.add_system_notice(
        "登录",
        format!("正在登录 {}…", provider_label(&ctx.io.models, provider_id)),
    );
    let auth_type = if provider_id == OPENAI_CODEX_PROVIDER {
        AuthType::OAuth
    } else {
        AuthType::ApiKey
    };
    ctx.io.login = Some(LoginHandle::start(
        &ctx.io.handle,
        std::sync::Arc::clone(&ctx.io.models),
        provider_id,
        auth_type,
    ));
}

fn handle_logout(prompt: &str, app: &mut App, ctx: &mut EventContext) {
    if let Some(arg) = command::first_arg(prompt) {
        match provider_args(arg) {
            Some(provider) => logout(app, ctx, provider),
            None => app.add_system_notice(
                "退出登录",
                format!("/logout: 只认 deepseek 或 openai-codex，不认 \"{arg}\"。"),
            ),
        }
        return;
    }
    let credentials = ctx.block_on(ctx.io.models.credentials().list(Default::default()));
    match credentials {
        Ok(credentials) => {
            let items: Vec<PickerItem> = credentials
                .into_iter()
                .filter(|credential| {
                    credential.provider_id == DEEPSEEK_PROVIDER
                        || credential.provider_id == OPENAI_CODEX_PROVIDER
                })
                .map(|credential| PickerItem {
                    label: provider_label(&ctx.io.models, &credential.provider_id),
                    id: credential.provider_id,
                    detail: credential.r#type.as_str().to_string(),
                    active: false,
                })
                .collect();
            if items.is_empty() {
                app.add_system_notice("退出登录", "没有保存的钥匙或登录。");
            } else {
                app.open_picker(PickerState {
                    kind: PickerKind::Logout,
                    title: "删除哪一家的登录 (↑/↓ 切换，Enter 确认)",
                    items,
                    index: 0,
                });
            }
        }
        Err(error) => app.add_system_notice("退出登录", describe_auth_error(&error)),
    }
}

pub fn logout(app: &mut App, ctx: &mut EventContext, provider_id: &str) {
    match ctx.block_on(ctx.io.models.logout(provider_id)) {
        Ok(()) => {
            app.add_system_notice(
                "退出登录",
                format!(
                    "已删除 {} 的登录。",
                    provider_label(&ctx.io.models, provider_id)
                ),
            );
            if ctx.io.session().model().provider == provider_id {
                app.add_system_notice(
                    "退出登录",
                    "当前模型属于这一家；下一次请求会要求重新登录，或者先用 /model 换一家。",
                );
            }
        }
        Err(error) => app.add_system_notice("退出登录失败", describe_auth_error(&error)),
    }
}

fn handle_copy(prompt: &str, app: &mut App) {
    let n: usize = command::first_arg(prompt)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(1);
    let texts = app.assistant_texts();
    if texts.is_empty() {
        app.add_system_notice("复制", "还没有回答可以复制。");
        return;
    }
    if n == 0 || n > texts.len() {
        app.add_system_notice(
            "复制",
            format!("/copy: 只有 {} 条回答，取不到第 {n} 条。", texts.len()),
        );
        return;
    }
    let text = texts[texts.len() - n].to_string();
    if crate::ui::clipboard::copy_to_clipboard(&text) {
        app.add_system_notice("复制", format!("已复制 {} 个字符。", text.chars().count()));
    } else {
        app.add_system_notice("复制", "复制失败：剪贴板不可用。");
    }
}

fn handle_mouse(prompt: &str, app: &mut App) {
    let value = command::first_arg(prompt)
        .unwrap_or("status")
        .to_ascii_lowercase();
    match value.as_str() {
        "status" => app.add_system_notice(
            "鼠标",
            if app.mouse_capture_enabled {
                "鼠标捕获已开：滚轮/触控板能滚，拖选松开自动复制。"
            } else {
                "鼠标捕获已关：用终端自己的选择；滚轮不会滚界面。"
            },
        ),
        "on" | "true" | "1" | "capture" => {
            app.mouse_capture_enabled = true;
            app.selection.clear();
            app.add_system_notice("鼠标", "鼠标捕获已开。");
        }
        "off" | "false" | "0" | "native" => {
            app.mouse_capture_enabled = false;
            app.selection.clear();
            app.add_system_notice("鼠标", "鼠标捕获已关；要用界面滚动时再 /mouse on。");
        }
        other => app.add_system_notice(
            "鼠标",
            format!("/mouse: 只认 on、off、status，不认 \"{other}\"。"),
        ),
    }
}

/// 把一段话作为下一轮发出去（技能展开后走这里）。跑着时按插话处理。
fn submit_chat(text: String, app: &mut App, ctx: &mut EventContext) {
    let session = ctx.io.session();
    if session.is_compacting() {
        app.add_system_notice("发送", "正在压缩上下文；等压缩完再发，或按 Esc 取消。");
        return;
    }
    if session.is_streaming() || session.is_retrying() {
        if let Err(error) = session.steer(&text) {
            app.add_system_notice("插话失败", error);
        }
        return;
    }
    ctx.io.spawn_prompt(text, app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_switch_guard_only_blocks_when_context_exceeds_the_new_window() {
        assert!(model_window_too_small(300_000, 272_000));
        assert!(!model_window_too_small(272_000, 272_000));
        assert!(!model_window_too_small(300_000, 0));
    }
}
