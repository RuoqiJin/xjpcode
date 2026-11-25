//! 对应上游 `packages/ai/src/auth/resolve.ts`。
//!
//! 解析一次请求的鉴权，规矩照 pi：
//! - 存着的凭证说了算：有存 OAuth 就用 OAuth，有存钥匙就用钥匙；什么都没存才看环境变量。
//!   续令牌失败、或存的类型没有对应的处理方式，都不会悄悄退到环境变量。
//! - OAuth 令牌剩不到五分钟就在 `modify` 的锁里续一次；锁里再查一遍，别的进程/请求续过就不续。
//! - 续令牌和落盘不受调用方取消影响（只受 15 秒超时约束），否则可能丢掉唯一一份有效的续期令牌。

use std::sync::Arc;

use crate::auth::helpers::{check_optional_signal, OverlayAuthContext};
use crate::auth::types::{
    ApiKeyAuth, ApiKeyCredential, AuthContext, AuthResult, Credential, CredentialStore,
    ModelsError, ModelsErrorCode, OAuthAuth, OAuthCredential,
};
use crate::auth::types::ProviderAuth;
use crate::types::BoxFuture;
use crate::utils::abort::AbortSignal;

/// pi: `AuthResolutionOverrides`
#[derive(Debug, Clone, Default)]
pub struct AuthResolutionOverrides {
    pub api_key: Option<String>,
    pub env: Option<crate::types::ProviderEnv>,
    /// 要求续完的令牌至少还剩这么久（毫秒）；缺省是五分钟。
    pub min_oauth_validity_ms: Option<i64>,
    pub signal: Option<AbortSignal>,
}

const DEFAULT_OAUTH_MINIMUM_VALIDITY_MS: i64 = 5 * 60 * 1000;
const DEFAULT_OAUTH_REFRESH_TIMEOUT_MS: u64 = 15_000;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

/// pi: `resolveProviderAuth`
pub async fn resolve_provider_auth(
    provider_id: &str,
    auth: &ProviderAuth,
    credentials: Arc<dyn CredentialStore>,
    auth_context: Arc<dyn AuthContext>,
    overrides: Option<AuthResolutionOverrides>,
) -> Result<Option<AuthResult>, ModelsError> {
    let signal = overrides
        .as_ref()
        .and_then(|overrides| overrides.signal.clone())
        .unwrap_or_else(AbortSignal::never);
    check_optional_signal(&Some(signal.clone()))?;    let request_context: Arc<dyn AuthContext> =
        match overrides.as_ref().and_then(|overrides| overrides.env.clone()) {
            Some(env) => Arc::new(OverlayAuthContext {
                base: auth_context.as_ref(),
                env,
            }),
            None => auth_context,
        };

    // 显式传进来的钥匙优先（`--api-key`）。
    if let Some(api_key) = overrides
        .as_ref()
        .and_then(|overrides| overrides.api_key.as_ref())
    {
        if let Some(api_key_auth) = &auth.api_key {
            return resolve_api_key(
                request_context.as_ref(),
                api_key_auth,
                provider_id,
                Some(ApiKeyCredential {
                    key: Some(api_key.clone()),
                    env: overrides.as_ref().and_then(|overrides| overrides.env.clone()),
                }),
                &signal,
            )
            .await;
        }
    }

    let stored = read_credential(credentials.as_ref(), provider_id, &signal).await?;
    if let Some(stored) = stored {
        match (&stored, &auth.oauth, &auth.api_key) {
            (Credential::OAuth(credential), Some(oauth), _) => {
                return resolve_stored_oauth(
                    credentials,
                    provider_id,
                    oauth,
                    credential.clone(),
                    &signal,
                    overrides
                        .as_ref()
                        .and_then(|overrides| overrides.min_oauth_validity_ms),
                )
                .await;
            }
            (Credential::ApiKey(credential), _, Some(api_key_auth)) => {
                let credential = match overrides
                    .as_ref()
                    .and_then(|overrides| overrides.env.clone())
                {
                    Some(env) => ApiKeyCredential {
                        key: credential.key.clone(),
                        env: Some({
                            let mut merged = credential.env.clone().unwrap_or_default();
                            merged.extend(env);
                            merged
                        }),
                    },
                    None => credential.clone(),
                };
                return resolve_api_key(
                    request_context.as_ref(),
                    api_key_auth,
                    provider_id,
                    Some(credential),
                    &signal,
                )
                .await;
            }
            _ => return Ok(None),
        }
    }

    match &auth.api_key {
        Some(api_key_auth) => {
            resolve_api_key(
                request_context.as_ref(),
                api_key_auth,
                provider_id,
                None,
                &signal,
            )
            .await
        }
        None => Ok(None),
    }
}

async fn read_credential(
    credentials: &dyn CredentialStore,
    provider_id: &str,
    signal: &AbortSignal,
) -> Result<Option<Credential>, ModelsError> {
    credentials
        .read(provider_id, crate::auth::types::AuthOperationOptions {
            signal: Some(signal.clone()),
        })
        .await
        .map_err(|error| {
            ModelsError::new(
                ModelsErrorCode::Auth,
                format!("Credential store read failed for {provider_id}"),
            )
            .with_cause(error.message)
        })
}

