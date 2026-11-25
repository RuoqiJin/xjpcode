//! 对应上游 `packages/coding-agent/src/core/auth-storage.ts`（文件存储与内存存储两条路）。
//!
//! `auth.json` 是一张 `{ providerId: Credential }` 表，权限 600；写入时先加文件锁（`flock`），
//! 读-改-写整段在锁里，所以两个进程不会互相覆盖。`modify` 是唯一写入路径。
//!
//! 与上游不同的：
//! - 锁用 `flock`（上游用 `proper-lockfile` 的锁目录），拿不到时每 10 毫秒重试、上限 30 秒；
//!   取消只取消等锁，拿到锁之后不会再写。
//! - 不缓存「上一次读到的内容」的修订号；每次读都在锁里读文件（结果一样，少了跨实例合并那一层）。

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use xjpcode_ai::auth::{
    AuthOperationOptions, AuthType, Credential, CredentialInfo, CredentialStore, ModelsError,
    ModelsErrorCode,
};
use xjpcode_ai::types::BoxFuture;
use xjpcode_ai::AbortSignal;

use crate::core::resolve_config_value::resolve_config_value;

type AuthStorageData = BTreeMap<String, Credential>;

const AUTH_FILE_MODE: u32 = 0o600;
const AUTH_DIR_MODE: u32 = 0o700;
const LOCK_RETRY: std::time::Duration = std::time::Duration::from_millis(10);
const LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn storage_error(message: impl Into<String>) -> ModelsError {
    ModelsError::new(ModelsErrorCode::Auth, message)
}

fn aborted() -> ModelsError {
    storage_error("Operation was aborted")
}

fn check_signal(signal: &Option<AbortSignal>) -> Result<(), ModelsError> {
    match signal {
        Some(signal) if signal.aborted() => Err(aborted()),
        _ => Ok(()),
    }
}

/// pi: `AuthStorageData` 的解析。空内容就是空表；不是对象或某条坏了都报同一句话。
fn parse_storage_data(content: &str) -> Result<AuthStorageData, ModelsError> {
    if content.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    let parsed: Value = serde_json::from_str(content.trim_start_matches('\u{feff}'))
        .map_err(|error| storage_error(format!("Failed to read auth.json: {error}")))?;
    let Value::Object(entries) = parsed else {
        return Err(storage_error("Invalid auth.json: expected an object"));
    };
    for (provider_id, credential) in &entries {
        let invalid = || {
            storage_error(format!(
                "Invalid auth.json credential for provider \"{provider_id}\""
            ))
        };
        let Value::Object(value) = credential else {
            return Err(invalid());
        };
        match value.get("type").and_then(Value::as_str) {
            Some("api_key") => {
                let valid_key = match value.get("key") {
                    None | Some(Value::Null) => true,
                    Some(Value::String(_)) => true,
                    Some(_) => false,
                };
                let valid_env = match value.get("env") {
                    None | Some(Value::Null) => true,
                    Some(Value::Object(env)) => {
                        env.values().all(|entry| entry.is_string())
                    }
                    Some(_) => false,
                };
                if !(valid_key && valid_env) {
                    return Err(invalid());
                }
            }
            Some("oauth") => {
                let valid = value.get("access").and_then(Value::as_str).is_some()
                    && value.get("refresh").and_then(Value::as_str).is_some()
                    && value
                        .get("expires")
                        .and_then(Value::as_f64)
                        .map(f64::is_finite)
                        .unwrap_or(false);
                if !valid {
                    return Err(invalid());
                }
            }
            _ => return Err(invalid()),
        }
    }
    serde_json::from_value(Value::Object(entries))
        .map_err(|error| storage_error(format!("Invalid auth.json: {error}")))
}

fn serialize_storage_data(data: &AuthStorageData) -> String {
    serde_json::to_string_pretty(data).expect("credentials serialize")
}

/// 读一份凭证：api_key 的 `key` 可能写成 `$ENV` 或 `!命令`，读的时候解出来。
fn resolve_credential(credential: Credential) -> Credential {
    match credential {
        Credential::ApiKey(mut api_key) => {
            // 上游的 `AuthStorage.read`：钥匙可以写成 `$ENV` 或 `!命令`，读的时候解出来。
            if let Some(key) = api_key.key.clone() {
                api_key.key = resolve_config_value(&key, api_key.env.as_ref());
            }
            Credential::ApiKey(api_key)
        }
        other => other,
    }
}

/// pi: `FileAuthStorageBackend` 的锁与读写部分。
struct FileBackend {
    auth_path: PathBuf,
}

impl FileBackend {
    fn new(auth_path: impl Into<PathBuf>) -> Self {
        Self {
            auth_path: auth_path.into(),
        }
    }

