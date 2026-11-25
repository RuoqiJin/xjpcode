//! 把模型、四样工具、规矩与技能、会话文件拼成一个 `AgentSession`。
//!
//! 这一层是界面和新内核之间唯一的装配点：`xjpcode` 的 TUI、朴素模式和一句话模式都从这里
//! 拿会话。装的仍是 pi 的四样工具（read/bash/edit/write），模型来自 `ModelSetup` 的登记表：
//! DeepSeek 的两个与 GPT 订阅那一组，钥匙和登录存在 xjpcode 自己的 `~/.xjpcode/agent/auth.json`。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use indexmap::IndexMap;
use xjpcode_agent::{Agent, AgentInitialState, AgentMessage, AgentOptions, AgentTool, ThinkingLevel};
use xjpcode_ai::auth::CredentialStore;
use xjpcode_ai::models::Models;
use xjpcode_ai::Model;
use xjpcode_coding_agent::core::agent_session::{AgentSession, AgentSessionOptions};
use xjpcode_coding_agent::core::auth_storage::AuthStorage;
use xjpcode_coding_agent::core::model_setup::{thinking_level_from_name, thinking_level_name, ModelSetup};
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
use xjpcode_coding_agent::{
    get_agent_dir, BashToolOptions, EditToolOptions, ReadToolOptions, Skill, WriteToolOptions,
};

/// 缺省模型（见 `ModelSetup`）：DeepSeek 目录里的 flash。
pub use xjpcode_coding_agent::core::model_setup::DEFAULT_MODEL as DEFAULT_MODEL_ID;

/// 打开会话的三种方式：新开、接着这个目录里最近一场、打开指定文件或编号。
#[derive(Debug, Clone)]
pub enum SessionChoice {
    New,
    Continue,
    Open(String),
}

/// 装配好的一场会话。
pub struct OpenedSession {
    pub session: Arc<AgentSession>,
    pub resumed: bool,
}

/// 进程级装配信息：模型登记、当前模型、规矩与技能、会话目录。
pub struct SessionSetup {
    cwd: PathBuf,
    session_dir: PathBuf,
    resource_dir: PathBuf,
    auth_path: PathBuf,
    model_setup: ModelSetup,
    thinking: ThinkingLevel,
    thinking_explicit: bool,
    settings: Settings,
    skills: Vec<Skill>,
    context_files: Option<Vec<ContextFile>>,
    rule_count: usize,
    skill_count: usize,
    skill_diagnostics: Vec<String>,
}

