//! 对应上游 `packages/ai/src/auth/helpers.ts`。
//!
//! `envApiKeyAuth`：存着的钥匙优先，什么都没存才看环境变量；登录就是问一把钥匙。
//! 上游的 `lazyOAuth` 在 Rust 里没有等价物（没有按需加载），所以 OAuth 由 provider 直接给出。

use std::sync::Arc;

use crate::auth::types::{
    ApiKeyAuth, ApiKeyCredential, ApiKeyLoginFn, ApiKeyResolveFn, AuthContext, AuthResult,
    ModelsError, ProviderAuthInteraction,
};
use crate::types::{BoxFuture, ProviderEnv};

/// pi: `envApiKeyAuth(name, envVars)`
pub fn env_api_key_auth(name: &str, env_vars: &[&str]) -> ApiKeyAuth {
    let login_name = name.to_string();
    let login: ApiKeyLoginFn = Arc::new(move |interaction: &dyn ProviderAuthInteraction| {
        let name = login_name.clone();
        Box::pin(async move {
            check_signal(&interaction.provider_signal())?;
            let key = interaction
                .prompt(
                    crate::auth::types::AuthPrompt::Secret {
                        message: format!("Enter {name}"),
                        placeholder: None,
                    },
                    None,
                )
                .await?;
            check_signal(&interaction.provider_signal())?;
            Ok(ApiKeyCredential {
                key: Some(key),
                env: None,
            })
        }) as BoxFuture<'_, Result<ApiKeyCredential, ModelsError>>
    });

    let env_owned: Vec<String> = env_vars.iter().map(|value| value.to_string()).collect();
    let resolve: ApiKeyResolveFn = Arc::new(
        move |ctx: &dyn AuthContext,
              credential: Option<ApiKeyCredential>,
              signal: Option<crate::utils::abort::AbortSignal>| {
            let env_vars = env_owned.clone();
            Box::pin(async move {
                check_optional_signal(&signal)?;
                if let Some(credential) = credential {
                    if let Some(key) = credential.key.filter(|key| !key.is_empty()) {
                        return Ok(Some(AuthResult {
                            auth: crate::auth::types::ModelAuth {
                                api_key: Some(key),
                                ..Default::default()
                            },
                            env: credential.env,
                            source: Some("stored credential".to_string()),
                        }));
                    }
                }
                for env_var in env_vars {
                    let value = ctx.env(&env_var).await;
                    check_optional_signal(&signal)?;
                    if let Some(value) = value.filter(|value| !value.is_empty()) {
                        return Ok(Some(AuthResult {
                            auth: crate::auth::types::ModelAuth {
                                api_key: Some(value),
                                ..Default::default()
                            },
                            env: None,
                            source: Some(env_var),
                        }));
                    }
                }
                Ok(None)
            }) as BoxFuture<'_, Result<Option<AuthResult>, ModelsError>>
        },
    );

    ApiKeyAuth {
        name: name.to_string(),
        login: Some(login),
        check: None,
        resolve,
    }
}

/// 把一份 `ProviderEnv` 叠在上下文的上层：`ctx.env` 先看它，没有再问原上下文。
/// pi: `overlayEnvAuthContext`
pub struct OverlayAuthContext<'a> {
    pub base: &'a dyn AuthContext,
    pub env: ProviderEnv,
}

impl AuthContext for OverlayAuthContext<'_> {
    fn env<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Option<String>> {
        let overlay = self.env.get(name).cloned().filter(|value| !value.is_empty());
        Box::pin(async move {
            match overlay {
                Some(value) => Some(value),
                None => self.base.env(name).await,
            }
        })
    }

    fn file_exists<'a>(&'a self, path: &'a str) -> BoxFuture<'a, bool> {
        self.base.file_exists(path)
    }
}

pub(crate) fn check_signal(signal: &crate::utils::abort::AbortSignal) -> Result<(), ModelsError> {
    if signal.aborted() {
        Err(ModelsError::login("Login cancelled"))
    } else {
        Ok(())
    }
}

pub(crate) fn check_optional_signal(
    signal: &Option<crate::utils::abort::AbortSignal>,
) -> Result<(), ModelsError> {
    match signal {
        Some(signal) => check_signal(signal),
        None => Ok(()),
    }
}
