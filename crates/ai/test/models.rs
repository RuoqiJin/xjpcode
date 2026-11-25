//! `crates/ai/src/models.rs` 的测试。
//!
//! 上游出处：`test/models-runtime.test.ts` 的 `calculateCost` 两条，以及
//! `test/max-thinking.test.ts` 的 `clampThinkingLevel`。模型目录、刷新与鉴权的用例不在本次范围。

use std::collections::BTreeMap;

use xjpcode_ai::models::{calculate_cost, clamp_thinking_level, get_supported_thinking_levels};
use xjpcode_ai::providers::model_schema::{
    ModelCost, ModelCostTier, ModelThinkingLevel, ThinkingLevelMap,
};
use xjpcode_ai::types::{Model, Usage, UsageCost};

fn test_model(id: &str, provider: &str) -> Model {
    Model {
        id: id.to_string(),
        name: id.to_string(),
        api: "test-api".to_string(),
        provider: provider.to_string(),
        base_url: "https://example.test/v1".to_string(),
        input: vec![xjpcode_ai::providers::model_schema::ModelInputModality::Text],
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        r#type: None,
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 10_000,
        max_tokens: 1_000,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

fn usage(input: u64, output: u64, cache_read: u64, cache_write: u64, cache_write_1h: u64) -> Usage {
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        cache_write_1h: Some(cache_write_1h),
        reasoning: None,
        total_tokens: input + output + cache_read + cache_write,
        cost: UsageCost::default(),
    }
}

#[test]
fn applies_request_wide_pricing_tiers_above_the_configured_input_threshold() {
    let mut model = test_model("openai", "gpt-5.6-sol");
    model.cost = ModelCost {
        input: 5.0,
        output: 30.0,
        cache_read: 0.5,
        cache_write: 6.25,
        tiers: Some(vec![ModelCostTier {
            input_tokens_above: 272_000.0,
            input: 10.0,
            output: 45.0,
            cache_read: 1.0,
            cache_write: 12.5,
        }]),
    };

    // 短上下文：用基础价目。input + cacheRead + cacheWrite = 272000，不比门槛高。
    let mut short = usage(200_000, 100_000, 72_000, 0, 0);
    calculate_cost(&model, &mut short);
    assert_eq!(short.cost.input, 1.0);
    assert_eq!(short.cost.output, 3.0);
    assert_eq!(short.cost.cache_read, 0.036);
    assert_eq!(short.cost.cache_write, 0.0);

    // 多写 1 个 token 就跨过门槛，整单按阶梯价。
    let mut long = usage(200_000, 100_000, 72_000, 1, 0);
    calculate_cost(&model, &mut long);
    assert_eq!(long.cost.input, 2.0);
    assert_eq!(long.cost.output, 4.5);
    assert_eq!(long.cost.cache_read, 0.072);
    assert!((long.cost.cache_write - 0.0000125).abs() < 1e-12);
}

#[test]
fn marks_catalog_derived_totals_as_not_provider_reported() {
    let model = test_model("openai", "gpt-5.6-sol");
    let mut usage = usage(1_000_000, 0, 0, 0, 0);
    usage.cost.total = 9.0;
    usage.cost.total_from_provider = Some(true);
    calculate_cost(&model, &mut usage);
    assert_eq!(usage.cost.total_from_provider, Some(false));
}

#[test]
fn a_one_hour_cache_write_costs_twice_the_input_price() {
    let mut model = test_model("anthropic", "claude");
    model.cost = ModelCost {
        input: 3.0,
        output: 15.0,
        cache_read: 0.3,
        cache_write: 3.75,
        tiers: None,
    };
    let mut usage = usage(0, 0, 0, 10, 10);
    calculate_cost(&model, &mut usage);
    // 1h 写入那份按两倍输入价：10 * 3 * 2 / 1e6。
    assert!((usage.cost.cache_write - 0.00006).abs() < 1e-12);
}

#[test]
fn supports_only_off_for_non_reasoning_models() {
    let model = test_model("openai", "plain");
    assert_eq!(
        get_supported_thinking_levels(&model),
        vec![ModelThinkingLevel::Off]
    );
}

/// 上游：`is opt-in for ordinary reasoning models`。
#[test]
fn max_and_xhigh_are_opt_in_for_ordinary_reasoning_models() {
    let mut model = test_model("test", "ordinary-reasoning");
    model.reasoning = true;
    assert_eq!(
        get_supported_thinking_levels(&model),
        vec![
            ModelThinkingLevel::Off,
            ModelThinkingLevel::Minimal,
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
        ]
    );
    assert_eq!(
        clamp_thinking_level(&model, ModelThinkingLevel::Max),
        ModelThinkingLevel::High
    );
}

#[test]
fn clamps_to_the_next_supported_level_then_falls_back() {
    // 上游 `max-thinking.test.ts`：只有 high/max 被映射时，xhigh 往上找到 max，
    // minimal 往下找到 high。
    let mut model = test_model("deepseek", "deepseek-v4-pro");
    model.reasoning = true;
    let mut map = ThinkingLevelMap::new();
    map.insert(ModelThinkingLevel::Off, None);
    map.insert(ModelThinkingLevel::Low, None);
    map.insert(ModelThinkingLevel::Medium, None);
    map.insert(ModelThinkingLevel::Minimal, None);
    map.insert(ModelThinkingLevel::High, Some("high".to_string()));
    map.insert(ModelThinkingLevel::Max, Some("max".to_string()));
    model.thinking_level_map = Some(map);
    assert_eq!(
        clamp_thinking_level(&model, ModelThinkingLevel::Xhigh),
        ModelThinkingLevel::Max
    );
    assert_eq!(
        clamp_thinking_level(&model, ModelThinkingLevel::Minimal),
        ModelThinkingLevel::High
    );
    assert_eq!(
        get_supported_thinking_levels(&model),
        vec![ModelThinkingLevel::High, ModelThinkingLevel::Max]
    );
}

#[test]
fn xhigh_and_max_need_an_explicit_mapping() {
    let mut model = test_model("openai", "gpt");
    model.reasoning = true;
    model.thinking_level_map = Some(BTreeMap::from([(
        ModelThinkingLevel::Off,
        Some("none".to_string()),
    )]));
    let levels = get_supported_thinking_levels(&model);
    assert!(levels.contains(&ModelThinkingLevel::High));
    assert!(!levels.contains(&ModelThinkingLevel::Xhigh));
    assert!(!levels.contains(&ModelThinkingLevel::Max));
}
