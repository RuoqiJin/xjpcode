//! 新内核的终端入口。
//!
//! 一条命令，输入一句话，回答边出边显示；模型有 pi 的四样工具（读、改、写、跑命令）；回答到一半
//! 按 Ctrl-C 能打断，程序正常收尾。系统提示词是 pi 的默认提示词（`core/system-prompt.ts`）。
//!
//! 会话层用 pi 的 `AgentSession`（`crates/coding-agent`）：能接着上一场聊、上下文满了自动压缩、
//! 临时出错自己重试、跑到一半能插话。
//!
//! ```text
//! xjpcode-new "把 src/lib.rs 里的 foo 改成 bar，然后跑测试"
//! xjpcode-new -c              # 接着这个目录里最近的一场聊
//! xjpcode-new --session 文件或编号前几位
//! xjpcode-new                 # 交互式，一行一句；跑着时打的字会在这一步之后插进去
//! xjpcode-new --model deepseek-v4-pro --thinking high "写一段快排"
//! ```
//!
//! 交互式里跑着时：普通一行是插话（`steer`），以 `>>` 开头的一行是排队（等它停下再发），
//! `/compact` 手动压缩。pi 的界面用 Alt+Enter 表示后者，纯终端读不到组合键，所以用前缀。
//!
//! 钥匙：`~/.xjpcode/agent/auth.json`（`/login` 或环境变量 `DEEPSEEK_API_KEY`）。

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;

use xjpcode_agent::{
    Agent, AgentEvent, AgentInitialState, AgentMessage, AgentOptions, AgentTool, ThinkingLevel,
};
use xjpcode_ai::auth::CredentialStore;
use xjpcode_ai::types::StopReason;
use xjpcode_ai::Model;
use xjpcode_coding_agent::core::agent_session::{
    AgentSession, AgentSessionEvent, AgentSessionOptions, CompactionReason,
};
use xjpcode_coding_agent::core::auth_storage::AuthStorage;
use xjpcode_coding_agent::core::model_setup::{
    thinking_level_from_name, thinking_level_name, ModelSetup,
};
use xjpcode_coding_agent::core::resource_loader::{
    DefaultResourceLoader, DefaultResourceLoaderOptions,
};
use xjpcode_coding_agent::core::session_manager::{
    find_session_by_id, get_default_session_dir, NewSessionOptions, SessionManager,
};
use xjpcode_coding_agent::core::settings_manager::Settings;
use xjpcode_coding_agent::core::system_prompt::{BuildSystemPromptOptions, ContextFile};
use xjpcode_coding_agent::core::tools::bash::{
    bash_tool_system_prompt_contribution, create_bash_tool,
};
use xjpcode_coding_agent::core::tools::edit::{
    create_edit_tool, edit_tool_system_prompt_contribution,
};
use xjpcode_coding_agent::core::tools::read::{
    create_read_tool, read_tool_system_prompt_contribution,
};
use xjpcode_coding_agent::core::tools::write::{
    create_write_tool, write_tool_system_prompt_contribution,
};
use xjpcode_coding_agent::{BashToolOptions, EditToolOptions, ReadToolOptions, WriteToolOptions};