impl SessionSetup {
    /// 读环境、模型登记、规矩与技能。`model_id` 来自命令行，没给就用设置里的缺省或 `DEFAULT_MODEL`。
    ///
    /// 这里不再要求 `DEEPSEEK_API_KEY`：钥匙和登录从 `auth.json` 取，没有也能进界面去登录。
    pub fn load(model_id: Option<&str>) -> Result<Self, String> {
        let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        let agent_dir = get_agent_dir();
        let session_dir = get_default_session_dir(&cwd, &agent_dir);
        let resource_dir = resource_dir();
        let auth_path = agent_dir.join("auth.json");
        let credentials: Arc<dyn CredentialStore> =
            Arc::new(AuthStorage::create(auth_path.clone()));

        let mut resource_loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: cwd.clone(),
            agent_dir: resource_dir.clone(),
            ..DefaultResourceLoaderOptions::default()
        });
        resource_loader.reload();
        let skill_diagnostics = resource_loader
            .get_skill_diagnostics()
            .iter()
            .map(|diagnostic| {
                format!(
                    "skill {}: {} ({})",
                    diagnostic.path, diagnostic.message, diagnostic.r#type
                )
            })
            .collect();
        let context_files: Vec<ContextFile> = resource_loader
            .get_agents_files()
            .iter()
            .map(|(path, content)| ContextFile {
                path: path.clone(),
                content: content.clone(),
            })
            .collect();
        let skills = resource_loader.get_skills().to_vec();
        let rule_count = context_files.len();
        let skill_count = skills.len();
        let settings = Settings::load(&resource_dir, &cwd);
        let model_setup = ModelSetup::load(model_id, credentials, &settings)?;
        let thinking = model_setup.default_thinking_level(&settings);

        Ok(Self {
            cwd,
            session_dir,
            resource_dir,
            auth_path,
            model_setup,
            thinking,
            thinking_explicit: false,
            settings,
            skills,
            context_files: (!context_files.is_empty()).then_some(context_files),
            rule_count,
            skill_count,
            skill_diagnostics,
        })
    }

    pub fn cwd(&self) -> &PathBuf {
        &self.cwd
    }

    pub fn session_dir(&self) -> &PathBuf {
        &self.session_dir
    }

    pub fn resource_dir(&self) -> &PathBuf {
        &self.resource_dir
    }

    /// `~/.xjpcode/agent/auth.json`（`XJPCODE_AGENT_DIR` 可以换目录）。
    pub fn auth_path(&self) -> &PathBuf {
        &self.auth_path
    }

    pub fn models(&self) -> Arc<Models> {
        self.model_setup.models()
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn model(&self) -> &Model {
        self.model_setup.model()
    }

    /// 把 `/model` 或命令行里的一串解析成登记表里的模型（`provider/id` 或唯一的裸 id）。
    pub fn resolve_model(&self, reference: &str) -> Result<Model, String> {
        xjpcode_coding_agent::core::model_setup::resolve_model(&self.models(), reference)
    }

    /// 新开一场的思考档（设置里的缺省档，退回 pi 的 medium）。
    pub fn default_thinking_level(&self) -> ThinkingLevel {
        self.model_setup.default_thinking_level(&self.settings)
    }

    pub fn rule_count(&self) -> usize {
        self.rule_count
    }

    pub fn skill_count(&self) -> usize {
        self.skill_count
    }

    /// 已经装载的技能（`/skill` 列表与正文展开用）。
    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }

    pub fn skill_diagnostics(&self) -> &[String] {
        &self.skill_diagnostics
    }

    /// 界面上用 `/model` 换过的模型，下一条新会话也用它。
    pub fn remember_model(&mut self, model: Model) {
        self.model_setup.remember_model(model);
    }

    /// 打开一场会话。恢复时用记录里的模型和思考档，命令行指定过的优先。
    pub fn open(&mut self, choice: &SessionChoice) -> Result<OpenedSession, String> {
        let manager = match choice {
            SessionChoice::New => SessionManager::create(
                &self.cwd,
                Some(self.session_dir.clone()),
                NewSessionOptions::default(),
            )?,
            SessionChoice::Continue => {
                SessionManager::continue_recent(&self.cwd, Some(self.session_dir.clone()))?
            }
            SessionChoice::Open(value) => {
                let path = PathBuf::from(value);
                let path = if path.exists() {
                    path
                } else {
                    find_session_by_id(&self.session_dir, value).ok_or_else(|| {
                        format!(
                            "no session {value}: not a file, and not an id under {}",
                            self.session_dir.display()
                        )
                    })?
                };
                SessionManager::open(&path, None, None)?
            }
        };
        self.open_manager(manager)
    }

    /// 从已经建好的 `SessionManager` 装出一场会话（界面上的 `/new` 也走这里）。
    pub fn open_manager(&mut self, mut manager: SessionManager) -> Result<OpenedSession, String> {
        let context = manager.build_session_context();
        let resumed = !context.messages.is_empty();

        // 恢复模型：没有显式指定时，用会话里记下的那个（登记表里有才算）。
        let restored_model = if !self.model_setup.model_explicit() {
            context.model.as_ref().and_then(|(provider, model_id)| {
                self.model_setup.restore_model(provider, model_id)
            })
        } else {
            None
        };
        let model = restored_model.unwrap_or_else(|| self.model_setup.model().clone());

        // 恢复思考档：没显式指定时用会话里的，并按模型支持的档位夹一次。
        let restored_thinking = thinking_level_from_name(&context.thinking_level);
        let thinking = if resumed && !self.thinking_explicit {
            restored_thinking
        } else {
            self.thinking
        };
        let thinking = xjpcode_ai::models::clamp_thinking_level(&model, thinking);
        if !resumed || context.model != Some((model.provider.clone(), model.id.clone())) {
            manager.append_model_change(&model.provider, &model.id)?;
        }
        if !resumed || context.thinking_level != thinking_level_name(thinking) {
            manager.append_thinking_level_change(thinking_level_name(thinking))?;
        }
        self.model_setup.remember_model(model.clone());
        self.thinking = thinking;

        let session = self.create_session(manager, model, thinking, resumed)?;
        Ok(OpenedSession { session, resumed })
    }

    fn create_session(
        &self,
        mut manager: SessionManager,
        model: Model,
        thinking: ThinkingLevel,
        resumed: bool,
    ) -> Result<Arc<AgentSession>, String> {
        let mut session_environment = BTreeMap::new();
        session_environment.insert("PI_SESSION_ID".to_string(), manager.get_session_id().to_string());
        if let Some(session_file) = manager.get_session_file() {
            session_environment.insert(
                "PI_SESSION_FILE".to_string(),
                session_file.to_string_lossy().into_owned(),
            );
        }
        session_environment.insert("PI_PROVIDER".to_string(), model.provider.clone());
        session_environment.insert("PI_MODEL".to_string(), model.id.clone());
        session_environment.insert(
            "PI_REASONING_LEVEL".to_string(),
            thinking_level_name(thinking).to_string(),
        );

        let (tools, prompt_options) = build_tools(
            &self.cwd,
            self.skills.clone(),
            self.context_files.clone(),
            session_environment,
        );

        // pi 恢复会话时会重新装载工具、按当前资源算一次系统提示差分，并把它记成一条系统消息；
        // 下一次请求就带上「增量工具声明 + 变了的段落」。这条放在装配层：会话层的压缩路径
        // 不该因此多出一条消息（`kit-session` 的压缩场景会对不上）。
        if resumed && !tools.is_empty() {
            let context = manager.build_session_context();
            if let Some(patch) = xjpcode_coding_agent::core::model_setup::restored_prompt_message(
                &context.messages,
                &tools,
                &prompt_options,
            ) {
                manager.append_message(AgentMessage::System(patch))?;
            }
        }

        let stream_fn = self.model_setup.stream_fn();
        let get_api_key = self.model_setup.get_api_key();
        let agent = Agent::new(AgentOptions {
            initial_state: Some(AgentInitialState {
                // 与朴素入口相同：pi 的 AgentSession 从空提示词开始，第一轮把分段提示词作为
                // 系统补丁发进去，工具也在这之后才装上。
                system_prompt: Some(String::new()),
                model: Some(model.clone()),
                thinking_level: Some(thinking),
                tools: Some(Vec::new()),
                messages: None,
            }),
            convert_to_llm: None,
            transform_context: None,
            stream_fn: Some(stream_fn.clone()),
            get_api_key: Some(get_api_key.clone()),
            ..AgentOptions::default()
        })
        .map_err(|error| error.to_string())?;
        agent.set_tools(tools);

        let session = AgentSession::new(AgentSessionOptions {
            agent,
            session_manager: manager,
            settings: self.settings.clone(),
            cwd: self.cwd.clone(),
            stream_fn,
            get_api_key: Some(get_api_key),
            api_key: None,
            prompt_options,
        });
        if resumed && self.thinking_explicit {
            session.agent().set_thinking_level(thinking);
        }
        Ok(session)
    }
}

/// 用户级的规矩与技能目录：owner 的规矩在 pi 的 `~/.pi/agent/AGENTS.md`，所以缺省读 pi 的目录。
/// 会话仍然写到 xjpcode 自己的 `~/.xjpcode/agent/`。`XJPCODE_RESOURCE_DIR` 可以改前者。
pub fn resource_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XJPCODE_RESOURCE_DIR") {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_default()
        .join(".pi")
        .join("agent")
}


/// 建齐四样工具，同时拼出系统提示词要的工具清单与规矩。
fn build_tools(
    cwd: &PathBuf,
    skills: Vec<Skill>,
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
