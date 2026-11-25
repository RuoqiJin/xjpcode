//! 对应上游 `packages/ai/src/models.ts` 中被 provider 路径用到的函数。
//!
//! 上游这个文件还管模型目录、刷新与鉴权；本次只搬 `calculateCost`、`getSupportedThinkingLevels`
//! 与 `clampThinkingLevel`（见 `PORTING.md` 文件对照表）。

use crate::providers::model_schema::{ModelCostRates, ModelThinkingLevel};
use crate::types::{Model, Usage};

/// pi: `calculateCost`——按模型价目算这一轮的用量成本，写进 `usage.cost`。
///
/// `inputTokens` 把缓存读/写也算进去，用于挑选阶梯价；命中最高一档的 `inputTokensAbove` 才用
/// 那一档。1h 缓存写入按两倍输入价（Anthropic 的计法）。`totalFromProvider` 恒为 `false`：
/// pi 不信 provider 报的总价。
pub fn calculate_cost(model: &Model, usage: &mut Usage) {
    let input_tokens = usage.input + usage.cache_read + usage.cache_write;
    let mut rates = ModelCostRates {
        input: model.cost.input,
        output: model.cost.output,
        cache_read: model.cost.cache_read,
        cache_write: model.cost.cache_write,
    };
    let mut matched_threshold = -1.0;
    if let Some(tiers) = &model.cost.tiers {
        for tier in tiers {
            if input_tokens as f64 > tier.input_tokens_above && tier.input_tokens_above > matched_threshold {
                rates = ModelCostRates {
                    input: tier.input,
                    output: tier.output,
                    cache_read: tier.cache_read,
                    cache_write: tier.cache_write,
                };
                matched_threshold = tier.input_tokens_above;
            }
        }
    }

    // Anthropic 的 1h 缓存写入按两倍基础输入价。
    let long_write = usage.cache_write_1h.unwrap_or(0) as f64;
    let short_write = (usage.cache_write as f64) - long_write;
    usage.cost.input = (rates.input / 1_000_000.0) * usage.input as f64;
    usage.cost.output = (rates.output / 1_000_000.0) * usage.output as f64;
    usage.cost.cache_read = (rates.cache_read / 1_000_000.0) * usage.cache_read as f64;
    usage.cost.cache_write = (rates.cache_write * short_write + rates.input * 2.0 * long_write) / 1_000_000.0;
    usage.cost.total = usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
    usage.cost.total_from_provider = Some(false);
}

/// 上游的 `EXTENDED_THINKING_LEVELS`。
const EXTENDED_THINKING_LEVELS: [ModelThinkingLevel; 7] = [
    ModelThinkingLevel::Off,
    ModelThinkingLevel::Minimal,
    ModelThinkingLevel::Low,
    ModelThinkingLevel::Medium,
    ModelThinkingLevel::High,
    ModelThinkingLevel::Xhigh,
    ModelThinkingLevel::Max,
];

/// pi: `getSupportedThinkingLevels`
///
/// 非推理模型只有 `off`；映射为 `null` 的等级不支持；`xhigh`/`max` 只有显式映射了才支持。
pub fn get_supported_thinking_levels(model: &Model) -> Vec<ModelThinkingLevel> {
    if !model.reasoning {
        return vec![ModelThinkingLevel::Off];
    }
    EXTENDED_THINKING_LEVELS
        .iter()
        .copied()
        .filter(|level| {
            let mapped = model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(level));
            match mapped {
                Some(None) => false,
                Some(Some(_)) => true,
                None => !matches!(level, ModelThinkingLevel::Xhigh | ModelThinkingLevel::Max),
            }
        })
        .collect()
}

