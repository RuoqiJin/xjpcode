//! 对应上游 `packages/ai/src/auth/types.ts`。
//!
//! 登录与钥匙的形状：存着的一份凭证（`Credential`）、一种 provider 的鉴权方式
//! （`ApiKeyAuth` / `OAuthAuth`）、解析出来的一次请求鉴权（`AuthResult`），以及登录过程中
//! 向界面提问、报事件的 `AuthInteraction`。
//!
//! 与上游不同的（见 `PORTING.md` 差异表）：
//! - Promise 落成 `BoxFuture`，回调字段是 `Arc<dyn Fn…>`；Rust 没有对象字面量，所以用结构体。
//! - `AbortSignal` 用 crate 自己的那一份（`utils::abort`），需要持有的地方按值传。
//! - `OAuthCredentials` 的索引签名落成 `extra`（`#[serde(flatten)]`），`accountId` 照样在里面；
//!   另外提供 `account_id()` 取它。

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::types::{BoxFuture, JsonValue, ProviderEnv, ProviderHeaders};
use crate::utils::abort::AbortSignal;

/// pi: `AuthType` = `"api_key" | "oauth"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AuthType {
    #[serde(rename = "api_key")]
    ApiKey,
    #[serde(rename = "oauth")]
    OAuth,
}

impl AuthType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::OAuth => "oauth",
        }
    }
}

/// pi: `ModelsErrorCode`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelsErrorCode {
    #[serde(rename = "auth")]
    Auth,
    #[serde(rename = "oauth")]
    OAuth,
    #[serde(rename = "provider")]
    Provider,
    #[serde(rename = "model_source")]
    ModelSource,
    #[serde(rename = "stream")]
    Stream,
}

impl ModelsErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::OAuth => "oauth",
            Self::Provider => "provider",
            Self::ModelSource => "model_source",
            Self::Stream => "stream",
        }
    }
}

/// pi: `ModelsError`——带分类的鉴权/请求失败。分类会一路留到界面，用来分辨
/// 「没登录」「登录过期又续不上」「订阅额度用完」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelsError {
    pub code: ModelsErrorCode,
    pub message: String,
    pub cause: Option<String>,
}

impl ModelsError {
    pub fn new(code: ModelsErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            cause: None,
        }
    }

    pub fn with_cause(mut self, cause: impl std::fmt::Display) -> Self {
        self.cause = Some(cause.to_string());
        self
    }

    /// 登录流程里的一句普通失败（上游的 `new Error(message)`）。
    pub fn login(message: impl Into<String>) -> Self {
        Self::new(ModelsErrorCode::Auth, message)
    }

    /// 界面上给用户看的一行：`[code] message: cause`。
    pub fn display_line(&self) -> String {
        match &self.cause {
            Some(cause) if !cause.is_empty() => {
                format!("[{}] {}: {cause}", self.code.as_str(), self.message)
            }
            _ => format!("[{}] {}", self.code.as_str(), self.message),
        }
    }
}

impl std::fmt::Display for ModelsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.display_line())
    }
}

impl std::error::Error for ModelsError {}

pub type AuthResultType = Result<AuthResult, ModelsError>;
pub type CredentialResult<T> = Result<T, ModelsError>;

/// pi: `ModelAuth`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelAuth {
    pub api_key: Option<String>,
    pub headers: Option<ProviderHeaders>,
    pub base_url: Option<String>,
}

/// pi: `ApiKeyCredential`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ApiKeyCredential {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<ProviderEnv>,
}

/// pi: `OAuthCredentials`——`{ refresh, access, expires, [key: string]: unknown }`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OAuthCredential {
    pub access: String,
    pub refresh: String,
    /// Unix 毫秒时间戳。
    pub expires: i64,
    /// 其余 provider 私有字段（OpenAI 的 `accountId` 也在这里）。
    #[serde(flatten)]
    pub extra: BTreeMap<String, JsonValue>,
}

impl OAuthCredential {
    /// pi: `credential.accountId`（OpenAI Codex 用）。
    pub fn account_id(&self) -> Option<&str> {
        self.extra.get("accountId").and_then(|value| value.as_str())
    }

    pub fn set_account_id(&mut self, account_id: impl Into<String>) {
        self.extra
            .insert("accountId".to_string(), JsonValue::String(account_id.into()));
    }
}

