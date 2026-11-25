//! 对应上游 `packages/ai/src/api/simple-options.ts`。
//!
//! `streamSimple` 把「简单选项」铺成完整的请求选项：按模型支持的思考等级挑采样参数、给输入留出
//! 余量后收紧 `maxTokens`、算思考预算。DeepSeek 的 `streamSimple` 走这条路。

use crate::models::clamp_thinking_level;
use crate::providers::model_schema::{ModelThinkingLevel, ThinkingLevel};
use crate::types::{
    Model, SamplingParams, SimpleStreamOptions, StreamOptions, ThinkingBudgets, TranscriptContext,
};
use crate::utils::estimate::estimate_context_tokens;

/// 给输入比估算值更大留的固定余量。
const CONTEXT_SAFETY_TOKENS: u64 = 4096;
/// OpenRouter 用自己的分词器数输入，实测比 provider 的计数最多高 12%；这里留 15%。
const CONTEXT_SAFETY_RATIO: f64 = 0.15;
/// 比例余量只在还能留出这么多输出时使用。
const MIN_OUTPUT_WITH_RATIO_MARGIN: u64 = 32768;
const MIN_MAX_TOKENS: u64 = 1;

/// pi: `clampMaxTokensToContext`——把 `maxTokens` 收进上下文窗口，给算不准的输入留余量。
pub fn clamp_max_tokens_to_context(
    model: &Model,
    context: &TranscriptContext,
    max_tokens: u64,
) -> u64 {
    if model.context_window == 0 {
        return max_tokens.max(MIN_MAX_TOKENS);
    }
    let input = estimate_context_tokens(context).tokens;
    let margin = CONTEXT_SAFETY_TOKENS.max((input as f64 * CONTEXT_SAFETY_RATIO).ceil() as u64);
    let room = model.context_window.saturating_sub(input).saturating_sub(margin);
    let room = if room < MIN_OUTPUT_WITH_RATIO_MARGIN {
        MIN_OUTPUT_WITH_RATIO_MARGIN.min(
            model
                .context_window
                .saturating_sub(input)
                .saturating_sub(CONTEXT_SAFETY_TOKENS),
        )
    } else {
        room
    };
    max_tokens.min(room.max(MIN_MAX_TOKENS))
}

/// pi: `resolveSamplingParams`——模型级、思考等级级、请求级依次覆盖。
pub fn resolve_sampling_params(
    model: &Model,
    thinking_level: ModelThinkingLevel,
    request_params: Option<&SamplingParams>,
) -> Option<SamplingParams> {
    let effective_thinking_level = clamp_thinking_level(model, thinking_level);
    let thinking_level_params = model
        .sampling_params_by_thinking_level
        .as_ref()
        .and_then(|by_level| by_level.get(&effective_thinking_level));
    if model.sampling_params.is_none() && thinking_level_params.is_none() && request_params.is_none() {
        return None;
    }
    let mut params = SamplingParams::new();
    if let Some(model_params) = &model.sampling_params {
        params.extend(model_params.iter().map(|(key, value)| (key.clone(), value.clone())));
    }
    if let Some(level_params) = thinking_level_params {
        params.extend(level_params.iter().map(|(key, value)| (key.clone(), value.clone())));
    }
    if let Some(request_params) = request_params {
        params.extend(request_params.iter().map(|(key, value)| (key.clone(), value.clone())));
    }
    Some(params)
}

/// pi: `buildBaseOptions`
pub fn build_base_options(
    model: &Model,
    context: &TranscriptContext,
    options: Option<&SimpleStreamOptions>,
    api_key: Option<&str>,
) -> StreamOptions {
    // 上游的 `options?.reasoning ?? "off"`：`ThinkingLevel` 不含 `off`，所以缺席时直接取
    // `ModelThinkingLevel::Off`；`resolveSamplingParams` 内部会把等级夹到模型支持的一档。
    let sampling_params = resolve_sampling_params_for_simple(model, options);

    StreamOptions {
        api_key: api_key
            .map(str::to_string)
            .or_else(|| options.and_then(|options| options.api_key.clone())),
        env: options.and_then(|options| options.env.clone()),
        on_payload: options.map(|options| options.on_payload.clone()).unwrap_or_default(),
        on_response: options.map(|options| options.on_response.clone()).unwrap_or_default(),
        headers: options.and_then(|options| options.headers.clone()),
        identity: options.and_then(|options| options.identity.clone()),
        timeout_ms: options.and_then(|options| options.timeout_ms),
        max_retries: options.and_then(|options| options.max_retries),
        max_retry_delay_ms: options.and_then(|options| options.max_retry_delay_ms),
        on_provider_stream_event: options
            .map(|options| options.on_provider_stream_event.clone())
            .unwrap_or_default(),
        temperature: options.and_then(|options| options.temperature),
        sampling_params,
        max_tokens: Some(clamp_max_tokens_to_context(
            model,
            context,
            options
                .and_then(|options| options.max_tokens)
                .unwrap_or(model.max_tokens),
        )),
        transport: options.and_then(|options| options.transport),
        cache_retention: options.and_then(|options| options.cache_retention),
        session_id: options.and_then(|options| options.session_id.clone()),
        disable_continuation: options.and_then(|options| options.disable_continuation),
        websocket_connect_timeout_ms: options
            .and_then(|options| options.websocket_connect_timeout_ms),
        metadata: options.and_then(|options| options.metadata.clone()),
    }
}