/// pi: `clampThinkingLevel`——把请求的等级落到模型支持的一档：先往后找，再往前找。
pub fn clamp_thinking_level(model: &Model, level: ModelThinkingLevel) -> ModelThinkingLevel {
    let available = get_supported_thinking_levels(model);
    if available.contains(&level) {
        return level;
    }
    let Some(requested_index) = EXTENDED_THINKING_LEVELS.iter().position(|l| *l == level) else {
        return available.first().copied().unwrap_or(ModelThinkingLevel::Off);
    };
    for candidate in &EXTENDED_THINKING_LEVELS[requested_index..] {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    for candidate in EXTENDED_THINKING_LEVELS[..requested_index].iter().rev() {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    available.first().copied().unwrap_or(ModelThinkingLevel::Off)
}

// ---------------------------------------------------------------------------
// 模型登记（上游 `models.ts` 的 `Models` / `Provider` / `createProvider`）
// ---------------------------------------------------------------------------
//
// 上游这一层还管动态目录（远端刷新、`models.json` 覆盖、虚拟模型）；这一轮只带
// DeepSeek 与 GPT 订阅两家静态目录，其余见 `PORTING.md` 差异表。

use std::sync::Arc;

use indexmap::IndexMap;

use crate::auth::types::{
    ApiKeyAuth, AuthCheck, AuthContext, AuthInteraction, AuthOperationOptions, AuthResult,
    AuthType, Credential, CredentialStore, LoginOptions, ModelsError, ModelsErrorCode, ProviderAuth,
    ProviderAuthInteraction,
};
use crate::auth::resolve::{resolve_provider_auth, AuthResolutionOverrides};
use crate::types::{
    ProviderHeaders, SimpleStreamOptions, TranscriptContext,
};
use crate::utils::abort::AbortSignal;
use crate::utils::event_stream::AssistantMessageEventStream;

/// provider 的请求函数（上游 `Provider.streamSimple`）。
pub type StreamSimpleFn = Arc<
    dyn Fn(
            Model,
            TranscriptContext,
            Option<SimpleStreamOptions>,
            Option<AbortSignal>,
        ) -> AssistantMessageEventStream
        + Send
        + Sync,
>;

/// pi: `Provider`——一家 provider 的运行时形态。
///
/// 上游的 `getModels()` 是同步函数；Rust 侧直接持有一份目录。`refreshModels` / `filterModels`
/// 这些动态目录字段这一轮没有搬。
#[derive(Clone)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub headers: Option<ProviderHeaders>,
    pub auth: ProviderAuth,
    pub models: Vec<Model>,
    pub stream_simple: StreamSimpleFn,
}

impl std::fmt::Debug for Provider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Provider")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("models", &self.models.len())
            .finish()
    }
}

impl Provider {
    /// pi: `createProvider({ id, name, baseUrl, models, auth, api })`
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        base_url: impl Into<String>,
        auth: ProviderAuth,
        models: Vec<Model>,
        stream_simple: StreamSimpleFn,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            base_url: base_url.into(),
            headers: None,
            auth,
            models,
            stream_simple,
        }
    }

    pub fn get_models(&self) -> &[Model] {
        &self.models
    }

    pub fn get_model(&self, id: &str) -> Option<&Model> {
        self.models.iter().find(|model| model.id == id)
    }
}

/// pi: `Models`——provider 的集合，外加鉴权与请求入口。
pub struct Models {
    providers: IndexMap<String, Provider>,
    credentials: Arc<dyn CredentialStore>,
    auth_context: Arc<dyn AuthContext>,
}