/// pi: `Credential` = `ApiKeyCredential | OAuthCredential`（`type` 是判别字段）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Credential {
    #[serde(rename = "api_key")]
    ApiKey(ApiKeyCredential),
    #[serde(rename = "oauth")]
    OAuth(OAuthCredential),
}

impl Credential {
    pub fn r#type(&self) -> AuthType {
        match self {
            Self::ApiKey(_) => AuthType::ApiKey,
            Self::OAuth(_) => AuthType::OAuth,
        }
    }
}

/// pi: `CredentialInfo`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialInfo {
    pub provider_id: String,
    pub r#type: AuthType,
}

/// pi: `AuthOperationOptions`——`signal` 只用来取消等待；真正落盘的写入不受它影响。
#[derive(Debug, Clone, Default)]
pub struct AuthOperationOptions {
    pub signal: Option<AbortSignal>,
}

/// pi: `CredentialStore`——按 provider id 一份凭证；`modify` 是唯一的写入路径，
/// 同一 provider 的写入串行（跨进程时由文件锁保证）。
pub trait CredentialStore: Send + Sync {
    fn read<'a>(
        &'a self,
        provider_id: &'a str,
        options: AuthOperationOptions,
    ) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>>;

    fn list<'a>(
        &'a self,
        options: AuthOperationOptions,
    ) -> BoxFuture<'a, Result<Vec<CredentialInfo>, ModelsError>>;

    fn modify<'a>(
        &'a self,
        provider_id: &'a str,
        f: ModifyCredentialFn<'a>,
        options: AuthOperationOptions,
    ) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>>;

    fn delete<'a>(
        &'a self,
        provider_id: &'a str,
        options: AuthOperationOptions,
    ) -> BoxFuture<'a, Result<(), ModelsError>>;
}

/// `modify` 的写入函数：看到当前那份，返回新的（`None` = 不改）。
pub type ModifyCredentialFn<'a> =
    Box<dyn FnOnce(Option<Credential>) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>> + Send + 'a>;

/// pi: `AuthContext`——环境变量与文件是否存在。测试与浏览器里可以换掉。
pub trait AuthContext: Send + Sync {
    fn env<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Option<String>>;
    fn file_exists<'a>(&'a self, path: &'a str) -> BoxFuture<'a, bool>;
}

/// pi: `AuthResult`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthResult {
    pub auth: ModelAuth,
    pub env: Option<ProviderEnv>,
    /// 界面上显示来源：`"DEEPSEEK_API_KEY"`、`"OAuth"`、`"stored credential"`。
    pub source: Option<String>,
}

/// pi: `AuthCheck`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthCheck {
    pub source: Option<String>,
    pub r#type: AuthType,
}

/// pi: `AuthPrompt` 的 `select` 选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthPromptOption {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
}

/// pi: `AuthPrompt`——登录途中要问用户的一句话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthPrompt {
    Text {
        message: String,
        placeholder: Option<String>,
    },
    Secret {
        message: String,
        placeholder: Option<String>,
    },
    Select {
        message: String,
        options: Vec<AuthPromptOption>,
    },
    ManualCode {
        message: String,
        placeholder: Option<String>,
    },
}

impl AuthPrompt {
    pub fn message(&self) -> &str {
        match self {
            Self::Text { message, .. }
            | Self::Secret { message, .. }
            | Self::Select { message, .. }
            | Self::ManualCode { message, .. } => message,
        }
    }
}

/// pi: `AuthInfoLink`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthInfoLink {
    pub url: String,
    pub label: Option<String>,
}

/// pi: `AuthEvent`——登录途中给界面的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthEvent {
    Info {
        message: String,
        links: Vec<AuthInfoLink>,
    },
    AuthUrl {
        url: String,
        instructions: Option<String>,
    },
    DeviceCode {
        user_code: String,
        verification_uri: String,
        open_browser_on_confirm: bool,
        copy_code_on_confirm: bool,
        interval_seconds: Option<u64>,
        expires_in_seconds: Option<u64>,
    },
    Progress {
        message: String,
    },
}

/// pi: `AuthInteraction`——登录问话与通知的回调。
pub trait AuthInteraction: Send + Sync {
    fn signal(&self) -> Option<AbortSignal>;

