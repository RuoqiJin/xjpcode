//! 模型登记与请求接线：`Models` 的两家 provider、缺省模型/思考档、`streamFn` 与 `getApiKey`。
//!
//! pi 里这些散在 `core/model-runtime.ts`、`core/model-resolver.ts` 与
//! `modes/interactive/interactive-mode.ts` 的装配部分；xjpcode 这一轮只带 DeepSeek 与
//! GPT 订阅两家，所以把"用哪一家、用哪把钥匙"集中在这里，界面（根包）与朴素入口（`crates/cli`）
//! 都从这里拿。

use std::sync::{Arc, Mutex};

use xjpcode_agent::{AgentMessage, AgentTool, GetApiKey, StreamFn};
use xjpcode_ai::auth::{CredentialStore, DefaultAuthContext, ModelsError, ModelsErrorCode};
use xjpcode_ai::{SystemContent, SystemMessage, SystemRole};
use xjpcode_ai::models::{Models, Provider, StreamSimpleFn};
use xjpcode_ai::providers::{deepseek, openai_codex};
use xjpcode_ai::types::{AssistantMessageEvent, BoxFuture, Model, StopReason};
use xjpcode_ai::{AbortSignal, AssistantMessageEventStream, TranscriptContext};

use crate::core::settings_manager::{Settings, DEFAULT_THINKING_LEVEL};

/// 新开一场的缺省模型：保持 xjpcode 现在的选择（DeepSeek 的 flash）。
///
/// pi 按它自己的缺省模型表挑（`findInitialModel` 的第三档）；那一档里 `openai-codex` 排在
/// `deepseek` 前面，登录了 GPT 订阅就会选 `gpt-6.2-sol`。xjpcode 这一轮保持现状：缺省干活的是
/// DeepSeek，GPT 订阅用 `/model` 换过去（见 `PORTING.md` 差异表）。
pub const DEFAULT_MODEL: &str = "deepseek-flash";

/// DeepSeek 的 provider id。
pub const DEEPSEEK_PROVIDER: &str = "deepseek";
/// GPT 订阅的 provider id。
pub const OPENAI_CODEX_PROVIDER: &str = "openai-codex";

fn deepseek_stream_simple() -> StreamSimpleFn {
    Arc::new(|model, context, options, signal| {
        xjpcode_ai::api::openai_completions::stream_simple(model, context, options, signal)
    })
}

fn codex_stream_simple() -> StreamSimpleFn {
    Arc::new(|model, context, options, signal| {
        xjpcode_ai::api::openai_codex_responses::stream_simple(model, context, options, signal)
    })
}

/// 测试用的地址覆盖：对着本机假接口验收时用（正式运行不设这个变量）。
///
/// pi 的同类做法是 `models.json` 里的 `providers.<id>.baseUrl`；xjpcode 这一轮不接 `models.json`，
/// 用环境变量代替（见 `PORTING.md` 差异表）。
fn base_url_override(name: &str, fallback: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

/// 建出这一轮带的两个 provider。
pub fn build_models(credentials: Arc<dyn CredentialStore>) -> Models {
    let mut models = Models::new(credentials, Arc::new(DefaultAuthContext));

    let deepseek_base = base_url_override("XJPCODE_DEEPSEEK_BASE_URL", deepseek::DEEPSEEK_BASE_URL);
    let deepseek_models: Vec<Model> = deepseek::models()
        .into_iter()
        .map(|mut model| {
            model.base_url = deepseek_base.clone();
            model
        })
        .collect();
    models.set_provider(Provider::new(
        DEEPSEEK_PROVIDER,
        "DeepSeek",
        deepseek_base,
        xjpcode_ai::auth::ProviderAuth {
            api_key: Some(xjpcode_ai::auth::env_api_key_auth(
                "DeepSeek API key",
                &["DEEPSEEK_API_KEY"],
            )),
            oauth: None,
        },
        deepseek_models,
        deepseek_stream_simple(),
    ));

    let codex = openai_codex::openai_codex_provider();
    let codex_base = base_url_override("XJPCODE_CODEX_BASE_URL", &codex.base_url);
    let codex_models: Vec<Model> = codex
        .models
        .into_iter()
        .map(|mut model| {
            model.base_url = codex_base.clone();
            model
        })
        .collect();
    models.set_provider(Provider::new(
        codex.id,
        codex.name,
        codex_base,
        xjpcode_ai::auth::ProviderAuth {
            api_key: None,
            oauth: Some(codex.auth),
        },
        codex_models,
        codex_stream_simple(),
    ));
    models
}

/// pi 恢复会话时补上的那条系统消息：工具重新声明 + 变了的提示词段落。
///
/// 上游是 `_restoreToolsFromTranscript` 与 `_preparePromptAndToolLoadout` 的合流：
/// 只有记录里声明过工具时才补；段落是「记录里的当前系统提示」与「按当前资源重算的一份」的差分。
pub fn restored_prompt_message(
    messages: &[AgentMessage],
    tools: &[Arc<dyn AgentTool>],
    prompt_options: &crate::core::system_prompt::BuildSystemPromptOptions,
) -> Option<SystemMessage> {
    let system_messages: Vec<xjpcode_ai::Message> = messages
        .iter()
        .filter_map(|message| match message {
            AgentMessage::System(system) => Some(xjpcode_ai::Message::System(system.clone())),
            _ => None,
        })
        .collect();
    if xjpcode_ai::get_current_tools(&system_messages).is_empty() {
        return None;
    }
    let declarations: Vec<xjpcode_ai::Tool> = tools.iter().map(|tool| tool.declaration()).collect();
    if declarations.is_empty() {
        return None;
    }
    let previous = xjpcode_ai::get_current_system_message(&system_messages)
        .and_then(|message| message.sections)
        .unwrap_or_default();
    let current =
        crate::core::system_prompt::build_system_prompt_sections(prompt_options.clone()).ok()?;
    Some(SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text(String::new()),
        sections: crate::core::system_prompt::diff_system_prompt_sections(&previous, &current),
        tools_added: Some(declarations),
        tools_removed: None,
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as u64)
            .unwrap_or(0),
    })
}