    fn ensure_parent_dir(&self) -> Result<(), ModelsError> {
        let Some(parent) = self.auth_path.parent() else {
            return Ok(());
        };
        if !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|error| storage_error(format!("Failed to create auth.json dir: {error}")))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(AUTH_DIR_MODE));
            }
        }
        Ok(())
    }

    fn open_lock_file(&self) -> Result<File, ModelsError> {
        if !self.auth_path.exists() {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(&self.auth_path)
                .map_err(|error| storage_error(format!("Failed to write auth.json: {error}")))?;
            file.write_all(b"{}")
                .map_err(|error| storage_error(format!("Failed to write auth.json: {error}")))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(
                    &self.auth_path,
                    std::fs::Permissions::from_mode(AUTH_FILE_MODE),
                );
            }
        }
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.auth_path)
            .map_err(|error| storage_error(format!("Failed to read auth.json: {error}")))
    }

    /// 等锁：每 10 毫秒试一次 `flock(LOCK_EX | LOCK_NB)`，最多 30 秒；收到取消就放手。
    async fn acquire(&self, signal: &Option<AbortSignal>) -> Result<File, ModelsError> {
        check_signal(signal)?;
        let file = self.open_lock_file()?;
        let deadline = std::time::Instant::now() + LOCK_TIMEOUT;
        loop {
            check_signal(signal)?;
            if try_lock(&file)? {
                check_signal(signal)?;
                return Ok(file);
            }
            if std::time::Instant::now() >= deadline {
                return Err(storage_error("Timed out waiting for the auth.json lock"));
            }
            tokio::time::sleep(LOCK_RETRY).await;
        }
    }

    fn read_all(file: &mut File) -> Result<String, ModelsError> {
        file.seek(SeekFrom::Start(0))
            .map_err(|error| storage_error(format!("Failed to read auth.json: {error}")))?;
        let mut content = String::new();
        file.read_to_string(&mut content)
            .map_err(|error| storage_error(format!("Failed to read auth.json: {error}")))?;
        Ok(content)
    }

    fn write_all(file: &mut File, content: &str) -> Result<(), ModelsError> {
        file.seek(SeekFrom::Start(0))
            .map_err(|error| storage_error(format!("Failed to write auth.json: {error}")))?;
        file.set_len(0)
            .map_err(|error| storage_error(format!("Failed to write auth.json: {error}")))?;
        file.write_all(content.as_bytes())
            .map_err(|error| storage_error(format!("Failed to write auth.json: {error}")))?;
        file.flush()
            .map_err(|error| storage_error(format!("Failed to write auth.json: {error}")))?;
        Ok(())
    }
}

#[cfg(unix)]
fn try_lock(file: &File) -> Result<bool, ModelsError> {
    use std::os::unix::io::AsRawFd;
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(false)
    } else {
        Err(storage_error(format!("Failed to lock auth.json: {error}")))
    }
}

#[cfg(not(unix))]
fn try_lock(_file: &File) -> Result<bool, ModelsError> {
    Ok(true)
}

/// pi: `InMemoryAuthStorageBackend`
#[derive(Default)]
struct InMemoryBackend {
    value: Mutex<Option<String>>,
    chain: tokio::sync::Mutex<()>,
}

enum Backend {
    File(FileBackend),
    Memory(Arc<InMemoryBackend>),
}

/// pi: `AuthStorage`
pub struct AuthStorage {
    backend: Backend,
}

impl AuthStorage {
    /// pi: `AuthStorage.create(authPath)`
    pub fn create(auth_path: impl Into<PathBuf>) -> Self {
        Self {
            backend: Backend::File(FileBackend::new(auth_path)),
        }
    }

    /// pi: `AuthStorage.inMemory(data)`
    pub fn in_memory(data: AuthStorageData) -> Self {
        Self {
            backend: Backend::Memory(Arc::new(InMemoryBackend {
                value: Mutex::new(Some(serialize_storage_data(&data))),
                chain: tokio::sync::Mutex::new(()),
            })),
        }
    }

    pub fn empty_in_memory() -> Self {
        Self::in_memory(BTreeMap::new())
    }

    /// 缺省位置：`<agentDir>/auth.json`。
    pub fn default_at(agent_dir: &Path) -> Self {
        Self::create(agent_dir.join("auth.json"))
    }

    pub fn path(&self) -> Option<&Path> {
        match &self.backend {
            Backend::File(backend) => Some(&backend.auth_path),
            Backend::Memory(_) => None,
        }
    }

