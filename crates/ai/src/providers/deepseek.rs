//! 对应上游 `packages/ai/src/providers/deepseek.ts`。
//!
//! 上游用 `createProvider` 把 id、名字、baseUrl、鉴权与模型目录装成一个 provider；本次只接
//! 模型目录与请求参数（鉴权由调用方传 `api_key`，没有搬 auth 包）。
//! 模型目录原样复制上游的 `providers/data/deepseek.json`。

use serde::Deserialize;

use crate::types::Model;

/// 上游 `deepseekProvider()` 的 `baseUrl`。
pub const DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";

/// pi: `createProvider({ id, name, baseUrl, models, api })` 的数据部分。
#[derive(Debug, Clone)]
pub struct DeepSeekProvider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub models: Vec<Model>,
}

/// 上游的模型目录（`flattenChatModelCatalog("deepseek", values)`）。
///
/// 目录的形状是 `{ api: { modelId: model } }`；`flattenModelCatalog` 丢掉 `type` 不是 `chat`
/// 的项，并按 `id` 放回一张表。这里保持同一份 JSON 与同一条规整。
#[derive(Debug, Deserialize)]
struct Catalog {
    #[serde(rename = "openai-completions")]
    openai_completions: std::collections::BTreeMap<String, Model>,
}

/// 目录里的全部模型。
pub fn models() -> Vec<Model> {
    let catalog: Catalog = serde_json::from_str(include_str!("data/deepseek.json"))
        .expect("the vendored DeepSeek catalog is valid JSON");
    catalog.openai_completions.into_values().collect()
}

/// 按 id 取一个模型（上游 `getModel("deepseek", id)` 的替身）。
pub fn model(id: &str) -> Option<Model> {
    models().into_iter().find(|model| model.id == id)
}

/// pi: `deepseekProvider()`
pub fn deepseek_provider() -> DeepSeekProvider {
    DeepSeekProvider {
        id: "deepseek".to_string(),
        name: "DeepSeek".to_string(),
        base_url: DEEPSEEK_BASE_URL.to_string(),
        models: models(),
    }
}