/// 一条错误事件流：请求还没发出去就失败时用它（没登录、续令牌失败）。
pub fn error_stream(model: Model, message: impl Into<String>) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::new();
    let mut output = xjpcode_ai::api::openai_responses_shared::create_output(&model);
    output.stop_reason = StopReason::Error;
    output.error_message = Some(message.into());
    stream.push(AssistantMessageEvent::Error {
        reason: StopReason::Error,
        error: output,
    });
    stream.end(None);
    stream
}

/// 一次会话要用的模型与请求接线。
pub struct ModelSetup {
    models: Arc<Models>,
    model: Model,
    /// 命令行指定过模型时，恢复会话也用这个。
    model_explicit: bool,
    /// `getApiKey` 解析失败时留下的那一句（`GetApiKey` 只能回 `Option<String>`）。
    auth_error: Arc<Mutex<Option<String>>>,
}

impl ModelSetup {
    /// 按命令行、设置、缺省值挑出新开一场用的模型。
    pub fn load(
        model_id: Option<&str>,
        credentials: Arc<dyn CredentialStore>,
        settings: &Settings,
    ) -> Result<Self, String> {
        let models = build_models(credentials);
        let (model, model_explicit) = match model_id {
            Some(reference) => (resolve_model(&models, reference)?, true),
            None => {
                let from_settings = settings
                    .default_provider
                    .as_deref()
                    .zip(settings.default_model.as_deref())
                    .and_then(|(provider, id)| models.get_model(provider, id));
                let model = from_settings
                    .or_else(|| models.get_model(DEEPSEEK_PROVIDER, DEFAULT_MODEL))
                    .or_else(|| models.get_models(None).into_iter().next())
                    .ok_or_else(|| "no models are registered".to_string())?;
                (model, false)
            }
        };
        Ok(Self {
            models: Arc::new(models),
            model,
            model_explicit,
            auth_error: Arc::new(Mutex::new(None)),
        })
    }

