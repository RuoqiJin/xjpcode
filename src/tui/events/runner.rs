use std::time::Duration;

use crossterm::event::{Event as CEvent, EventStream};
use futures_util::StreamExt;
use xjpcode_ai::auth::{AuthEvent, AuthPrompt};
use xjpcode_coding_agent::core::agent_session::AgentSessionEvent;
use xjpcode_coding_agent::core::model_setup::describe_auth_error;

use crate::login::LoginEvent;
use crate::screens::terminal::TerminalGuard;
use crate::ui::state::LoginPrompt;
use crate::ui::{projection, session_info, App};

use super::context::EventContext;
use super::engine;
use super::keyboard::{handle_keyboard_event, KeyResult};
use super::mouse::handle_mouse_event;

pub async fn main_loop(
    terminal: &mut TerminalGuard,
    app: &mut App,
    mut ctx: EventContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut event_stream = EventStream::new();
    let mut tick = tokio::time::interval(engine::FRAME_BUDGET);
    let mut mouse_capture_enabled = app.mouse_capture_enabled;

    loop {
        engine::step(terminal, app, &mut ctx)?;

        tokio::select! {
            _ = tick.tick() => {
                if app.spinner.is_processing {
                    app.spinner.tick(crate::tui::render::spinner::SPINNER_FRAMES.len());
                    ctx.needs_redraw = true;
                }
                if app.toast.as_ref().is_some_and(|toast| toast.is_expired()) {
                    app.toast = None;
                    ctx.needs_redraw = true;
                }
            }

            Some(event) = ctx.rx.recv() => {
                let recalc_context = matches!(
                    event,
                    AgentSessionEvent::TurnEnd { .. }
                        | AgentSessionEvent::AgentSettled { .. }
                        | AgentSessionEvent::CompactionEnd { .. }
                );
                projection::apply(&event, app);
                if recalc_context {
                    session_info::sync_session(app, ctx.io.session());
                }
                ctx.needs_redraw = true;
            }

            Some(Ok(event)) = event_stream.next() => {
                ctx.needs_redraw = true;
                match event {
                    CEvent::Key(key) => {
                        if let KeyResult::Exit = handle_keyboard_event(key, app, &mut ctx) {
                            break;
                        }
                    }
                    CEvent::Mouse(mouse) => {
                        if app.mouse_capture_enabled {
                            handle_mouse_event(mouse, app, &mut ctx.io.auto_scroll_bottom);
                        }
                    }
                    CEvent::Paste(text) => {
                        crate::tui::events::keyboard::paste::handle_paste(&text, app);
                    }
                    CEvent::Resize(_, _) | CEvent::FocusGained => {
                        ctx.force_clear = true;
                    }
                    _ => {}
                }
            }
        }

        if ctx.force_clear {
            terminal.clear()?;
            ctx.force_clear = false;
            ctx.needs_redraw = true;
        }

        if app.mouse_capture_enabled != mouse_capture_enabled {
            mouse_capture_enabled = app.mouse_capture_enabled;
            terminal.set_mouse_capture(mouse_capture_enabled)?;
            ctx.needs_redraw = true;
        }

        poll_login(&mut ctx, app);
        poll_tasks(&mut ctx, app).await;
    }

    app.save_history();
    shutdown(&mut ctx).await;
    Ok(())
}

async fn shutdown(ctx: &mut EventContext) {
    ctx.io.session().request_abort();
    if let Some(mut task) = ctx.io.prompt_task.take() {
        if tokio::time::timeout(Duration::from_secs(5), &mut task)
            .await
            .is_err()
        {
            task.abort();
        }
    }
    if let Some(mut task) = ctx.io.compaction_task.take() {
        if tokio::time::timeout(Duration::from_secs(5), &mut task)
            .await
            .is_err()
        {
            task.abort();
        }
    }
    if let Some(login) = &ctx.io.login {
        login.cancel();
    }
    ctx.io.login = None;
}

