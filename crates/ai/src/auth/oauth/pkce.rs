//! 对应上游 `packages/ai/src/auth/oauth/pkce.ts`。
//!
//! PKCE 的 verifier 与 challenge：32 个随机字节做 base64url 得到 verifier，
//! 再用 SHA-256 摘要的 base64url 做 challenge（S256）。

use base64::Engine;
use sha2::{Digest, Sha256};

use crate::auth::types::{ModelsError, ModelsErrorCode};

/// pi: `generatePKCE` 的结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

fn base64url_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// pi: `generatePKCE`
pub fn generate_pkce() -> Result<Pkce, ModelsError> {
    let mut verifier_bytes = [0u8; 32];
    getrandom::getrandom(&mut verifier_bytes).map_err(|error| {
        ModelsError::new(ModelsErrorCode::Auth, "could not read random bytes")
            .with_cause(error.to_string())
    })?;
    let verifier = base64url_encode(&verifier_bytes);
    let challenge = base64url_encode(&Sha256::digest(verifier.as_bytes()));
    Ok(Pkce {
        verifier,
        challenge,
    })
}

/// 上游的 `crypto.randomBytes(16).toString("hex")`（OAuth `state`）。
pub fn random_hex(bytes: usize) -> Result<String, ModelsError> {
    let mut data = vec![0u8; bytes];
    getrandom::getrandom(&mut data).map_err(|error| {
        ModelsError::new(ModelsErrorCode::Auth, "could not read random bytes")
            .with_cause(error.to_string())
    })?;
    Ok(data.iter().map(|byte| format!("{byte:02x}")).collect())
}
