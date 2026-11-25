//! 对应上游 `packages/ai/src/providers/compat-schema.ts` 的 OpenAI Completions 部分。
//!
//! `Model.compat`（差异表 #13 留到 provider 步骤的字段）。上游是条件类型：`Model<"openai-completions">`
//! 才有 `compat?: OpenAICompletionsCompat`。Rust 的 `Model` 不分具体 api，字段挂在 `Model` 上、
//! 解成 `Option`，由读到它的 provider 代码负责含义。
//!
//! 本次接的是 DeepSeek 与「未知 OpenAI 兼容端点」的默认值；其余 provider 的字段照上游全量解析，
//! 但对应的分支没有接（见 `PORTING.md` 差异表 #110）。上游用嵌套 schema 表达的四个路由/kwargs
//! 字段在这里保持 `JsonValue`（差异表 #111）。

use serde::{Deserialize, Serialize};

use crate::types::JsonValue;

/// pi: `maxTokensField` = `"max_completion_tokens" | "max_tokens"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaxTokensField {
    #[serde(rename = "max_completion_tokens")]
    MaxCompletionTokens,
    #[serde(rename = "max_tokens")]
    MaxTokens,
}

/// pi: `thinkingFormat`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThinkingFormat {
    #[serde(rename = "openai")]
    Openai,
    #[serde(rename = "openrouter")]
    Openrouter,
    #[serde(rename = "deepseek")]
    Deepseek,
    #[serde(rename = "together")]
    Together,
    #[serde(rename = "baseten")]
    Baseten,
    #[serde(rename = "zai")]
    Zai,
    #[serde(rename = "qwen")]
    Qwen,
    #[serde(rename = "chat-template")]
    ChatTemplate,
    #[serde(rename = "qwen-chat-template")]
    QwenChatTemplate,
    #[serde(rename = "string-thinking")]
    StringThinking,
    #[serde(rename = "ant-ling")]
    AntLing,
}

/// pi: `cacheControlFormat` = `"anthropic"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CacheControlFormat {
    #[serde(rename = "anthropic")]
    Anthropic,
}

/// pi: `sessionAffinityFormat` = `"openai" | "openai-nosession" | "openrouter"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionAffinityFormat {
    #[serde(rename = "openai")]
    Openai,
    #[serde(rename = "openai-nosession")]
    OpenaiNosession,
    #[serde(rename = "openrouter")]
    Openrouter,
}

/// pi: `promptCacheKeyMode` = `"auto" | "enabled" | "disabled"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptCacheKeyMode {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "enabled")]
    Enabled,
    #[serde(rename = "disabled")]
    Disabled,
}

/// pi: `thinkingTokenBudgetField`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThinkingTokenBudgetField {
    #[serde(rename = "thinking_token_budget")]
    ThinkingTokenBudget,
    #[serde(rename = "thinking_budget")]
    ThinkingBudget,
    #[serde(rename = "thinking_budget_tokens")]
    ThinkingBudgetTokens,
}

impl ThinkingTokenBudgetField {
    /// 上游的字段名（写进请求体的键）。
    pub fn field_name(&self) -> &'static str {
        match self {
            Self::ThinkingTokenBudget => "thinking_token_budget",
            Self::ThinkingBudget => "thinking_budget",
            Self::ThinkingBudgetTokens => "thinking_budget_tokens",
        }
    }
}

/// pi: `OpenAICompletionsCompat`
///
/// 每个字段都是 `Option`：缺席表示「按 provider/URL 自动判断」（上游的 `?? detected`）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAICompletionsCompat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_store: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_developer_role: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_reasoning_effort: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_usage_in_streaming: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_finish_reason: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_field: Option<MaxTokensField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_tool_result_name: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_assistant_after_tool_result: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_non_null_assistant_content: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_tools_with_tool_history: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_thinking_as_text: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_reasoning_content_on_assistant_messages: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_format: Option<ThinkingFormat>,
    /// 上游是 `OpenRouterRoutingSchema`（结构化对象）；这里原样保留 JSON（差异表 #111）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_router_routing: Option<JsonValue>,
    /// 上游是 `VercelGatewayRoutingSchema`；这里原样保留 JSON（差异表 #111）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vercel_gateway_routing: Option<JsonValue>,
    /// 上游是 `Record<string, ChatTemplateKwargValue>`；这里原样保留 JSON（差异表 #111）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<JsonValue>,
    /// 上游是 `Record<string, ChatTemplateKwargValue>`；这里原样保留 JSON（差异表 #111）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template_args: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zai_tool_stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_thinking_token_budget: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_strict_mode: Option<bool>,
    // 上游的键是 `supportsOpenAIGrammarTools`（`AI` 两个字母都大写），camelCase 规则拼不出来。
    #[serde(
        rename = "supportsOpenAIGrammarTools",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_open_ai_grammar_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_system_messages: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_tool_additions: Option<bool>,
    // Responses 这一族才有的三项（上游是另一份 `OpenAIResponsesCompat`；这里并进同一份）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_additional_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tool_search: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_mid_convo_effort: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control_format: Option<CacheControlFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_session_affinity_headers: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_affinity_format: Option<SessionAffinityFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_long_cache_retention: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key_mode: Option<PromptCacheKeyMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vllm_priority: Option<f64>,
}
