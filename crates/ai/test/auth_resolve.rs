//! 对应上游 `packages/ai/test/models-runtime.test.ts` 与 `oauth-auth.test.ts` 里的解析规则：
//! 存着的凭证优先、什么都没存才看环境变量、OAuth 剩不到五分钟在锁里续一次、
//! 续失败保留旧凭证、显式要求的最短有效期。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use xjpcode_ai::auth::{
    env_api_key_auth, resolve_provider_auth, ApiKeyCredential, AuthContext, AuthResolutionOverrides,
    Credential, CredentialStore, InMemoryCredentialStore, ModelAuth, ModelsErrorCode, OAuthAuth,
    OAuthCredential, ProviderAuth,
};
use xjpcode_ai::types::BoxFuture;
use xjpcode_ai::AbortSignal;

static CONTEXT: std::sync::LazyLock<Arc<dyn AuthContext>> =
    std::sync::LazyLock::new(|| {
        Arc::new(xjpcode_ai::auth::StaticAuthContext {
            env: [("DEEPSEEK_API_KEY".to_string(), "env-key".to_string())].into(),
            files: Default::default(),
        })
    });

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn oauth_with(
    refresh_count: Arc<AtomicUsize>,
    refreshed_expires: i64,
) -> OAuthAuth {
    OAuthAuth {
        name: "Test OAuth".to_string(),
        is_subscription: true,
        login_label: None,
        login: Arc::new(|_interaction, _options| {
            Box::pin(async {
                Err(xjpcode_ai::auth::ModelsError::login("not used"))
            })
        }),
        refresh: Arc::new(move |credential: OAuthCredential, _signal: AbortSignal| {
            let refresh_count = Arc::clone(&refresh_count);
            Box::pin(async move {
                refresh_count.fetch_add(1, Ordering::SeqCst);
                Ok(OAuthCredential {
                    access: format!("refreshed-{}", credential.access),
                    refresh: "new-refresh".to_string(),
                    expires: refreshed_expires,
                    extra: Default::default(),
                })
            })
        }),
        to_auth: Arc::new(|credential: OAuthCredential| {
            Box::pin(async move {
                Ok(ModelAuth {
                    api_key: Some(credential.access),
                    ..Default::default()
                })
            })
        }),
    }
}

fn api_key_provider() -> ProviderAuth {
    ProviderAuth {
        api_key: Some(env_api_key_auth("DeepSeek API key", &["DEEPSEEK_API_KEY"])),
        oauth: None,
    }
}

#[tokio::test]
async fn a_stored_key_wins_over_the_environment() {
    let store = InMemoryCredentialStore::new();
    store
        .modify(
            "deepseek",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::ApiKey(ApiKeyCredential {
                        key: Some("stored-key".to_string()),
                        env: None,
                    })))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();
    let result = resolve_provider_auth(
        "deepseek",
        &api_key_provider(),
        Arc::new(store),
        Arc::clone(&CONTEXT),
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.auth.api_key.as_deref(), Some("stored-key"));
    assert_eq!(result.source.as_deref(), Some("stored credential"));
}

#[tokio::test]
async fn the_environment_is_consulted_only_when_nothing_is_stored() {
    let result = resolve_provider_auth(
        "deepseek",
        &api_key_provider(),
        Arc::new(InMemoryCredentialStore::new()),
        Arc::clone(&CONTEXT),
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.auth.api_key.as_deref(), Some("env-key"));
    assert_eq!(result.source.as_deref(), Some("DEEPSEEK_API_KEY"));

    let missing = ProviderAuth {
        api_key: Some(env_api_key_auth("Other", &["OTHER_KEY"])),
        oauth: None,
    };
    assert!(resolve_provider_auth(
        "other",
        &missing,
        Arc::new(InMemoryCredentialStore::new()),
        Arc::clone(&CONTEXT),
        None,
    )
    .await
    .unwrap()
    .is_none());
}