/// pi: `refreshStoredOAuthCredential`
///
/// 在 store 的锁里续、续完先落盘再放锁；`needs_refresh` 在锁里重新查一遍。
/// 取消只影响等锁；续令牌本身只受超时约束。
pub async fn refresh_stored_oauth_credential(
    credentials: Arc<dyn CredentialStore>,
    provider_id: &str,
    oauth: OAuthAuth,
    needs_refresh: Arc<dyn Fn(&OAuthCredential) -> bool + Send + Sync>,
    signal: AbortSignal,
) -> Result<Option<OAuthCredential>, ModelsError> {
    let lock_controller = crate::utils::abort::AbortController::new();
    let lock_signal = lock_controller.signal();
    // 调用方取消只取消等锁；等到的锁里用什么都不取消的信号续令牌。
    let watcher = {
        let caller = signal.clone();
        let lock_controller = lock_controller.clone();
        tokio::spawn(async move {
            caller.cancelled().await;
            lock_controller.abort();
        })
    };
    if signal.aborted() {
        lock_controller.abort();
    }

    let provider = provider_id.to_string();
    let options = crate::auth::types::AuthOperationOptions {
        signal: Some(lock_signal),
    };
    let modify: BoxFuture<'_, Result<Option<Credential>, ModelsError>> = {
        let credentials = Arc::clone(&credentials);
        let oauth = oauth.clone();
        let provider_name = provider.clone();
        let signal = signal.clone();
        Box::pin(async move {
            credentials
                .modify(
                    &provider,
                    Box::new(move |current| {
                        Box::pin(async move {
                            check_optional_signal(&Some(signal.clone()))?;
                            let current = match current {
                                Some(Credential::OAuth(current)) => current,
                                _ => return Ok(None), // 这期间登出了
                            };
                            if !needs_refresh(&current) {
                                return Ok(None); // 别的进程/请求续过了
                            }
                            let refresh_signal = AbortSignal::never();
                            let refreshed = tokio::time::timeout(
                                std::time::Duration::from_millis(DEFAULT_OAUTH_REFRESH_TIMEOUT_MS),
                                (oauth.refresh)(current, refresh_signal),
                            )
                            .await
                            .map_err(|_| {
                                ModelsError::new(
                                    ModelsErrorCode::OAuth,
                                    format!("OAuth refresh failed for {provider_name}"),
                                )
                                .with_cause("refresh timed out")
                            })?;
                            // 上游把 provider 的续期失败包成 code=oauth（保留旧凭证，重新登录能修好）。
                            let refreshed = refreshed.map_err(|error| {
                                ModelsError::new(
                                    ModelsErrorCode::OAuth,
                                    format!("OAuth refresh failed for {provider_name}"),
                                )
                                .with_cause(error.message)
                            })?;
                            Ok(Some(Credential::OAuth(refreshed)))
                        }) as BoxFuture<'_, Result<Option<Credential>, ModelsError>>
                    }),
                    options,
                )
                .await
        })
    };

    let post = match modify.await {
        Ok(post) => post,
        Err(error) => {
            watcher.abort();
            if error.code == ModelsErrorCode::OAuth {
                return Err(error);
            }
            if signal.aborted() {
                return Err(ModelsError::new(
                    ModelsErrorCode::Auth,
                    "Operation was aborted",
                ));
            }
            return Err(ModelsError::new(
                ModelsErrorCode::Auth,
                format!("Credential store modify failed for {provider_id}"),
            )
            .with_cause(error.message));
        }
    };
    watcher.abort();
    Ok(match post {
        Some(Credential::OAuth(credential)) => Some(credential),
        _ => None,
    })
}

/// pi: `resolveStoredOAuth`
async fn resolve_stored_oauth(
    credentials: Arc<dyn CredentialStore>,
    provider_id: &str,
    oauth: &OAuthAuth,
    stored: OAuthCredential,
    signal: &AbortSignal,
    min_oauth_validity_ms: Option<i64>,
) -> Result<Option<AuthResult>, ModelsError> {
    let minimum_validity_ms = DEFAULT_OAUTH_MINIMUM_VALIDITY_MS.max(min_oauth_validity_ms.unwrap_or(0));
    let expires_soon = {
        let minimum_validity_ms = minimum_validity_ms;
        Arc::new(move |credential: &OAuthCredential| {
            now_ms() + minimum_validity_ms >= credential.expires
        }) as Arc<dyn Fn(&OAuthCredential) -> bool + Send + Sync>
    };
    let mut credential = stored;

    if expires_soon(&credential) {
        let post = refresh_stored_oauth_credential(
            Arc::clone(&credentials),
            provider_id,
            oauth.clone(),
            Arc::clone(&expires_soon),
            signal.clone(),
        )
        .await?;
        let Some(post) = post else {
            return Ok(None); // 这期间登出了
        };
        credential = post;
        // 五分钟窗口只触发续一次，不是 provider 的约定；显式要求了最短有效期的调用方
        // （比如导出 bearer token）续完还得够久。
        if min_oauth_validity_ms.is_some() && expires_soon(&credential) {
            return Err(ModelsError::new(
                ModelsErrorCode::OAuth,
                format!("OAuth refresh returned a token that expires too soon for {provider_id}"),
            ));
        }
    }

    match (oauth.to_auth)(credential).await {
        Ok(auth) => Ok(Some(AuthResult {
            auth,
            env: None,
            source: Some("OAuth".to_string()),
        })),
        Err(error) => Err(ModelsError::new(
            ModelsErrorCode::OAuth,
            format!("OAuth auth derivation failed for {provider_id}"),
        )
        .with_cause(error.message)),
    }
}

async fn resolve_api_key(
    auth_context: &dyn AuthContext,
    api_key: &ApiKeyAuth,
    provider_id: &str,
    credential: Option<ApiKeyCredential>,
    signal: &AbortSignal,
) -> Result<Option<AuthResult>, ModelsError> {
    (api_key.resolve)(auth_context, credential, Some(signal.clone()))
        .await
        .map_err(|error| {
            ModelsError::new(
                ModelsErrorCode::Auth,
                format!("API key auth failed for provider {provider_id}"),
            )
            .with_cause(error.message)
        })
}