    async fn with_content<T>(
        &self,
        options: AuthOperationOptions,
        mut f: impl FnMut(Option<&str>) -> Result<(T, Option<String>), ModelsError>,
    ) -> Result<T, ModelsError> {
        check_signal(&options.signal)?;
        match &self.backend {
            Backend::File(backend) => {
                backend.ensure_parent_dir()?;
                let mut file = backend.acquire(&options.signal).await?;
                let content = FileBackend::read_all(&mut file)?;
                let (result, next) = f(Some(&content))?;
                check_signal(&options.signal)?;
                if let Some(next) = next {
                    FileBackend::write_all(&mut file, &next)?;
                }
                Ok(result)
            }
            Backend::Memory(backend) => {
                let _guard = backend.chain.lock().await;
                check_signal(&options.signal)?;
                let content = backend.value.lock().expect("auth value").clone();
                let (result, next) = f(content.as_deref())?;
                check_signal(&options.signal)?;
                if let Some(next) = next {
                    *backend.value.lock().expect("auth value") = Some(next);
                }
                Ok(result)
            }
        }
    }

    async fn read_data(
        &self,
        options: AuthOperationOptions,
    ) -> Result<AuthStorageData, ModelsError> {
        self.with_content(options, |content| {
            let data = parse_storage_data(content.unwrap_or(""))?;
            Ok((data, None))
        })
        .await
    }
}

impl CredentialStore for AuthStorage {
    fn read<'a>(
        &'a self,
        provider_id: &'a str,
        options: AuthOperationOptions,
    ) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>> {
        Box::pin(async move {
            let data = self.read_data(options).await?;
            Ok(data.get(provider_id).cloned().map(resolve_credential))
        })
    }

    fn list<'a>(
        &'a self,
        options: AuthOperationOptions,
    ) -> BoxFuture<'a, Result<Vec<CredentialInfo>, ModelsError>> {
        Box::pin(async move {
            let data = self.read_data(options).await?;
            Ok(data
                .into_iter()
                .map(|(provider_id, credential)| CredentialInfo {
                    provider_id,
                    r#type: match credential {
                        Credential::ApiKey(_) => AuthType::ApiKey,
                        Credential::OAuth(_) => AuthType::OAuth,
                    },
                })
                .collect())
        })
    }

    fn modify<'a>(
        &'a self,
        provider_id: &'a str,
        f: xjpcode_ai::auth::ModifyCredentialFn<'a>,
        options: AuthOperationOptions,
    ) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>> {
        Box::pin(async move {
            check_signal(&options.signal)?;
            match &self.backend {
                Backend::File(backend) => {
                    backend.ensure_parent_dir()?;
                    // 锁一直拿到写入完（含 f 的网络续令牌），两个进程不会同时续。
                    let mut file = backend.acquire(&options.signal).await?;
                    let content = FileBackend::read_all(&mut file)?;
                    let mut data = parse_storage_data(&content)?;
                    let current = data.get(provider_id).cloned();
                    let next = f(current.clone()).await?;
                    check_signal(&options.signal)?;
                    if let Some(next) = &next {
                        data.insert(provider_id.to_string(), next.clone());
                        FileBackend::write_all(&mut file, &serialize_storage_data(&data))?;
                    }
                    Ok(next.or(current).map(resolve_credential))
                }
                Backend::Memory(backend) => {
                    let _guard = backend.chain.lock().await;
                    check_signal(&options.signal)?;
                    let content = backend.value.lock().expect("auth value").clone();
                    let mut data = parse_storage_data(content.as_deref().unwrap_or(""))?;
                    let current = data.get(provider_id).cloned();
                    let next = f(current.clone()).await?;
                    check_signal(&options.signal)?;
                    if let Some(next) = &next {
                        data.insert(provider_id.to_string(), next.clone());
                        *backend.value.lock().expect("auth value") =
                            Some(serialize_storage_data(&data));
                    }
                    Ok(next.or(current).map(resolve_credential))
                }
            }
        })
    }

    fn delete<'a>(
        &'a self,
        provider_id: &'a str,
        options: AuthOperationOptions,
    ) -> BoxFuture<'a, Result<(), ModelsError>> {
        Box::pin(async move {
            let provider = provider_id.to_string();
            self.with_content(options, move |content| {
                let mut data = parse_storage_data(content.unwrap_or(""))?;
                data.remove(&provider);
                Ok(((), Some(serialize_storage_data(&data))))
            })
            .await?;
            Ok(())
        })
    }
}

/// pi: `readStoredCredential`——不走存储对象的一次性读取（不解析 `$ENV`）。
pub fn read_stored_credential(provider_id: &str, auth_path: &Path) -> Option<Credential> {
    let content = std::fs::read_to_string(auth_path).ok()?;
    let data = parse_storage_data(&content).ok()?;
    data.get(provider_id).cloned()
}
