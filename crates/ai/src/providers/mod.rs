//! 对应上游 `packages/ai/src/providers/`。`model-schema.ts` 与 `compat-schema.ts` 的
//! OpenAI 部分是类型依赖；`deepseek.ts` 是本次接的第一家 provider。

pub mod compat_schema;
pub mod deepseek;
pub mod model_schema;
pub mod openai_codex;