    pub fn models(&self) -> Arc<Models> {
        Arc::clone(&self.models)
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    pub fn model_explicit(&self) -> bool {
        self.model_explicit
    }

    pub fn remember_model(&mut self, model: Model) {
        self.model = model;
    }

    /// 按会话里记下的 (provider, model) 找回模型；找不到就是 `None`。
    pub fn restore_model(&self, provider: &str, model_id: &str) -> Option<Model> {
        self.models.get_model(provider, model_id)
    }

    /// 新开一场的思考档：设置的缺省档，退回 pi 的 `medium`。
    pub fn default_thinking_level(&self, settings: &Settings) -> xjpcode_ai::providers::model_schema::ModelThinkingLevel {
        settings
            .default_thinking_level
            .as_deref()
            .map(thinking_level_from_name)
            .unwrap_or_else(|| thinking_level_from_name(DEFAULT_THINKING_LEVEL))
    }

    /// `getApiKey`：每次请求现解析（OAuth 令牌快到期就在这里续）。
    pub fn get_api_key(&self) -> GetApiKey {
        let models = Arc::clone(&self.models);
        let auth_error = Arc::clone(&self.auth_error);
        Arc::new(move |provider_id: &str| {
            let models = Arc::clone(&models);
            let auth_error = Arc::clone(&auth_error);
            let provider_id = provider_id.to_string();
            Box::pin(async move {
                match models.get_auth(&provider_id, None).await {
                    Ok(Some(result)) => {
                        *auth_error.lock().expect("auth error") = None;
                        result.auth.api_key
                    }
                    Ok(None) => {
                        *auth_error.lock().expect("auth error") = Some(if provider_id == DEEPSEEK_PROVIDER {
                            "No credentials for deepseek. Log in with /login or set DEEPSEEK_API_KEY."
                                .to_string()
                        } else {
                            format!("No credentials for {provider_id}. Log in with /login.")
                        });
                        None
                    }
                    Err(error) => {
                        // 把「没登录 / 登录过期又续不上」翻成一句人话，别只留内部错误码。
                        *auth_error.lock().expect("auth error") = Some(describe_auth_error(&error));
                        None
                    }
                }
            }) as BoxFuture<'_, Option<String>>
        })
    }

    /// 按 `model.api` 把请求交给那一家；没有钥匙时把 `getApiKey` 留下的原因报出来。
    pub fn stream_fn(&self) -> StreamFn {
        let auth_error = Arc::clone(&self.auth_error);
        Arc::new(
            move |model: Model, context: TranscriptContext, options: xjpcode_agent::StreamFnOptions| {
                if options
                    .options
                    .api_key
                    .as_ref()
                    .map(|key| key.is_empty())
                    .unwrap_or(true)
                {
                    let message = auth_error
                        .lock()
                        .expect("auth error")
                        .take()
                        .unwrap_or_else(|| {
                            format!(
                                "No credentials for {}. Log in with /login.",
                                model.provider
                            )
                        });
                    return error_stream(model, message);
                }
                dispatch_stream(model, context, options)
            },
        )
    }
}

fn dispatch_stream(
    model: Model,
    context: TranscriptContext,
    options: xjpcode_agent::StreamFnOptions,
) -> AssistantMessageEventStream {
    match model.api.as_str() {
        "openai-codex-responses" => xjpcode_ai::api::openai_codex_responses::stream_simple(
            model,
            context,
            Some(options.options),
            options.signal,
        ),
        _ => xjpcode_ai::api::openai_completions::stream_simple(
            model,
            context,
            Some(options.options),
            options.signal,
        ),
    }
}

/// 解析命令行给的模型引用：`provider/id` 或唯一的裸 id。
pub fn resolve_model(models: &Models, reference: &str) -> Result<Model, String> {
    let reference = reference.trim();
    if let Some((provider, id)) = reference.split_once('/') {
        return models.get_model(provider, id).ok_or_else(|| {
            format!(
                "unknown model {reference} (available: {})",
                available_model_ids(models).join(", ")
            )
        });
    }
    let matches: Vec<Model> = models
        .get_models(None)
        .into_iter()
        .filter(|model| model.id == reference)
        .collect();
    match matches.len() {
        1 => Ok(matches[0].clone()),
        0 => Err(format!(
            "unknown model {reference} (available: {})",
            available_model_ids(models).join(", ")
        )),
        _ => Err(format!(
            "model {reference} exists on several providers; use provider/id ({})",
            matches
                .iter()
                .map(|model| format!("{}/{}", model.provider, model.id))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// 列出来的模型：`provider/id` 形式。
pub fn available_model_ids(models: &Models) -> Vec<String> {
    models
        .get_models(None)
        .into_iter()
        .map(|model| format!("{}/{}", model.provider, model.id))
        .collect()
}

/// pi: `isValidThinkingLevel`
pub fn thinking_level_from_name(name: &str) -> xjpcode_ai::providers::model_schema::ModelThinkingLevel {
    use xjpcode_ai::providers::model_schema::ModelThinkingLevel::*;
    match name {
        "minimal" => Minimal,
        "low" => Low,
        "medium" => Medium,
        "high" => High,
        "xhigh" => Xhigh,
        "max" => Max,
        _ => Off,
    }
}

/// pi 的 `thinkingLevel` 名字（写进会话文件与 bash 环境）。
pub fn thinking_level_name(
    level: xjpcode_ai::providers::model_schema::ModelThinkingLevel,
) -> &'static str {
    use xjpcode_ai::providers::model_schema::ModelThinkingLevel::*;
    match level {
        Off => "off",
        Minimal => "minimal",
        Low => "low",
        Medium => "medium",
        High => "high",
        Xhigh => "xhigh",
        Max => "max",
    }
}

/// 把 `ModelsError` 的分类翻成界面上的一句话（没登录 / 续不上 / 额度用完）。
pub fn describe_auth_error(error: &ModelsError) -> String {
    match error.code {
        ModelsErrorCode::OAuth => format!("登录过期或续期失败：{}", error.display_line()),
        ModelsErrorCode::Auth => format!("没有登录或钥匙无效：{}", error.display_line()),
        _ => error.display_line(),
    }
}

/// 让调用方知道某一家里有谁（`/model` 与 `/login` 用）。
pub fn provider_label(models: &Models, provider_id: &str) -> String {
    models
        .get_provider(provider_id)
        .map(|provider| provider.name.clone())
        .unwrap_or_else(|| provider_id.to_string())
}

/// 保留给以后：判断一个模型是不是走订阅。
pub async fn uses_subscription(models: &Models, model: &Model) -> bool {
    models.is_using_subscription(&model.provider).await
}

/// 这个 crate 里不直接用 `AbortSignal`，但 `get_api_key` 的签名要用到它（保持再导出）。
pub type _AbortSignal = AbortSignal;
