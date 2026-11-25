//! 移植自上游 `packages/ai/test/identity.test.ts` 中落在本次范围内的部分。
//!
//! 里程碑一搬了核心循环用到的两样：`mergeClientIdentity` 与 `PI_ATTRIBUTION`，并把
//! `ClientIdentity.attribution` 的三态（缺席 / `null` / 一份归属）钉在 JSON 上。里程碑二在接
//! provider 时补上 `formatUserAgent`、`getAttributionHeaders`、`getIdentityHeaders`，这里跟着补
//! 它们的用例。`withClientIdentity` 与“身份在请求里的效果”属于应用层的 Models 封装，仍未搬。

use serde_json::{json, Value};
use xjpcode_ai::providers::model_schema::{ModelCost, ModelInputModality};
use xjpcode_ai::types::Model;
use xjpcode_ai::utils::identity::{
    format_user_agent, get_attribution_headers, get_identity_headers, merge_client_identity,
    ClientAttribution, ClientIdentity, ClientIdentityTarget, UserAgentMode, PI_AI_VERSION,
    PI_ATTRIBUTION,
};

fn target<'a>(provider: &'a str, base_url: &'a str) -> ClientIdentityTarget<'a> {
    ClientIdentityTarget { provider, base_url }
}

fn acme() -> ClientAttribution {
    ClientAttribution {
        name: "acme".to_string(),
        url: "https://acme.example".to_string(),
        categories: None,
    }
}

fn model(provider: &str, base_url: &str) -> Model {
    Model {
        id: "test-model".to_string(),
        name: "Test Model".to_string(),
        api: "openai-completions".to_string(),
        provider: provider.to_string(),
        base_url: base_url.to_string(),
        input: vec![ModelInputModality::Text],
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        r#type: None,
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 128_000,
        max_tokens: 4_096,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
        compat: None,
    }
}

fn pi() -> ClientAttribution {
    PI_ATTRIBUTION.clone()
}

fn json_of(identity: &ClientIdentity) -> Value {
    serde_json::to_value(identity).expect("an identity is JSON")
}

#[test]
fn keeps_pi_attribution_in_sync_with_pi() {
    assert_eq!(
        serde_json::to_value(pi()).expect("JSON"),
        json!({
            "name": "pi",
            "url": "https://pi.dev",
            "categories": ["cli-agent"]
        })
    );
}

#[test]
fn merges_identities_field_by_field() {
    assert_eq!(merge_client_identity(&[None, None]), None);

    let identity = ClientIdentity {
        user_agent: Some(UserAgentMode::Short),
        attribution: Some(Some(pi())),
        version: None,
    };
    // 第二个来源的 `attribution` 缺席（`None`）：跳过，不覆盖第一个来源的归属。
    let version = ClientIdentity {
        version: Some("1.0.0".to_string()),
        ..ClientIdentity::default()
    };
    assert_eq!(
        merge_client_identity(&[Some(&identity), Some(&version)]),
        Some(ClientIdentity {
            user_agent: Some(UserAgentMode::Short),
            attribution: Some(Some(pi())),
            version: Some("1.0.0".to_string()),
        })
    );

    // 显式的 `null` 也是一份“给了”的字段：它把前面来源的归属关掉。
    let off = ClientIdentity {
        attribution: Some(None),
        ..ClientIdentity::default()
    };
    assert_eq!(
        merge_client_identity(&[Some(&identity), Some(&off)]),
        Some(ClientIdentity {
            user_agent: Some(UserAgentMode::Short),
            attribution: Some(None),
            version: None,
        })
    );
}

#[test]
fn skips_absent_sources_but_keeps_an_empty_one() {
    // 上游 `if (!source) continue;`：`undefined` 跳过，`{}` 仍会返回一个新对象。
    let empty = ClientIdentity::default();
    assert_eq!(
        merge_client_identity(&[None, Some(&empty), None]),
        Some(ClientIdentity::default())
    );
    assert_eq!(json_of(&ClientIdentity::default()), json!({}));
}

