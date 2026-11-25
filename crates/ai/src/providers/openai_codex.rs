//! 对应上游 `packages/ai/src/providers/openai-codex.ts`。
//!
//! GPT 订阅这一家：模型目录原样复制装着的 pi 包里的 `providers/data/openai-codex.json`
//! （10 个模型，含 `gpt-6.2-astra`），接口走 `api/openai_codex_responses.rs`，鉴权是 OAuth。

use serde::Deserialize;

use crate::auth::{openai_codex_oauth, OAuthAuth};
use crate::types::Model;

/// 上游 `openaiCodexProvider()` 的 `baseUrl`。
pub const OPENAI_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";

/// 上游 `createProvider({ id, name, baseUrl, models, auth, api })` 的数据部分。
#[derive(Debug, Clone)]
pub struct OpenAICodexProvider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub models: Vec<Model>,
    pub auth: OAuthAuth,
}

#[derive(Debug, Deserialize)]
struct Catalog {
    #[serde(rename = "openai-codex-responses")]
    openai_codex_responses: std::collections::BTreeMap<String, Model>,
}

/// 目录里的全部模型。
pub fn models() -> Vec<Model> {
    let catalog: Catalog = serde_json::from_str(include_str!("data/openai-codex.json"))
        .expect("the vendored OpenAI Codex catalog is valid JSON");
    catalog.openai_codex_responses.into_values().collect()
}

/// 按 id 取一个模型。
pub fn model(id: &str) -> Option<Model> {
    models().into_iter().find(|model| model.id == id)
}

/// pi: `openaiCodexProvider()`
pub fn openai_codex_provider() -> OpenAICodexProvider {
    OpenAICodexProvider {
        id: "openai-codex".to_string(),
        name: "OpenAI Codex".to_string(),
        base_url: OPENAI_CODEX_BASE_URL.to_string(),
        models: models(),
        auth: openai_codex_oauth(),
    }
}
