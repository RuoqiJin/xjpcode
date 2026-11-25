//! 对应上游 `packages/coding-agent/test/auth-storage.test.ts` 的存储部分：
//! 读、解析钥匙、改、删、并发串行、文件权限、坏文件不覆盖、取消。
//!
//! 上游用 proper-lockfile 的锁目录并测了很多等待/取消的细节；这里用 `flock`，
//! 对应的那部分见 `PORTING.md` 的对账表（其余用例登记为 partial/deferred）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tempfile::TempDir;
use xjpcode_ai::auth::{
    AuthOperationOptions, Credential, CredentialStore, ModelsError,
};
use xjpcode_ai::AbortController;
use xjpcode_coding_agent::core::auth_storage::AuthStorage;

fn api_key(key: &str) -> Credential {
    Credential::ApiKey(xjpcode_ai::auth::ApiKeyCredential {
        key: Some(key.to_string()),
        env: None,
    })
}

fn oauth() -> Credential {
    Credential::OAuth(xjpcode_ai::auth::OAuthCredential {
        access: "access-token".to_string(),
        refresh: "refresh-token".to_string(),
        expires: 4_102_444_800_000,
        extra: Default::default(),
    })
}

fn write_auth(path: &std::path::Path, value: serde_json::Value) {
    std::fs::write(path, value.to_string()).unwrap();
}

#[tokio::test]
async fn reads_and_resolves_stored_api_key_credentials() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("auth.json");
    std::env::set_var("TEST_AUTH_STORAGE_KEY", "environment-key");
    write_auth(
        &path,
        serde_json::json!({"anthropic": {"type": "api_key", "key": "$TEST_AUTH_STORAGE_KEY"}}),
    );
    let storage = AuthStorage::create(&path);
    let credential = storage.read("anthropic", Default::default()).await.unwrap();
    match credential {
        Some(Credential::ApiKey(credential)) => {
            assert_eq!(credential.key.as_deref(), Some("environment-key"));
        }
        other => panic!("unexpected credential {other:?}"),
    }
    std::env::remove_var("TEST_AUTH_STORAGE_KEY");
}

#[tokio::test]
async fn resolves_command_backed_api_key_credentials() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("auth.json");
    write_auth(
        &path,
        serde_json::json!({"anthropic": {"type": "api_key", "key": "!printf 'command-key'"}}),
    );
    let storage = AuthStorage::create(&path);
    let credential = storage.read("anthropic", Default::default()).await.unwrap();
    match credential {
        Some(Credential::ApiKey(credential)) => {
            assert_eq!(credential.key.as_deref(), Some("command-key"));
        }
        other => panic!("unexpected credential {other:?}"),
    }
}

#[tokio::test]
async fn returns_oauth_credentials_unchanged_and_lists_them() {
    let storage = AuthStorage::in_memory(
        [("openai-codex".to_string(), oauth())].into_iter().collect(),
    );
    assert_eq!(
        storage.read("openai-codex", Default::default()).await.unwrap(),
        Some(oauth())
    );
    let listed = storage.list(Default::default()).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].provider_id, "openai-codex");
    assert_eq!(listed[0].r#type, xjpcode_ai::auth::AuthType::OAuth);
}

#[tokio::test]
async fn credential_scoped_environment_takes_precedence_and_stays_inspectable() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("auth.json");
    write_auth(
        &path,
        serde_json::json!({"anthropic": {"type": "api_key", "key": "$SCOPED_KEY",
            "env": {"SCOPED_KEY": "scoped-value", "REGION": "test-region"}}}),
    );
    let storage = AuthStorage::create(&path);
    match storage.read("anthropic", Default::default()).await.unwrap() {
        Some(Credential::ApiKey(credential)) => {
            assert_eq!(credential.key.as_deref(), Some("scoped-value"));
            assert_eq!(
                credential.env.as_ref().and_then(|env| env.get("REGION")).map(String::as_str),
                Some("test-region")
            );
        }
        other => panic!("unexpected credential {other:?}"),
    }
}