/// 建齐四样工具，同时拼出系统提示词要的工具清单与规矩。
fn build_tools(
    cwd: &PathBuf,
    skills: Vec<xjpcode_coding_agent::Skill>,
    context_files: Option<Vec<ContextFile>>,
    session_environment: BTreeMap<String, String>,
) -> (Vec<Arc<dyn AgentTool>>, BuildSystemPromptOptions) {
    let read = create_read_tool(cwd, ReadToolOptions::default());
    let bash = create_bash_tool(
        cwd,
        BashToolOptions {
            expose_session_environment: Some(true),
            session_environment: Some(session_environment),
            ..BashToolOptions::default()
        },
    );
    let edit = create_edit_tool(cwd, EditToolOptions::default());
    let write = create_write_tool(cwd, WriteToolOptions::default());

    let mut snippets: IndexMap<String, String> = IndexMap::new();
    snippets.insert(
        "read".to_string(),
        read_tool_system_prompt_contribution().snippet.to_string(),
    );
    snippets.insert(
        "bash".to_string(),
        bash_tool_system_prompt_contribution().snippet.to_string(),
    );
    snippets.insert(
        "edit".to_string(),
        edit_tool_system_prompt_contribution().snippet.to_string(),
    );
    snippets.insert(
        "write".to_string(),
        write_tool_system_prompt_contribution().snippet.to_string(),
    );

    let mut guidelines: IndexMap<String, Vec<String>> = IndexMap::new();
    guidelines.insert(
        "read".to_string(),
        read_tool_system_prompt_contribution()
            .guidelines
            .iter()
            .map(|rule| rule.to_string())
            .collect(),
    );
    guidelines.insert(
        "edit".to_string(),
        edit_tool_system_prompt_contribution()
            .guidelines
            .iter()
            .map(|rule| rule.to_string())
            .collect(),
    );
    guidelines.insert(
        "write".to_string(),
        write_tool_system_prompt_contribution()
            .guidelines
            .iter()
            .map(|rule| rule.to_string())
            .collect(),
    );
    guidelines.insert(
        "bash".to_string(),
        bash_tool_system_prompt_contribution()
            .guidelines
            .iter()
            .map(|rule| rule.to_string())
            .collect(),
    );

    let tools: Vec<Arc<dyn AgentTool>> = vec![
        Arc::new(read),
        Arc::new(bash),
        Arc::new(edit),
        Arc::new(write),
    ];
    let options = BuildSystemPromptOptions {
        selected_tools: Some(vec![
            "read".to_string(),
            "bash".to_string(),
            "edit".to_string(),
            "write".to_string(),
        ]),
        tool_snippets: Some(snippets),
        tool_guidelines: Some(guidelines),
        cwd: cwd.to_string_lossy().into_owned(),
        context_files,
        skills: Some(skills),
        ..BuildSystemPromptOptions::default()
    };
    (tools, options)
}

/// 用户级的规矩与技能目录：owner 的规矩在 pi 的 `~/.pi/agent/AGENTS.md`，所以缺省读 pi 的目录。
/// 会话仍然写到 xjpcode 自己的 `~/.xjpcode/agent/`。`XJPCODE_RESOURCE_DIR` 可以改前者。
fn resource_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XJPCODE_RESOURCE_DIR") {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_default()
        .join(".pi")
        .join("agent")
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SessionChoice {
    New,
    Continue,
    Open(String),
}

struct CliOptions {
    model_id: Option<String>,
    model_explicit: bool,
    thinking: ThinkingLevel,
    thinking_explicit: bool,
    session: SessionChoice,
    prompt: Option<String>,
}