#[test]
fn keeps_absent_and_null_attribution_apart() {
    // 上游 `attribution?: ClientAttribution | null`：缺席、`null`、一份归属是三种状态。
    let absent: ClientIdentity = serde_json::from_value(json!({})).expect("parse");
    assert_eq!(absent.attribution, None);

    let off: ClientIdentity = serde_json::from_value(json!({ "attribution": null })).expect("parse");
    assert_eq!(off.attribution, Some(None));

    let given: ClientIdentity =
        serde_json::from_value(json!({ "attribution": pi() })).expect("parse");
    assert_eq!(given.attribution, Some(Some(pi())));

    assert_eq!(json_of(&absent), json!({}));
    assert_eq!(json_of(&off), json!({ "attribution": null }));
    assert_eq!(json_of(&given), json!({ "attribution": pi() }));
}

#[test]
fn serializes_the_user_agent_modes_as_pi_does() {
    assert_eq!(
        json_of(&ClientIdentity {
            user_agent: Some(UserAgentMode::Short),
            ..ClientIdentity::default()
        }),
        json!({ "userAgent": "short" })
    );
    assert_eq!(
        json_of(&ClientIdentity {
            user_agent: Some(UserAgentMode::Custom {
                custom: "acme/1.0".to_string()
            }),
            ..ClientIdentity::default()
        }),
        json!({ "userAgent": { "custom": "acme/1.0" } })
    );
}

/// 上游：`keeps the version constant in sync with package.json`。基准提交的 `package.json` 是 1.6.0。
#[test]
fn keeps_the_version_constant_in_sync_with_the_benchmark() {
    assert_eq!(PI_AI_VERSION, "1.6.0");
}