    /// 问一句话；`signal` 只取消这一次提问（例如浏览器回调先到了）。
    fn prompt<'a>(
        &'a self,
        prompt: AuthPrompt,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'a, Result<String, ModelsError>>;

    fn notify(&self, event: AuthEvent);
}

/// pi: `ProviderAuthInteraction`——登录流程拿到的版本，`signal` 必有。
pub trait ProviderAuthInteraction: AuthInteraction {
    fn provider_signal(&self) -> AbortSignal;
}

/// pi: `ApiKeyAuth` 的解析回调。
pub type ApiKeyResolveFn = Arc<
    dyn for<'a> Fn(
            &'a dyn AuthContext,
            Option<ApiKeyCredential>,
            Option<AbortSignal>,
        ) -> BoxFuture<'a, Result<Option<AuthResult>, ModelsError>>
        + Send
        + Sync,
>;

/// pi: `ApiKeyAuth.check`
pub type ApiKeyCheckFn = Arc<
    dyn for<'a> Fn(
            &'a dyn AuthContext,
            Option<ApiKeyCredential>,
            AbortSignal,
        ) -> BoxFuture<'a, Result<Option<AuthCheck>, ModelsError>>
        + Send
        + Sync,
>;

/// pi: `ApiKeyAuth.login`
pub type ApiKeyLoginFn = Arc<
    dyn for<'a> Fn(
            &'a dyn ProviderAuthInteraction,
        ) -> BoxFuture<'a, Result<ApiKeyCredential, ModelsError>>
        + Send
        + Sync,
>;

/// pi: `ApiKeyAuth`
#[derive(Clone)]
pub struct ApiKeyAuth {
    pub name: String,
    pub login: Option<ApiKeyLoginFn>,
    pub check: Option<ApiKeyCheckFn>,
    pub resolve: ApiKeyResolveFn,
}

impl std::fmt::Debug for ApiKeyAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApiKeyAuth")
            .field("name", &self.name)
            .field("has_login", &self.login.is_some())
            .field("has_check", &self.check.is_some())
            .finish()
    }
}

/// pi: `OAuthAuth.login`
pub type OAuthLoginFn = Arc<
    dyn Fn(
            Arc<dyn ProviderAuthInteraction>,
            Option<LoginOptions>,
        ) -> BoxFuture<'static, Result<OAuthCredential, ModelsError>>
        + Send
        + Sync,
>;

/// pi: `OAuthAuth.refresh`——换令牌。真实网络上失败就是失败，调用方保留旧凭证。
pub type OAuthRefreshFn = Arc<
    dyn Fn(
            OAuthCredential,
            AbortSignal,
        ) -> BoxFuture<'static, Result<OAuthCredential, ModelsError>>
        + Send
        + Sync,
>;

/// pi: `OAuthAuth.toAuth`——从有效凭证推出请求鉴权。
pub type OAuthToAuthFn = Arc<
    dyn Fn(OAuthCredential) -> BoxFuture<'static, Result<ModelAuth, ModelsError>> + Send + Sync,
>;

/// pi: `OAuthAuth`
#[derive(Clone)]
pub struct OAuthAuth {
    pub name: String,
    pub is_subscription: bool,
    pub login_label: Option<String>,
    pub login: OAuthLoginFn,
    pub refresh: OAuthRefreshFn,
    pub to_auth: OAuthToAuthFn,
}

impl std::fmt::Debug for OAuthAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OAuthAuth")
            .field("name", &self.name)
            .field("is_subscription", &self.is_subscription)
            .finish()
    }
}

/// pi: `ProviderAuth`——至少有一种；没有的写成 `None`。
#[derive(Clone, Debug)]
pub struct ProviderAuth {
    pub api_key: Option<ApiKeyAuth>,
    pub oauth: Option<OAuthAuth>,
}

/// pi: `LoginOptions`
#[derive(Debug, Clone, Default)]
pub struct LoginOptions {
    /// 这台机器稳定的 id（OpenAI 在登录时要）。
    pub device_id: Option<String>,
    /// 应用介绍自己用的名字（OpenAI 的 agent name / Codex originator），缺省 `pi`。
    pub agent_name: Option<String>,
}

impl LoginOptions {
    pub fn agent_name_or_default(&self) -> &str {
        self.agent_name.as_deref().unwrap_or("pi")
    }
}
