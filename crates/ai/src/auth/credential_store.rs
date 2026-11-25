//! 对应上游 `packages/ai/src/auth/credential-store.ts` 的 `InMemoryCredentialStore`。
//!
//! 进程内的一份凭证存储：按 provider id 一份，写入按 provider 串行。测试与应用启动时
//! 都可以用它；真正落盘的那一份在 `crates/coding-agent/src/core/auth_storage.rs`。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::auth::types::{
    AuthOperationOptions, Credential, CredentialInfo, CredentialStore, ModifyCredentialFn,
    ModelsError,
};
use crate::types::BoxFuture;
use crate::utils::abort::AbortSignal;

fn check(signal: &Option<AbortSignal>) -> Result<(), ModelsError> {
    if signal.as_ref().is_some_and(|signal| signal.aborted()) {
        Err(ModelsError::new(
            crate::auth::types::ModelsErrorCode::Auth,
            "Operation was aborted",
        ))
    } else {
        Ok(())
    }
}

/// pi: `InMemoryCredentialStore`
#[derive(Debug, Default)]
pub struct InMemoryCredentialStore {
    credentials: Mutex<HashMap<String, Credential>>,
    chains: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl InMemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 用一份现成的表建出来（测试）。
    pub fn from_entries(entries: impl IntoIterator<Item = (String, Credential)>) -> Self {
        Self {
            credentials: Mutex::new(entries.into_iter().collect()),
            chains: Mutex::new(HashMap::new()),
        }
    }

    fn chain(&self, provider_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut chains = self.chains.lock().expect("credential chains");
        chains
            .entry(provider_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// 同步看一眼（测试和界面展示用，不经过锁）。
    pub fn peek(&self, provider_id: &str) -> Option<Credential> {
        self.credentials
            .lock()
            .expect("credentials")
            .get(provider_id)
            .cloned()
    }
}

impl CredentialStore for InMemoryCredentialStore {
    fn read<'a>(
        &'a self,
        provider_id: &'a str,
        options: AuthOperationOptions,
    ) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>> {
        Box::pin(async move {
            check(&options.signal)?;
            Ok(self.peek(provider_id))
        })
    }

    fn list<'a>(
        &'a self,
        options: AuthOperationOptions,
    ) -> BoxFuture<'a, Result<Vec<CredentialInfo>, ModelsError>> {
        Box::pin(async move {
            check(&options.signal)?;
            let credentials = self.credentials.lock().expect("credentials");
            let mut entries: Vec<CredentialInfo> = credentials
                .iter()
                .map(|(provider_id, credential)| CredentialInfo {
                    provider_id: provider_id.clone(),
                    r#type: credential.r#type(),
                })
                .collect();
            entries.sort_by(|left, right| left.provider_id.cmp(&right.provider_id));
            Ok(entries)
        })
    }

    fn modify<'a>(
        &'a self,
        provider_id: &'a str,
        f: ModifyCredentialFn<'a>,
        options: AuthOperationOptions,
    ) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>> {
        Box::pin(async move {
            check(&options.signal)?;
            let lock = self.chain(provider_id);
            let _guard = lock.lock().await;
            check(&options.signal)?;
            let current = self.peek(provider_id);
            let next = f(current.clone()).await?;
            check(&options.signal)?;
            if let Some(next) = &next {
                self.credentials
                    .lock()
                    .expect("credentials")
                    .insert(provider_id.to_string(), next.clone());
            }
            Ok(next.or(current))
        })
    }

    fn delete<'a>(
        &'a self,
        provider_id: &'a str,
        options: AuthOperationOptions,
    ) -> BoxFuture<'a, Result<(), ModelsError>> {
        Box::pin(async move {
            check(&options.signal)?;
            let lock = self.chain(provider_id);
            let _guard = lock.lock().await;
            check(&options.signal)?;
            self.credentials
                .lock()
                .expect("credentials")
                .remove(provider_id);
            Ok(())
        })
    }
}