/// 上游：`formats user agents`。运行时的写法不一样（差异 #116）：`rust`、`macos`/`aarch64`。
#[test]
fn formats_user_agents() {
    let runtime = format!(
        "pi/{PI_AI_VERSION} ({}; rust; {})",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    assert_eq!(format_user_agent(None), runtime);
    assert_eq!(
        format_user_agent(Some(&ClientIdentity {
            version: Some("9.9.9".to_string()),
            ..ClientIdentity::default()
        })),
        format!(
            "pi/9.9.9 ({}; rust; {})",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    );
    assert_eq!(
        format_user_agent(Some(&ClientIdentity {
            user_agent: Some(UserAgentMode::Short),
            version: Some("9.9.9".to_string()),
            ..ClientIdentity::default()
        })),
        "pi"
    );
    assert_eq!(
        format_user_agent(Some(&ClientIdentity {
            user_agent: Some(UserAgentMode::Custom {
                custom: "acme/1.0".to_string()
            }),
            ..ClientIdentity::default()
        })),
        "acme/1.0"
    );
}

/// 上游：`sends no attribution unless enabled`。
#[test]
fn sends_no_attribution_unless_enabled() {
    let openrouter = target("openrouter", "https://openrouter.ai/api/v1");
    assert!(get_attribution_headers(openrouter, None).is_empty());
    assert!(get_attribution_headers(
        openrouter,
        Some(&ClientIdentity {
            attribution: Some(None),
            ..ClientIdentity::default()
        })
    )
    .is_empty());
    let headers = get_identity_headers(&model("openrouter", "https://openrouter.ai/api/v1"), None);
    assert_eq!(headers.len(), 1);
    assert_eq!(headers.get("User-Agent"), Some(&format_user_agent(None)));
}

/// 上游：`attributes OpenRouter requests`。
#[test]
fn attributes_openrouter_requests() {
    let openrouter = target("openrouter", "https://openrouter.ai/api/v1");
    let headers = get_attribution_headers(
        openrouter,
        Some(&ClientIdentity {
            attribution: Some(Some(pi())),
            ..ClientIdentity::default()
        }),
    );
    assert_eq!(headers.get("HTTP-Referer"), Some(&"https://pi.dev".to_string()));
    assert_eq!(headers.get("X-OpenRouter-Title"), Some(&"pi".to_string()));
    assert_eq!(
        headers.get("X-OpenRouter-Categories"),
        Some(&"cli-agent".to_string())
    );

    let custom = get_attribution_headers(
        openrouter,
        Some(&ClientIdentity {
            attribution: Some(Some(ClientAttribution {
                categories: Some(vec!["cli-agent".to_string(), "cloud-agent".to_string()]),
                ..acme()
            })),
            ..ClientIdentity::default()
        }),
    );
    assert_eq!(
        custom.get("X-OpenRouter-Categories"),
        Some(&"cli-agent,cloud-agent".to_string())
    );

    let no_categories = get_attribution_headers(
        openrouter,
        Some(&ClientIdentity {
            attribution: Some(Some(ClientAttribution {
                categories: Some(Vec::new()),
                ..acme()
            })),
            ..ClientIdentity::default()
        }),
    );
    assert_eq!(no_categories.get("X-OpenRouter-Categories"), None);

    // 自定义 provider 只要 baseUrl 里带 openrouter.ai 也算。
    let routed = get_attribution_headers(
        target("custom", "not-a-url-openrouter.ai"),
        Some(&ClientIdentity {
            attribution: Some(Some(pi())),
            ..ClientIdentity::default()
        }),
    );
    assert_eq!(routed.get("X-OpenRouter-Title"), Some(&"pi".to_string()));
}

/// 上游：`attributes NVIDIA NIM requests with a capitalized name and OpenCode requests with the name`。
#[test]
fn attributes_nvidia_and_opencode_requests() {
    let nvidia = get_attribution_headers(
        target("nvidia", "https://example.test/v1"),
        Some(&ClientIdentity {
            attribution: Some(Some(pi())),
            ..ClientIdentity::default()
        }),
    );
    assert_eq!(
        nvidia.get("X-BILLING-INVOKE-ORIGIN"),
        Some(&"Pi".to_string())
    );

    let custom_nim = get_attribution_headers(
        target("custom-nim", "https://integrate.api.nvidia.com/v1"),
        Some(&ClientIdentity {
            attribution: Some(Some(acme())),
            ..ClientIdentity::default()
        }),
    );
    assert_eq!(
        custom_nim.get("X-BILLING-INVOKE-ORIGIN"),
        Some(&"Acme".to_string())
    );

    let opencode = get_attribution_headers(
        target("opencode", "https://opencode.ai/zen/v1"),
        Some(&ClientIdentity {
            attribution: Some(Some(pi())),
            ..ClientIdentity::default()
        }),
    );
    assert_eq!(opencode.get("x-opencode-client"), Some(&"pi".to_string()));

    let custom_opencode = get_attribution_headers(
        target("custom", "https://opencode.ai/zen/v1"),
        Some(&ClientIdentity {
            attribution: Some(Some(acme())),
            ..ClientIdentity::default()
        }),
    );
    assert_eq!(
        custom_opencode.get("x-opencode-client"),
        Some(&"acme".to_string())
    );
}

/// 上游：`does not attribute NVIDIA models routed through gateways to NVIDIA`。
#[test]
fn does_not_attribute_nvidia_models_routed_through_gateways() {
    let via_openrouter = get_attribution_headers(
        target("openrouter", "https://openrouter.ai/api/v1"),
        Some(&ClientIdentity {
            attribution: Some(Some(pi())),
            ..ClientIdentity::default()
        }),
    );
    assert!(!via_openrouter.contains_key("X-BILLING-INVOKE-ORIGIN"));

    let via_vercel = get_attribution_headers(
        target("vercel-ai-gateway", "https://ai-gateway.vercel.sh/v1"),
        Some(&ClientIdentity {
            attribution: Some(Some(pi())),
            ..ClientIdentity::default()
        }),
    );
    assert!(via_vercel.is_empty());
}