async fn poll_tasks(ctx: &mut EventContext, app: &mut App) {
    if ctx
        .io
        .prompt_task
        .as_ref()
        .is_some_and(|task| task.is_finished())
    {
        let task = ctx.io.prompt_task.take().expect("finished prompt task");
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(message)) => {
                app.add_system_notice("请求失败", message);
            }
            Err(error) => {
                app.add_system_notice("请求任务失败", error.to_string());
            }
        }
        // 把同一轮最后几条事件也画出来。
        while let Ok(event) = ctx.rx.try_recv() {
            projection::apply(&event, app);
        }
        app.spinner.stop();
        session_info::sync_session(app, ctx.io.session());
        ctx.needs_redraw = true;
    }

    if ctx
        .io
        .compaction_task
        .as_ref()
        .is_some_and(|task| task.is_finished())
    {
        let task = ctx
            .io
            .compaction_task
            .take()
            .expect("finished compaction task");
        if let Err(error) = task.await {
            app.add_system_notice("压缩任务失败", error.to_string());
        }
        let session = ctx.io.session();
        if !session.is_streaming() && !session.is_compacting() && !session.is_retrying() {
            app.spinner.stop();
        }
        session_info::sync_session(app, ctx.io.session());
        ctx.needs_redraw = true;
    }
}

fn poll_login(ctx: &mut EventContext, app: &mut App) {
    let mut events = Vec::new();
    if let Some(login) = &mut ctx.io.login {
        while let Ok(event) = login.events.try_recv() {
            events.push(event);
        }
    }
    if events.is_empty() {
        if ctx
            .io
            .login
            .as_ref()
            .is_some_and(|login| login.is_finished())
        {
            ctx.io.login = None;
        }
        return;
    }

    let mut done = false;
    for event in events {
        match event {
            LoginEvent::Notify(event) => match event {
                AuthEvent::AuthUrl { url, instructions } => {
                    app.add_system_notice(
                        "登录",
                        format!(
                            "打开这个地址完成登录：\n{url}{}",
                            instructions
                                .map(|instructions| format!("\n{instructions}"))
                                .unwrap_or_default()
                        ),
                    );
                    app.login_url = Some(url.clone());
                    open_browser(&url);
                }
                AuthEvent::DeviceCode {
                    user_code,
                    verification_uri,
                    ..
                } => app.add_system_notice(
                    "登录",
                    format!("打开 {verification_uri}，输入 {user_code}"),
                ),
                AuthEvent::Info { message, links } => {
                    let mut text = message;
                    for link in links {
                        text.push_str(&format!("\n{}", link.url));
                    }
                    app.add_system_notice("登录", text);
                }
                AuthEvent::Progress { message } => {
                    app.add_system_notice("登录", message);
                }
            },
            LoginEvent::Prompt(prompt) => match &prompt {
                AuthPrompt::Select { options, .. } => {
                    if let Some(first) = options.first() {
                        if let Some(login) = &ctx.io.login {
                            app.add_system_notice("登录", "使用浏览器登录。");
                            login.answer(first.id.clone());
                        }
                    }
                }
                other => {
                    let secret = matches!(other, AuthPrompt::Secret { .. });
                    app.login_prompt = Some(LoginPrompt {
                        message: other.message().to_string(),
                        secret,
                        input: String::new(),
                        notice: None,
                    });
                }
            },
            LoginEvent::Done(result) => {
                app.login_url = None;
                app.login_prompt = None;
                match result {
                    Ok(credential) => app.add_system_notice(
                        "登录成功",
                        format!(
                            "已登录（{}）。用 /model 选一个模型。",
                            credential.r#type().as_str()
                        ),
                    ),
                    Err(error) => app.add_system_notice("登录失败", describe_auth_error(&error)),
                }
                done = true;
            }
        }
    }

    if done
        || ctx
            .io
            .login
            .as_ref()
            .is_some_and(|login| login.is_finished())
    {
        ctx.io.login = None;
    }
    ctx.needs_redraw = true;
}

fn open_browser(url: &str) {
    if std::env::var("XJPCODE_NO_BROWSER")
        .map(|value| !value.is_empty() && value != "0")
        .unwrap_or(false)
    {
        return;
    }
    #[cfg(target_os = "macos")]
    let (program, arguments): (&str, Vec<&str>) = ("open", vec![url]);
    #[cfg(target_os = "windows")]
    let (program, arguments): (&str, Vec<&str>) =
        ("rundll32", vec!["url.dll,FileProtocolHandler", url]);
    #[cfg(all(unix, not(target_os = "macos")))]
    let (program, arguments): (&str, Vec<&str>) = ("xdg-open", vec![url]);

    let _ = std::process::Command::new(program)
        .args(arguments)
        .current_dir("/")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