/// 上游 `resolveSamplingParams(model, options?.reasoning ?? "off", …)`：`off` 由模型的
/// `thinkingLevelMap` 给出。Rust 的 `ModelThinkingLevel` 才有 `Off`。
fn resolve_sampling_params_for_simple(
    model: &Model,
    options: Option<&SimpleStreamOptions>,
) -> Option<SamplingParams> {
    let level = match options.and_then(|options| options.reasoning) {
        Some(level) => thinking_level_to_model_level(level),
        None => ModelThinkingLevel::Off,
    };
    resolve_sampling_params(model, level, options.and_then(|options| options.sampling_params.as_ref()))
}

/// `ThinkingLevel`（没有 `off`）换到 `ModelThinkingLevel`。
pub fn thinking_level_to_model_level(level: ThinkingLevel) -> ModelThinkingLevel {
    match level {
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
        ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

/// `ModelThinkingLevel` 换回 `ThinkingLevel`；`Off` 没有对应值，返回 `None`。
pub fn model_level_to_thinking_level(level: ModelThinkingLevel) -> Option<ThinkingLevel> {
    match level {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(ThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(ThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(ThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(ThinkingLevel::High),
        ModelThinkingLevel::Xhigh => Some(ThinkingLevel::Xhigh),
        ModelThinkingLevel::Max => Some(ThinkingLevel::Max),
    }
}

/// 上游 `resolveSamplingParams(model, options?.reasoningEffort ?? "off", …)`。
pub fn resolve_sampling_params_for_request(
    model: &Model,
    reasoning_effort: Option<ThinkingLevel>,
    request_params: Option<&SamplingParams>,
) -> Option<SamplingParams> {
    let level = match reasoning_effort {
        Some(effort) => thinking_level_to_model_level(effort),
        None => ModelThinkingLevel::Off,
    };
    resolve_sampling_params(model, level, request_params)
}

/// 回答至少留多少 token（与思考预算共用响应上限时）。
pub const MIN_ANSWER_TOKENS: u64 = 1024;

/// pi: `DEFAULT_THINKING_BUDGETS`
pub fn default_thinking_budgets() -> ThinkingBudgets {
    ThinkingBudgets {
        minimal: Some(1024),
        low: Some(2048),
        medium: Some(8192),
        high: Some(16384),
    }
}

/// pi: `clampReasoning`——`xhigh`/`max` 落到 `high`。
pub fn clamp_reasoning(effort: ThinkingLevel) -> ThinkingLevel {
    match effort {
        ThinkingLevel::Xhigh | ThinkingLevel::Max => ThinkingLevel::High,
        other => other,
    }
}

/// pi: `thinkingBudgetForLevel`
pub fn thinking_budget_for_level(level: ThinkingLevel, custom_budgets: Option<&ThinkingBudgets>) -> u64 {
    let budgets = default_thinking_budgets();
    let level = clamp_reasoning(level);
    let budget = match level {
        ThinkingLevel::Minimal => custom_budgets.and_then(|b| b.minimal).or(budgets.minimal),
        ThinkingLevel::Low => custom_budgets.and_then(|b| b.low).or(budgets.low),
        ThinkingLevel::Medium => custom_budgets.and_then(|b| b.medium).or(budgets.medium),
        ThinkingLevel::High => custom_budgets.and_then(|b| b.high).or(budgets.high),
        ThinkingLevel::Xhigh | ThinkingLevel::Max => custom_budgets.and_then(|b| b.high).or(budgets.high),
    };
    budget.unwrap_or(0)
}

/// pi: `clampThinkingBudgetToAnswerRoom`
pub fn clamp_thinking_budget_to_answer_room(thinking_budget: u64, ceiling: u64) -> u64 {
    thinking_budget.min(ceiling.saturating_sub(MIN_ANSWER_TOKENS))
}

/// pi: `adjustMaxTokensForThinking`
pub fn adjust_max_tokens_for_thinking(
    base_max_tokens: Option<u64>,
    model_max_tokens: u64,
    reasoning_level: ThinkingLevel,
    custom_budgets: Option<&ThinkingBudgets>,
) -> (u64, u64) {
    let mut thinking_budget = thinking_budget_for_level(reasoning_level, custom_budgets);
    let max_tokens = match base_max_tokens {
        None => model_max_tokens,
        Some(base) => (base + thinking_budget).min(model_max_tokens),
    };
    if max_tokens <= thinking_budget {
        thinking_budget = clamp_thinking_budget_to_answer_room(thinking_budget, max_tokens);
    }
    (max_tokens, thinking_budget)
}