fn parse_args() -> Result<CliOptions, String> {
    let mut model_id: Option<String> = None;
    let mut model_explicit = false;
    let mut thinking = ThinkingLevel::Off;
    let mut thinking_explicit = false;
    let mut session = SessionChoice::New;
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model" | "-m" => {
                model_id = Some(args.next().ok_or("--model 需要一个模型名")?);
                model_explicit = true;
            }
            "--continue" | "-c" => session = SessionChoice::Continue,
            "--session" | "-s" => {
                let value = args.next().ok_or("--session 需要一个文件或编号")?;
                session = SessionChoice::Open(value);
            }
            "--thinking" | "-t" => {
                let level = args.next().ok_or("--thinking 需要一个等级")?;
                thinking_explicit = true;
                thinking = match level.as_str() {
                    "off" => ThinkingLevel::Off,
                    "minimal" => ThinkingLevel::Minimal,
                    "low" => ThinkingLevel::Low,
                    "medium" => ThinkingLevel::Medium,
                    "high" => ThinkingLevel::High,
                    "xhigh" => ThinkingLevel::Xhigh,
                    "max" => ThinkingLevel::Max,
                    other => return Err(format!("不认识的思考等级：{other}")),
                };
            }
            "--help" | "-h" => {
                println!(
                    "用法：xjpcode-new [--model 模型名] [--thinking off|minimal|low|medium|high|xhigh|max]\n\
                                       [--continue|-c] [--session|-s 文件或编号] [一句话]\n\
                     不给一句话就进交互式。跑着时普通一行是插话，>> 开头是排队，/compact 手动压缩。\n\
                     钥匙与登录读 ~/.xjpcode/agent/auth.json 或环境变量 DEEPSEEK_API_KEY。"
                );
                std::process::exit(0);
            }
            other if other.starts_with('-') => return Err(format!("不认识的参数：{other}")),
            other => prompt_parts.push(other.to_string()),
        }
    }
    Ok(CliOptions {
        model_id,
        model_explicit,
        thinking,
        thinking_explicit,
        session,
        prompt: if prompt_parts.is_empty() {
            None
        } else {
            Some(prompt_parts.join(" "))
        },
    })
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    if let Err(message) = run().await {
        eprintln!("xjpcode-new: {message}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let options = parse_args()?;
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let agent_dir_home = xjpcode_coding_agent::get_agent_dir();

    // 会话：新建 / 接着这个目录里最近的一场 / 指定一场。
    let session_dir = get_default_session_dir(&cwd, &agent_dir_home);
    let mut session_manager = match &options.session {
        SessionChoice::New => SessionManager::create(
            &cwd,
            Some(session_dir.clone()),
            NewSessionOptions::default(),
        ),
        SessionChoice::Continue => SessionManager::continue_recent(&cwd, Some(session_dir.clone())),
        SessionChoice::Open(value) => {
            let path = PathBuf::from(value);
            let path = if path.exists() {
                path
            } else {
                find_session_by_id(&session_dir, value).ok_or_else(|| {
                    format!(
                        "找不到会话：{value}（不是文件，也{}里没有这个编号）",
                        session_dir.display()
                    )
                })?
            };
            SessionManager::open(&path, None, None)
        }
    }?;
    let restored = session_manager.build_session_context();
    let resumed = !restored.messages.is_empty();

    // 模型登记：DeepSeek 的两个与 GPT 订阅那一组；钥匙和登录读 auth.json。
    let settings = Settings::load(&resource_dir(), &cwd);
    let credentials: Arc<dyn CredentialStore> =
        Arc::new(AuthStorage::create(agent_dir_home.join("auth.json")));
    let model_setup = ModelSetup::load(
        options.model_id.as_deref(),
        credentials,
        &settings,
    )?;

    // 恢复模型：没有显式指定时，用会话里记下的那个。
    let restored_model = if options.model_explicit {
        None
    } else {
        restored
            .model
            .as_ref()
            .and_then(|(provider, model_id)| model_setup.restore_model(provider, model_id))
    };
    let model = restored_model.unwrap_or_else(|| model_setup.model().clone());

    // 恢复思考档：显式指定 > 会话里记下的 > 设置的缺省档。
    let effective_thinking = if options.thinking_explicit {
        options.thinking
    } else if resumed {
        thinking_level_from_name(&restored.thinking_level)
    } else {
        model_setup.default_thinking_level(&settings)
    };
    let effective_thinking = xjpcode_ai::models::clamp_thinking_level(&model, effective_thinking);
    if !resumed
        || restored.model
            != Some((model.provider.clone(), model.id.clone()))
    {
        session_manager.append_model_change(&model.provider, &model.id)?;
    }
    if !resumed || restored.thinking_level != thinking_level_name(effective_thinking) {
        session_manager.append_thinking_level_change(thinking_level_name(effective_thinking))?;
    }
    let mut session_environment = BTreeMap::new();
    session_environment.insert(
        "PI_SESSION_ID".to_string(),
        session_manager.get_session_id().to_string(),
    );
    if let Some(session_file) = session_manager.get_session_file() {
        session_environment.insert(
            "PI_SESSION_FILE".to_string(),
            session_file.to_string_lossy().into_owned(),
        );
    }
    session_environment.insert("PI_PROVIDER".to_string(), model.provider.clone());
    session_environment.insert("PI_MODEL".to_string(), model.id.clone());
    session_environment.insert(
        "PI_REASONING_LEVEL".to_string(),
        thinking_level_name(effective_thinking).to_string(),
    );

    // 规矩与技能：和 pi 在同一个目录里看到的一样（用户级 + 从 cwd 往上各层）。
    let mut resource_loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
        cwd: cwd.clone(),
        agent_dir: resource_dir(),
        ..DefaultResourceLoaderOptions::default()
    });
    resource_loader.reload();
    for diagnostic in resource_loader.get_skill_diagnostics() {
        eprintln!(
            "xjpcode-new: 技能 {}：{}（{}）",
            diagnostic.path, diagnostic.message, diagnostic.r#type
        );
    }
    let context_files: Vec<ContextFile> = resource_loader
        .get_agents_files()
        .iter()
        .map(|(path, content)| ContextFile {
            path: path.clone(),
            content: content.clone(),
        })
        .collect();
    let (tools, prompt_options) = build_tools(
        &cwd,
        resource_loader.get_skills().to_vec(),
        (!context_files.is_empty()).then_some(context_files),
        session_environment,
    );
    let rule_count = resource_loader.get_agents_files().len();
    let skill_count = resource_loader.get_skills().len();

    // pi 恢复会话时会把工具重新声明一遍、并按当前资源补一条系统提示差分；两处入口都要做。
    if resumed {
        if let Some(patch) = xjpcode_coding_agent::core::model_setup::restored_prompt_message(
            &restored.messages,
            &tools,
            &prompt_options,
        ) {
            session_manager.append_message(xjpcode_agent::AgentMessage::System(patch))?;
        }
    }

    let stream_fn = model_setup.stream_fn();
    let get_api_key = model_setup.get_api_key();

    let agent = Agent::new(AgentOptions {
        initial_state: Some(AgentInitialState {
            // pi 的 AgentSession 从空提示词开始，第一轮把分段提示词作为系统补丁发进去；
            // 工具也在这之后才装上，于是首条系统消息带上 `toolsAdded`（与 pi 的会话文件一致）。
            system_prompt: Some(String::new()),
            model: Some(model.clone()),
            thinking_level: Some(effective_thinking),
            tools: Some(Vec::new()),
            messages: None,
        }),
        convert_to_llm: None,
        transform_context: None,
        stream_fn: Some(stream_fn.clone()),
        // pi 的 `envApiKeyAuth`：每次请求现读环境变量（令牌会过期时也就是这里换）。
        get_api_key: Some(get_api_key.clone()),
        ..AgentOptions::default()
    })
    .map_err(|error| error.to_string())?;
    agent.set_tools(tools);

    let session = AgentSession::new(AgentSessionOptions {
        agent,
        session_manager,
        settings: settings.clone(),
        cwd: cwd.clone(),
        stream_fn,
        get_api_key: Some(get_api_key),
        api_key: None,
        prompt_options,
    });
    if resumed && options.thinking_explicit {
        session.agent().set_thinking_level(options.thinking);
    }
    print_banner(
        &session.model(),
        session.thinking_level(),
        rule_count,
        skill_count,
        session.session_file().as_deref(),
        resumed,
    );

    // 事件 → 终端。文本边到边显示；工具调用有起止两行；结束时补用量。
    let show_thinking = std::env::var("XJPCODE_SHOW_THINKING").ok().as_deref() == Some("1");
    let _subscription = session.agent().subscribe(move |event, _signal| {
        Box::pin(async move {
            match event {
                AgentEvent::MessageUpdate {
                    assistant_message_event,
                    ..
                } => match assistant_message_event {
                    xjpcode_ai::AssistantMessageEvent::TextDelta { delta, .. } => {
                        print!("{delta}");
                        let _ = std::io::stdout().flush();
                    }
                    xjpcode_ai::AssistantMessageEvent::ThinkingDelta { delta, .. } => {
                        if show_thinking {
                            eprint!("{delta}");
                            let _ = std::io::stderr().flush();
                        }
                    }
                    _ => {}
                },
                AgentEvent::ToolExecutionStart {
                    tool_name, args, ..
                } => {
                    println!("\n{}", describe_tool_start(tool_name, args));
                }
                AgentEvent::ToolExecutionEnd {
                    tool_name,
                    result,
                    is_error,
                    duration_ms,
                    ..
                } => {
                    print_tool_result(tool_name, result, *is_error, *duration_ms);
                }
                AgentEvent::TurnEnd { message, .. } => {
                    if let AgentMessage::Assistant(assistant) = message {
                        let usage = &assistant.usage;
                        println!(
                            "\n[用量] 输入 {} / 输出 {} / 缓存读 {} / 合计 {} token，约 ${:.6}",
                            usage.input,
                            usage.output,
                            usage.cache_read,
                            usage.total_tokens,
                            usage.cost.total
                        );
                    }
                }
                _ => {}
            }
        })
    });

    // 会话层的事件 → 终端：重试、压缩、排队都能看见。
    let _session_events = session.subscribe(move |event| match event {
        AgentSessionEvent::AutoRetryStart {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        } => {
            eprintln!(
                "\n[重试] 第 {attempt}/{max_attempts} 次，{delay_ms} 毫秒后重试：{error_message}"
            );
        }
        AgentSessionEvent::AutoRetryEnd {
            success,
            attempt,
            final_error,
        } => {
            if *success {
                eprintln!("[重试] 成功（第 {attempt} 次）");
            } else {
                eprintln!(
                    "[重试] 放弃（{attempt} 次）：{}",
                    final_error.as_deref().unwrap_or("未知错误")
                );
            }
        }
        AgentSessionEvent::CompactionStart { reason } => {
            eprintln!("\n[压缩] 开始（{}）…", compaction_reason_name(*reason));
        }
        AgentSessionEvent::CompactionEnd {
            reason,
            result,
            aborted,
            error_message,
            ..
        } => {
            if *aborted {
                eprintln!("[压缩] 已取消（{}）", compaction_reason_name(*reason));
            } else if let Some(result) = result {
                eprintln!(
                    "[压缩] 完成（{}）：{} → {} token",
                    compaction_reason_name(*reason),
                    result.tokens_before,
                    result.estimated_tokens_after.unwrap_or(0)
                );
            } else if let Some(error_message) = error_message {
                eprintln!("[压缩] 失败（{}）：{error_message}", compaction_reason_name(*reason));
            }
        }
        AgentSessionEvent::SummarizationRetryScheduled {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        } => {
            eprintln!(
                "[压缩] 摘要请求重试 第 {attempt}/{max_attempts} 次，{delay_ms} 毫秒后：{error_message}"
            );
        }
        AgentSessionEvent::QueueUpdate {
            steering,
            follow_up,
        } => {
            if !steering.is_empty() || !follow_up.is_empty() {
                eprintln!(
                    "[排队] 插话 {} 条，排队 {} 条",
                    steering.len(),
                    follow_up.len()
                );
            }
        }
        _ => {}
    });

    // Ctrl-C：打断当前这一轮（重试的等待、压缩也一并取消），程序不退出。
    let abort_handle = {
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            loop {
                if tokio::signal::ctrl_c().await.is_err() {
                    break;
                }
                if session.is_streaming() || session.is_retrying() || session.is_compacting() {
                    session.request_abort();
                    eprintln!("\n[已打断]");
                }
            }
        })
    };

    let result = match options.prompt {
        Some(prompt) => {
            let result = run_prompt_once(&session, &prompt).await;
            println!();
            result
        }
        None => repl(&session).await,
    };
    abort_handle.abort();
    result
}

