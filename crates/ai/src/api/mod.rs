//! 对应上游 `packages/ai/src/api/`。本次搬 OpenAI Completions 这一条链与它依赖的变换。

pub mod constrained_sampling;
pub mod openai_codex_responses;
pub mod openai_completions;
pub mod openai_responses_shared;
pub mod simple_options;
pub mod transform_messages;