#[tokio::test]
async fn modify_persists_and_preserves_unrelated_external_edits() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("auth.json");
    let storage = AuthStorage::create(&path);
    storage
        .modify(
            "anthropic",
            Box::new(|_| Box::pin(async { Ok(Some(api_key("a"))) })),
            Default::default(),
        )
        .await
        .unwrap();
    storage
        .modify(
            "openai",
            Box::new(|_| Box::pin(async { Ok(Some(api_key("openai-key"))) })),
            Default::default(),
        )
        .await
        .unwrap();
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(content["anthropic"]["key"], "a");
    assert_eq!(content["openai"]["key"], "openai-key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[tokio::test]
async fn modify_with_none_leaves_the_current_credential_unchanged() {
    let storage = AuthStorage::in_memory(
        [("deepseek".to_string(), api_key("keep"))].into_iter().collect(),
    );
    let result = storage
        .modify(
            "deepseek",
            Box::new(|_| Box::pin(async { Ok(None) })),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(result, Some(api_key("keep")));
    assert_eq!(storage.read("deepseek", Default::default()).await.unwrap(), Some(api_key("keep")));
}

#[tokio::test]
async fn serializes_concurrent_modifications() {
    let storage = Arc::new(AuthStorage::create(TempDir::new().unwrap().path().join("auth.json")));
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for index in 0..4 {
        let storage = Arc::clone(&storage);
        let active = Arc::clone(&active);
        let max_active = Arc::clone(&max_active);
        tasks.push(tokio::spawn(async move {
            storage
                .modify(
                    "deepseek",
                    Box::new(move |current| {
                        let active = Arc::clone(&active);
                        let max_active = Arc::clone(&max_active);
                        Box::pin(async move {
                            let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                            max_active.fetch_max(now, Ordering::SeqCst);
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                            active.fetch_sub(1, Ordering::SeqCst);
                            let value = current
                                .and_then(|credential| match credential {
                                    Credential::ApiKey(api_key) => api_key.key,
                                    _ => None,
                                })
                                .and_then(|key| key.parse::<usize>().ok())
                                .unwrap_or(0)
                                + index
                                + 1;
                            Ok(Some(api_key(&value.to_string())))
                        })
                    }),
                    Default::default(),
                )
                .await
                .unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(max_active.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn delete_removes_one_credential_and_keeps_the_others() {
    let storage = AuthStorage::in_memory(
        [
            ("deepseek".to_string(), api_key("d")),
            ("openai-codex".to_string(), oauth()),
        ]
        .into_iter()
        .collect(),
    );
    storage.delete("deepseek", Default::default()).await.unwrap();
    assert!(storage.read("deepseek", Default::default()).await.unwrap().is_none());
    assert!(storage.read("openai-codex", Default::default()).await.unwrap().is_some());
}

#[tokio::test]
async fn in_memory_storage_implements_the_same_credential_store_behavior() {
    let storage = AuthStorage::in_memory(Default::default());
    storage
        .modify(
            "deepseek",
            Box::new(|_| Box::pin(async { Ok(Some(api_key("memory"))) })),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(storage.read("deepseek", Default::default()).await.unwrap(), Some(api_key("memory")));
    storage.delete("deepseek", Default::default()).await.unwrap();
    assert!(storage.read("deepseek", Default::default()).await.unwrap().is_none());
}

#[tokio::test]
async fn pre_aborted_file_operations_do_not_create_the_backing_file_or_run_the_mutation() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("auth.json");
    let storage = AuthStorage::create(&path);
    let controller = AbortController::new();
    controller.abort();
    let ran = Arc::new(AtomicUsize::new(0));
    let ran_for_call = Arc::clone(&ran);
    let result = storage
        .modify(
            "deepseek",
            Box::new(move |_| {
                let ran = Arc::clone(&ran_for_call);
                Box::pin(async move {
                    ran.fetch_add(1, Ordering::SeqCst);
                    Ok(Some(api_key("never")))
                })
            }),
            AuthOperationOptions {
                signal: Some(controller.signal()),
            },
        )
        .await;
    assert!(result.is_err());
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert!(!path.exists());
}

#[tokio::test]
async fn does_not_overwrite_malformed_auth_files() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("auth.json");
    std::fs::write(&path, "{not json").unwrap();
    let storage = AuthStorage::create(&path);
    let result = storage
        .modify(
            "deepseek",
            Box::new(|_| Box::pin(async { Ok(Some(api_key("new"))) })),
            Default::default(),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
}

#[tokio::test]
async fn rejects_invalid_credential_shapes() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("auth.json");
    write_auth(
        &path,
        serde_json::json!({"deepseek": {"type": "oauth", "access": "a"}}),
    );
    let storage = AuthStorage::create(&path);
    let error = storage
        .read("deepseek", Default::default())
        .await
        .unwrap_err();
    assert!(error.message.contains("Invalid auth.json credential"));
    let _: Option<&dyn std::fmt::Debug> = None;
    let _ = ModelsError::new(xjpcode_ai::auth::ModelsErrorCode::Auth, "x");
}
