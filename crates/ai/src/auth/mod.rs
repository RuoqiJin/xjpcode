//! 对应上游 `packages/ai/src/auth/`：凭证形状、解析规则、环境变量钥匙与三家 OAuth。
//!
//! GPT 订阅（`openai-codex`）这一轮只带它；别家的登录流程没有搬。

pub mod context;
pub mod credential_store;
pub mod helpers;
pub mod oauth;
pub mod resolve;
pub mod types;

pub use context::{DefaultAuthContext, StaticAuthContext};
pub use credential_store::InMemoryCredentialStore;
pub use helpers::env_api_key_auth;
pub use oauth::openai_codex::openai_codex_oauth;
pub use resolve::{refresh_stored_oauth_credential, resolve_provider_auth, AuthResolutionOverrides};
pub use types::{
    ApiKeyAuth, ApiKeyCredential, ApiKeyLoginFn, ApiKeyResolveFn, AuthCheck, AuthContext,
    AuthEvent, AuthInfoLink, AuthInteraction, AuthOperationOptions, AuthPrompt, AuthPromptOption,
    AuthResult, AuthType, Credential, CredentialInfo, CredentialStore, LoginOptions, ModelAuth,
    ModifyCredentialFn,
    ModelsError, ModelsErrorCode, OAuthAuth, OAuthCredential, ProviderAuth,
    ProviderAuthInteraction,
};