#[tokio::test]
async fn an_expiring_oauth_token_is_refreshed_once_and_persisted() {
    let store = Arc::new(InMemoryCredentialStore::new());
    store
        .modify(
            "codex",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential {
                        access: "old".to_string(),
                        refresh: "old-refresh".to_string(),
                        expires: now_ms() + 60_000, // 剩一分钟：在五分钟窗口里
                        extra: Default::default(),
                    })))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();
    let refresh_count = Arc::new(AtomicUsize::new(0));
    let auth = ProviderAuth {
        api_key: None,
        oauth: Some(oauth_with(Arc::clone(&refresh_count), now_ms() + 3_600_000)),
    };
    let first = resolve_provider_auth(
        "codex",
        &auth,
        Arc::clone(&store) as Arc<dyn CredentialStore>,
        Arc::clone(&CONTEXT),
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(first.auth.api_key.as_deref(), Some("refreshed-old"));
    assert_eq!(first.source.as_deref(), Some("OAuth"));
    assert_eq!(refresh_count.load(Ordering::SeqCst), 1);

    // 续完的令牌还够久，第二次不再续。
    let second = resolve_provider_auth(
        "codex",
        &auth,
        Arc::clone(&store) as Arc<dyn CredentialStore>,
        Arc::clone(&CONTEXT),
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(second.auth.api_key.as_deref(), Some("refreshed-old"));
    assert_eq!(refresh_count.load(Ordering::SeqCst), 1);

    let stored = store.peek("codex").unwrap();
    match stored {
        Credential::OAuth(stored) => {
            assert_eq!(stored.access, "refreshed-old");
            assert_eq!(stored.refresh, "new-refresh");
        }
        _ => panic!("expected an OAuth credential"),
    }
}

#[tokio::test]
async fn a_failed_refresh_is_code_oauth_and_keeps_the_stored_credential() {
    let store = Arc::new(InMemoryCredentialStore::new());
    store
        .modify(
            "codex",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential {
                        access: "old".to_string(),
                        refresh: "old-refresh".to_string(),
                        expires: now_ms(),
                        extra: Default::default(),
                    })))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();
    let auth = ProviderAuth {
        api_key: None,
        oauth: Some(OAuthAuth {
            name: "Broken".to_string(),
            is_subscription: true,
            login_label: None,
            login: Arc::new(|_, _| Box::pin(async { Err(xjpcode_ai::auth::ModelsError::login("x")) })),
            refresh: Arc::new(|_, _| {
                Box::pin(async {
                    Err(xjpcode_ai::auth::ModelsError::new(
                        ModelsErrorCode::Auth,
                        "invalid_grant",
                    ))
                })
            }),
            to_auth: Arc::new(|credential| {
                Box::pin(async move {
                    Ok(ModelAuth {
                        api_key: Some(credential.access),
                        ..Default::default()
                    })
                })
            }),
        }),
    };
    let error = resolve_provider_auth(
        "codex",
        &auth,
        Arc::clone(&store) as Arc<dyn CredentialStore>,
        Arc::clone(&CONTEXT),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ModelsErrorCode::OAuth);
    assert!(store.peek("codex").is_some(), "the old credential is preserved");
}

#[tokio::test]
async fn an_explicit_minimum_validity_rejects_a_token_that_still_expires_soon() {
    let store = Arc::new(InMemoryCredentialStore::new());
    store
        .modify(
            "codex",
            Box::new(|_| {
                Box::pin(async {
                    Ok(Some(Credential::OAuth(OAuthCredential {
                        access: "old".to_string(),
                        refresh: "old-refresh".to_string(),
                        expires: now_ms(),
                        extra: Default::default(),
                    })))
                })
            }),
            Default::default(),
        )
        .await
        .unwrap();
    let auth = ProviderAuth {
        api_key: None,
        oauth: Some(oauth_with(Arc::new(AtomicUsize::new(0)), now_ms() + 60_000)),
    };
    let error = resolve_provider_auth(
        "codex",
        &auth,
        Arc::clone(&store) as Arc<dyn CredentialStore>,
        Arc::clone(&CONTEXT),
        Some(AuthResolutionOverrides {
            min_oauth_validity_ms: Some(10 * 60 * 1000),
            ..Default::default()
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ModelsErrorCode::OAuth);
    assert!(error.message.contains("expires too soon"));
}

#[tokio::test]
async fn an_explicit_api_key_override_wins() {
    let result = resolve_provider_auth(
        "deepseek",
        &api_key_provider(),
        Arc::new(InMemoryCredentialStore::new()),
        Arc::clone(&CONTEXT),
        Some(AuthResolutionOverrides {
            api_key: Some("inline-key".to_string()),
            ..Default::default()
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.auth.api_key.as_deref(), Some("inline-key"));
}

/// 一个读就失败的凭证库：错误要落成 code=auth。
struct BrokenStore;

impl CredentialStore for BrokenStore {
    fn read<'a>(
        &'a self,
        _provider_id: &'a str,
        _options: xjpcode_ai::auth::AuthOperationOptions,
    ) -> BoxFuture<'a, Result<Option<Credential>, xjpcode_ai::auth::ModelsError>> {
        Box::pin(async { Err(xjpcode_ai::auth::ModelsError::new(ModelsErrorCode::Auth, "disk")) })
    }

    fn list<'a>(
        &'a self,
        _options: xjpcode_ai::auth::AuthOperationOptions,
    ) -> BoxFuture<'a, Result<Vec<xjpcode_ai::auth::CredentialInfo>, xjpcode_ai::auth::ModelsError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn modify<'a>(
        &'a self,
        _provider_id: &'a str,
        _f: xjpcode_ai::auth::ModifyCredentialFn<'a>,
        _options: xjpcode_ai::auth::AuthOperationOptions,
    ) -> BoxFuture<'a, Result<Option<Credential>, xjpcode_ai::auth::ModelsError>> {
        Box::pin(async { Ok(None) })
    }

    fn delete<'a>(
        &'a self,
        _provider_id: &'a str,
        _options: xjpcode_ai::auth::AuthOperationOptions,
    ) -> BoxFuture<'a, Result<(), xjpcode_ai::auth::ModelsError>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn a_credential_store_failure_is_code_auth() {
    let error = resolve_provider_auth(
        "deepseek",
        &api_key_provider(),
        Arc::new(BrokenStore),
        Arc::clone(&CONTEXT),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ModelsErrorCode::Auth);
    assert!(error.message.contains("Credential store read failed"));
    let _ = Mutex::new(());
}