impl std::fmt::Debug for Models {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Models")
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Models {
    pub fn new(credentials: Arc<dyn CredentialStore>, auth_context: Arc<dyn AuthContext>) -> Self {
        Self {
            providers: IndexMap::new(),
            credentials,
            auth_context,
        }
    }

    /// 进程内、没有存证的一份（测试与临时用途）。
    pub fn in_memory() -> Self {
        Self::new(
            Arc::new(crate::auth::InMemoryCredentialStore::new()),
            Arc::new(crate::auth::DefaultAuthContext),
        )
    }

    pub fn set_provider(&mut self, provider: Provider) {
        self.providers.insert(provider.id.clone(), provider);
    }

    pub fn delete_provider(&mut self, provider_id: &str) {
        self.providers.shift_remove(provider_id);
    }

    pub fn get_providers(&self) -> Vec<&Provider> {
        self.providers.values().collect()
    }

    pub fn get_provider(&self, provider_id: &str) -> Option<&Provider> {
        self.providers.get(provider_id)
    }

    pub fn get_models(&self, provider_id: Option<&str>) -> Vec<Model> {
        match provider_id {
            Some(provider_id) => self
                .providers
                .get(provider_id)
                .map(|provider| provider.models.clone())
                .unwrap_or_default(),
            None => self
                .providers
                .values()
                .flat_map(|provider| provider.models.clone())
                .collect(),
        }
    }

    pub fn get_model(&self, provider_id: &str, model_id: &str) -> Option<Model> {
        self.providers
            .get(provider_id)
            .and_then(|provider| provider.get_model(model_id))
            .cloned()
    }

    pub fn credentials(&self) -> Arc<dyn CredentialStore> {
        Arc::clone(&self.credentials)
    }

    /// pi: `checkAuth`——不续 OAuth，只看有没有配好。
    pub async fn check_auth(
        &self,
        provider_id: &str,
    ) -> Result<Option<AuthCheck>, ModelsError> {
        let Some(provider) = self.providers.get(provider_id) else {
            return Ok(None);
        };
        let stored = self.credentials.read(provider_id, Default::default()).await?;
        if let Some(Credential::OAuth(_)) = &stored {
            return Ok(provider.auth.oauth.as_ref().map(|_| AuthCheck {
                source: Some("OAuth".to_string()),
                r#type: AuthType::OAuth,
            }));
        }
        let Some(api_key) = &provider.auth.api_key else {
            return Ok(None);
        };
        if let Some(check) = &api_key.check {
            let signal = AbortSignal::never();
            let credential = match stored {
                Some(Credential::ApiKey(credential)) => Some(credential),
                _ => None,
            };
            return (check)(self.auth_context.as_ref(), credential, signal)
                .await
                .map_err(|error| {
                    ModelsError::new(
                        ModelsErrorCode::Auth,
                        format!("API key auth check failed for provider {provider_id}"),
                    )
                    .with_cause(error.message)
                });
        }
        let resolution = resolve_provider_auth(
            provider_id,
            &provider.auth,
            Arc::clone(&self.credentials),
            Arc::clone(&self.auth_context),
            None,
        )
        .await?;
        Ok(resolution.map(|resolution| AuthCheck {
            source: resolution.source,
            r#type: AuthType::ApiKey,
        }))
    }

    /// pi: `getAvailable`——有钥匙或登录的 provider 的模型。
    pub async fn get_available(
        &self,
        provider_id: Option<&str>,
    ) -> Result<Vec<Model>, ModelsError> {
        let providers: Vec<&Provider> = match provider_id {
            Some(provider_id) => self.providers.get(provider_id).into_iter().collect(),
            None => self.providers.values().collect(),
        };
        let mut available = Vec::new();
        for provider in providers {
            if self.check_auth(&provider.id).await?.is_some() {
                available.extend(provider.models.clone());
            }
        }
        Ok(available)
    }

    /// pi: `getAuth`——解析一次请求的鉴权（OAuth 快到期就在锁里续）。
    pub async fn get_auth(
        &self,
        provider_id: &str,
        overrides: Option<AuthResolutionOverrides>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        let Some(provider) = self.providers.get(provider_id) else {
            return Ok(None);
        };
        resolve_provider_auth(
            provider_id,
            &provider.auth,
            Arc::clone(&self.credentials),
            Arc::clone(&self.auth_context),
            overrides,
        )
        .await
    }

    /// pi: `login`——跑 provider 的登录流程，把结果存进凭证库。
    pub async fn login(
        &self,
        provider_id: &str,
        auth_type: AuthType,
        interaction: Arc<dyn AuthInteraction>,
        options: Option<LoginOptions>,
    ) -> Result<Credential, ModelsError> {
        let Some(provider) = self.providers.get(provider_id) else {
            return Err(ModelsError::new(
                ModelsErrorCode::Provider,
                format!("Unknown provider: {provider_id}"),
            ));
        };
        let method = match auth_type {
            AuthType::OAuth => provider.auth.oauth.clone(),
            AuthType::ApiKey => None,
        };
        let with_signal: Arc<dyn ProviderAuthInteraction> =
            Arc::new(InteractionWithSignal::new(interaction));

        let credential = match (auth_type, method) {
            (AuthType::OAuth, Some(oauth)) => {
                Credential::OAuth((oauth.login)(Arc::clone(&with_signal), options).await?)
            }
            (AuthType::ApiKey, _) => {
                let api_key: &ApiKeyAuth = provider.auth.api_key.as_ref().ok_or_else(|| {
                    ModelsError::new(
                        ModelsErrorCode::Auth,
                        format!("{} does not support api_key login", provider.name),
                    )
                })?;
                let login = api_key.login.as_ref().ok_or_else(|| {
                    ModelsError::new(
                        ModelsErrorCode::Auth,
                        format!("{} does not support api_key login", provider.name),
                    )
                })?;
                Credential::ApiKey(login(with_signal.as_ref()).await?)
            }
            (AuthType::OAuth, None) => {
                return Err(ModelsError::new(
                    ModelsErrorCode::Auth,
                    format!("{} does not support oauth login", provider.name),
                ))
            }
        };

        let provider_id_owned = provider_id.to_string();
        let stored = credential.clone();
        self.credentials
            .modify(
                provider_id,
                Box::new(move |_current| {
                    let stored = stored.clone();
                    Box::pin(async move { Ok(Some(stored)) })
                }),
                AuthOperationOptions::default(),
            )
            .await
            .map_err(|error| {
                ModelsError::new(
                    ModelsErrorCode::Auth,
                    format!("Credential store modify failed for {provider_id_owned}"),
                )
                .with_cause(error.message)
            })?;
        Ok(credential)
    }

    /// pi: `logout`
    pub async fn logout(&self, provider_id: &str) -> Result<(), ModelsError> {
        self.credentials
            .delete(provider_id, AuthOperationOptions::default())
            .await
    }

    /// pi: `isUsingSubscription`——用的是不是订阅（GPT 订阅这一条为真）。
    pub async fn is_using_subscription(&self, provider_id: &str) -> bool {
        let Some(provider) = self.providers.get(provider_id) else {
            return false;
        };
        let uses_oauth = self
            .check_auth(provider_id)
            .await
            .ok()
            .flatten()
            .map(|check| check.r#type == AuthType::OAuth)
            .unwrap_or(false);
        uses_oauth
            && provider
                .auth
                .oauth
                .as_ref()
                .map(|oauth| oauth.is_subscription)
                .unwrap_or(false)
    }

    /// pi: `streamSimple`——按 model.provider 把它交给那一家。
    ///
    /// 上游在这里解析鉴权；Rust 侧请求函数是同步的，鉴权由 agent 的 `get_api_key` 在调用前
    /// 解析（同一份 `getAuth`），见 `PORTING.md` 差异表。
    pub fn stream_simple(
        &self,
        model: Model,
        context: TranscriptContext,
        options: Option<SimpleStreamOptions>,
        signal: Option<AbortSignal>,
    ) -> AssistantMessageEventStream {
        let Some(provider) = self.providers.get(&model.provider) else {
            let stream = AssistantMessageEventStream::new();
            let mut message = crate::api::openai_responses_shared::create_output(&model);
            message.stop_reason = crate::types::StopReason::Error;
            message.error_message = Some(format!("Unknown provider: {}", model.provider));
            stream.push(crate::types::AssistantMessageEvent::Error {
                reason: crate::types::StopReason::Error,
                error: message,
            });
            stream.end(None);
            return stream;
        };
        (provider.stream_simple)(model, context, options, signal)
    }

    /// pi: `modelsAreEqual`
    pub fn models_are_equal(left: &Model, right: &Model) -> bool {
        left.provider == right.provider && left.id == right.id
    }
}

/// 把只有 `AuthInteraction` 的实现补上一个必需的取消信号。
struct InteractionWithSignal {
    inner: Arc<dyn AuthInteraction>,
    signal: AbortSignal,
}

impl InteractionWithSignal {
    fn new(inner: Arc<dyn AuthInteraction>) -> Self {
        let signal = inner.signal().unwrap_or_else(AbortSignal::never);
        Self { inner, signal }
    }
}

impl AuthInteraction for InteractionWithSignal {
    fn signal(&self) -> Option<AbortSignal> {
        Some(self.signal.clone())
    }

    fn prompt<'a>(
        &'a self,
        prompt: crate::auth::types::AuthPrompt,
        signal: Option<AbortSignal>,
    ) -> crate::types::BoxFuture<'a, Result<String, ModelsError>> {
        self.inner.prompt(prompt, signal)
    }

    fn notify(&self, event: crate::auth::types::AuthEvent) {
        self.inner.notify(event);
    }
}

impl ProviderAuthInteraction for InteractionWithSignal {
    fn provider_signal(&self) -> AbortSignal {
        self.signal.clone()
    }
}