fn compaction_reason_name(reason: CompactionReason) -> &'static str {
    match reason {
        CompactionReason::Manual => "手动",
        CompactionReason::Threshold => "到门槛",
        CompactionReason::Overflow => "溢出恢复",
    }
}

fn print_banner(
    model: &Model,
    thinking: ThinkingLevel,
    rule_count: usize,
    skill_count: usize,
    session_file: Option<&std::path::Path>,
    resumed: bool,
) {
    println!(
        "xjpcode 新内核 · {} ({}) · 思考档 {thinking:?} · 规矩 {rule_count} 份 · 技能 {skill_count} 个 · 空行或 exit 退出，Ctrl-C 打断",
        model.name, model.id
    );
    if let Some(session_file) = session_file {
        println!(
            "会话：{}{}",
            session_file.display(),
            if resumed { "（接着聊）" } else { "" }
        );
    }
}

fn json_arg(args: &serde_json::Map<String, Value>, key: &str) -> String {
    args.get(key)
        .map(|value| match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}

/// 工具开始时的那一行：说明它要读什么、改什么、跑什么。
fn describe_tool_start(tool_name: &str, args: &serde_json::Map<String, Value>) -> String {
    match tool_name {
        "read" => {
            let mut text = format!("[工具] read 读：{}", json_arg(args, "path"));
            if let Some(offset) = args.get("offset").and_then(Value::as_f64) {
                text.push_str(&format!("（从第 {} 行", offset as i64));
                if let Some(limit) = args.get("limit").and_then(Value::as_f64) {
                    text.push_str(&format!("，最多 {} 行", limit as i64));
                }
                text.push('）');
            } else if let Some(limit) = args.get("limit").and_then(Value::as_f64) {
                text.push_str(&format!("（最多 {} 行）", limit as i64));
            }
            text
        }
        "write" => format!("[工具] write 写：{}", json_arg(args, "path")),
        "edit" => {
            let count = args
                .get("edits")
                .and_then(Value::as_array)
                .map(|edits| edits.len())
                .unwrap_or(0);
            format!("[工具] edit 改：{}（{count} 处）", json_arg(args, "path"))
        }
        "bash" => format!("[工具] bash 跑：{}", json_arg(args, "command")),
        other => format!("[工具] {other} {}", Value::Object(args.clone())),
    }
}

fn text_of(result: &xjpcode_agent::AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            xjpcode_ai::ToolResultContentBlock::Text(text) => Some(text.text.clone()),
            xjpcode_ai::ToolResultContentBlock::Image(_) => Some("[图片]".to_string()),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn print_indented(text: &str, max_lines: usize, from_end: bool) {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return;
    }
    let (shown, omitted) = if lines.len() > max_lines {
        if from_end {
            (&lines[lines.len() - max_lines..], lines.len() - max_lines)
        } else {
            (&lines[..max_lines], lines.len() - max_lines)
        }
    } else {
        (&lines[..], 0)
    };
    if from_end && omitted > 0 {
        println!("  │ …（前 {omitted} 行略）");
    }
    for line in shown {
        println!("  │ {line}");
    }
    if !from_end && omitted > 0 {
        println!("  │ …（后 {omitted} 行略）");
    }
}

fn print_tool_result(
    tool_name: &str,
    result: &xjpcode_agent::AgentToolResult,
    is_error: bool,
    duration_ms: Option<u64>,
) {
    let elapsed = duration_ms.unwrap_or(0);
    let text = text_of(result);
    if is_error {
        println!("[工具] {tool_name} 失败（{elapsed} ms）：{text}");
        return;
    }
    match tool_name {
        "read" => {
            let line_count = text.lines().count();
            println!(
                "[工具] read 完成（{elapsed} ms）：{line_count} 行 / {} 字节",
                text.len()
            );
            print_indented(&text, 8, false);
        }
        "edit" => {
            let diff = result
                .details
                .as_ref()
                .and_then(|details| details.get("diff"))
                .and_then(Value::as_str);
            println!("[工具] edit 完成（{elapsed} ms）：{text}");
            if let Some(diff) = diff {
                print_indented(diff, 40, false);
            }
        }
        "bash" => {
            let exit_code = result
                .structured_content
                .as_ref()
                .and_then(|structured| structured.get("exit_code"))
                .and_then(Value::as_i64);
            match exit_code {
                Some(exit_code) => {
                    println!("[工具] bash 完成（{elapsed} ms，退出码 {exit_code}）：");
                }
                None => println!("[工具] bash 完成（{elapsed} ms）："),
            }
            print_indented(&text, 20, true);
        }
        _ => println!("[工具] {tool_name} 完成（{elapsed} ms）：{text}"),
    }
}

/// pi `print-mode.ts`：最后一轮是 error/aborted 就报出 `errorMessage`；length 另说一句。
fn last_assistant_failure(messages: &[AgentMessage]) -> Option<String> {
    let last = messages.iter().rev().find_map(|message| match message {
        AgentMessage::Assistant(assistant) => Some(assistant),
        _ => None,
    })?;
    match last.stop_reason {
        StopReason::Error | StopReason::Aborted => {
            Some(last.error_message.clone().unwrap_or_else(|| {
                format!(
                    "Request {}",
                    match last.stop_reason {
                        StopReason::Error => "error",
                        StopReason::Aborted => "aborted",
                        _ => unreachable!(),
                    }
                )
            }))
        }
        StopReason::Length => {
            Some("Response was truncated: the model reached its output token limit.".to_string())
        }
        _ => None,
    }
}

async fn run_prompt_once(session: &AgentSession, prompt: &str) -> Result<(), String> {
    session.prompt(prompt, None).await?;
    match last_assistant_failure(&session.messages()) {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

/// 交互式：一条输入线和一个在跑的任务并行。跑着时打进来的话排队（`steer` 或 `>>` 的
/// `followUp`），跑完再收下一条。`/compact` 手动压缩。
async fn repl(session: &Arc<AgentSession>) -> Result<(), String> {
    let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<Option<String>>();
    // 用一条普通线程读标准输入：tokio 的 stdin 读在运行时收尾时会等它，程序会卡在退出上。
    std::thread::spawn(move || {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(line) => {
                    if line_tx.send(Some(line)).is_err() {
                        break;
                    }
                }
                Err(_) => {
                    let _ = line_tx.send(None);
                    break;
                }
            }
        }
        let _ = line_tx.send(None);
    });
    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let mut running: Option<tokio::task::JoinHandle<Result<(), String>>> = None;

    loop {
        if running.is_none() {
            eprint!("› ");
            let _ = std::io::stderr().flush();
        }
        tokio::select! {
            message = line_rx.recv() => {
                let Some(message) = message else {
                    if running.is_none() {
                        break;
                    }
                    let _ = done_rx.recv().await;
                    running = None;
                    break;
                };
                let Some(line) = message else {
                    if running.is_none() {
                        break;
                    }
                    let _ = done_rx.recv().await;
                    running = None;
                    break;
                };
                let text = line.trim().to_string();
                if running.is_some() {
                    if text.is_empty() {
                        continue;
                    }
                    if let Some(rest) = text.strip_prefix(">>") {
                        session.follow_up(rest.trim())?;
                        eprintln!("[已排队] 等它停下再发");
                    } else if text == "/compact" {
                        eprintln!("[压缩] 等这一轮结束再打 /compact");
                    } else {
                        session.steer(&text)?;
                        eprintln!("[已插话] 这一步做完就发");
                    }
                    continue;
                }
                if text.is_empty() || matches!(text.as_str(), "exit" | "quit") {
                    break;
                }
                if text == "/compact" {
                    match session.compact(None).await {
                        Ok(result) => println!(
                            "[压缩] 完成：{} → {} token",
                            result.tokens_before,
                            result.estimated_tokens_after.unwrap_or(0)
                        ),
                        Err(error) => eprintln!("[压缩] {error}"),
                    }
                    continue;
                }
                let session = Arc::clone(session);
                let prompt = text.clone();
                let done_tx = done_tx.clone();
                running = Some(tokio::spawn(async move {
                    let result = session.prompt(&prompt, None).await;
                    let _ = done_tx.send(());
                    result
                }));
            }
            _ = done_rx.recv(), if running.is_some() => {
                if let Some(handle) = running.take() {
                    match handle.await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => eprintln!("[失败] {error}"),
                        Err(error) => eprintln!("[失败] {error}"),
                    }
                }
                println!();
            }
        }
    }

    if let Some(handle) = running.take() {
        let _ = handle.await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(stop_reason: StopReason, error_message: Option<&str>) -> AgentMessage {
        let mut message = xjpcode_agent::empty_assistant_message(
            "openai-completions",
            "deepseek",
            "deepseek-flash",
        );
        message.stop_reason = stop_reason;
        message.error_message = error_message.map(ToString::to_string);
        AgentMessage::Assistant(message)
    }

    #[test]
    fn reports_the_last_assistant_error_and_abort() {
        assert_eq!(
            last_assistant_failure(&[assistant(StopReason::Error, Some("401 Unauthorized"))]),
            Some("401 Unauthorized".to_string())
        );
        assert_eq!(
            last_assistant_failure(&[assistant(StopReason::Aborted, None)]),
            Some("Request aborted".to_string())
        );
        assert_eq!(
            last_assistant_failure(&[assistant(StopReason::Length, None)]),
            Some("Response was truncated: the model reached its output token limit.".to_string())
        );
    }

    #[test]
    fn reports_nothing_for_a_normal_turn_or_no_assistant_message() {
        assert_eq!(
            last_assistant_failure(&[assistant(StopReason::Stop, None)]),
            None
        );
        assert_eq!(last_assistant_failure(&[]), None);
        assert_eq!(
            last_assistant_failure(&[AgentMessage::Custom(xjpcode_agent::CustomAgentMessage {
                role: "custom".to_string(),
                payload: serde_json::Map::new(),
            })]),
            None
        );
    }
}
