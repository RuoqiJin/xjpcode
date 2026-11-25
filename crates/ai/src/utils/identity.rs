//! 对应上游 `packages/ai/src/utils/identity.ts` 中被核心循环用到的部分。
//!
//! 已搬：`UserAgentMode`、`ClientAttribution`、`ClientIdentity`、`PI_ATTRIBUTION`、
//! `mergeClientIdentity`（里程碑一），以及 provider 适配层要的 `formatUserAgent`、
//! `getAttributionHeaders`、`getIdentityHeaders`（这一步）。

use std::sync::LazyLock;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// pi: `UserAgentMode` = `"short" | "detailed" | { custom: string }`
///
/// 上游是"字符串字面量或对象"的联合，serde 的 untagged 无法用一个类型表达
/// （单元变体在 untagged 下会序列化成 `null`），因此手写实现。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserAgentMode {
    Short,
    Detailed,
    Custom { custom: String },
}

impl Serialize for UserAgentMode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Short => serializer.serialize_str("short"),
            Self::Detailed => serializer.serialize_str("detailed"),
            Self::Custom { custom } => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("custom", custom)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for UserAgentMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Name(String),
            Object { custom: String },
        }
        match Raw::deserialize(deserializer)? {
            Raw::Name(name) => match name.as_str() {
                "short" => Ok(Self::Short),
                "detailed" => Ok(Self::Detailed),
                other => Err(D::Error::custom(format!(
                    "unknown user agent mode {other:?}"
                ))),
            },
            Raw::Object { custom } => Ok(Self::Custom { custom }),
        }
    }
}

/// pi: `ClientAttribution`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientAttribution {
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<String>>,
}

/// pi: `PI_ATTRIBUTION`—pi-agent-core、pi-durable 与 coding agent 共用的归属。
///
/// 上游是一个对象字面量常量；Rust 侧 `String`/`Vec` 不能出现在 `const` 里，所以用 `LazyLock`
/// 保住同一个名字。取值与上游逐字相同（由 `identity.rs` 测试守住）。
pub static PI_ATTRIBUTION: LazyLock<ClientAttribution> = LazyLock::new(|| ClientAttribution {
    name: "pi".to_string(),
    url: "https://pi.dev".to_string(),
    categories: Some(vec!["cli-agent".to_string()]),
});

/// pi: `ClientIdentity`
///
/// 上游的 `attribution?: ClientAttribution | null`：缺席（`undefined`）表示“没给”，`null` 表示
/// “关掉合并来的归属”。Rust 侧用 `Option<Option<_>>` 保住三态：外层 `None` 是缺席，`Some(None)`
/// 是显式 `null`，`Some(Some(_))` 是给了一份（见 `PORTING.md` 差异表 #29）。`deserialize_some`
/// 让显式 `null` 落成 `Some(None)`，而不是与缺席一样变成 `None`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<UserAgentMode>,
    #[serde(
        default,
        deserialize_with = "deserialize_some",
        skip_serializing_if = "Option::is_none"
    )]
    pub attribution: Option<Option<ClientAttribution>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// 把一个可缺席的字段读成 `Option<Option<T>>`：`null` 是 `Some(None)`，缺席由 `default` 给 `None`。
fn deserialize_some<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// pi: `mergeClientIdentity`—逐个字段合并，后面的来源覆盖前面的，缺席的字段跳过。
///
/// 上游是可变参数 `(...sources: (ClientIdentity | undefined)[])`；Rust 用切片，每个来源可以缺席。
/// 显式 `null` 的 `attribution`（`Some(None)`）也算“给了”：它会把前面来源的归属关掉。
pub fn merge_client_identity(sources: &[Option<&ClientIdentity>]) -> Option<ClientIdentity> {
    let mut merged: Option<ClientIdentity> = None;
    for source in sources {
        let Some(source) = source else {
            continue;
        };
        let merged = merged.get_or_insert_with(ClientIdentity::default);
        if source.user_agent.is_some() {
            merged.user_agent = source.user_agent.clone();
        }
        if source.attribution.is_some() {
            merged.attribution = source.attribution.clone();
        }
        if source.version.is_some() {
            merged.version = source.version.clone();
        }
    }
    merged
}

/// 上游 `version.ts` 里的 `PI_AI_VERSION`（基准提交的 `packages/ai/package.json`）。
pub const PI_AI_VERSION: &str = "1.6.0";

/// pi: `ClientIdentityTarget`——身份头要知道请求发往谁。
#[derive(Debug, Clone, Copy)]
pub struct ClientIdentityTarget<'a> {
    pub provider: &'a str,
    pub base_url: &'a str,
}

/// 上游的 `runtimeDescription()` 读的是 `process.versions`（bun/node）；Rust 侧没有运行时版本，
/// 写成 `rust`（见 `PORTING.md` 差异表 #116）。
fn runtime_description() -> Option<&'static str> {
    Some("rust")
}

/// pi: `getClientVersion`
pub fn get_client_version(identity: Option<&ClientIdentity>) -> String {
    identity
        .and_then(|identity| identity.version.clone())
        .unwrap_or_else(|| PI_AI_VERSION.to_string())
}

/// pi: `formatUserAgent`
pub fn format_user_agent(identity: Option<&ClientIdentity>) -> String {
    let mode = identity.and_then(|identity| identity.user_agent.as_ref());
    match mode {
        Some(UserAgentMode::Custom { custom }) => custom.clone(),
        Some(UserAgentMode::Short) => "pi".to_string(),
        _ => {
            let product = format!("pi/{}", get_client_version(identity));
            match runtime_description() {
                Some(runtime) => format!(
                    "{product} ({}; {runtime}; {})",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ),
                None => format!("{product} (browser)"),
            }
        }
    }
}

const NVIDIA_NIM_HOST: &str = "integrate.api.nvidia.com";
const OPENCODE_HOST: &str = "opencode.ai";

fn url_host(base_url: &str) -> Option<String> {
    url::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
}

fn matches_host(base_url: &str, expected_host: &str) -> bool {
    url_host(base_url).as_deref() == Some(expected_host)
}

fn is_openrouter(target: ClientIdentityTarget<'_>) -> bool {
    target.provider == "openrouter" || target.base_url.contains("openrouter.ai")
}

fn is_nvidia_nim(target: ClientIdentityTarget<'_>) -> bool {
    target.provider == "nvidia" || matches_host(target.base_url, NVIDIA_NIM_HOST)
}

fn is_opencode(target: ClientIdentityTarget<'_>) -> bool {
    target.provider == "opencode"
        || target.provider == "opencode-go"
        || matches_host(target.base_url, OPENCODE_HOST)
}

/// pi: `getAttributionHeaders`——归属关掉时是空的。
pub fn get_attribution_headers(
    target: ClientIdentityTarget<'_>,
    identity: Option<&ClientIdentity>,
) -> std::collections::BTreeMap<String, String> {
    let mut headers = std::collections::BTreeMap::new();
    let Some(Some(attribution)) = identity.and_then(|identity| identity.attribution.clone()) else {
        return headers;
    };
    if is_openrouter(target) {
        headers.insert("HTTP-Referer".to_string(), attribution.url);
        headers.insert("X-OpenRouter-Title".to_string(), attribution.name);
        if let Some(categories) = attribution.categories.filter(|categories| !categories.is_empty()) {
            headers.insert("X-OpenRouter-Categories".to_string(), categories.join(","));
        }
    } else if is_nvidia_nim(target) {
        let mut chars = attribution.name.chars();
        let capitalized = match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        };
        headers.insert("X-BILLING-INVOKE-ORIGIN".to_string(), capitalized);
    } else if is_opencode(target) {
        headers.insert("x-opencode-client".to_string(), attribution.name);
    }
    headers
}

/// pi: `getIdentityHeaders`——默认的 `User-Agent` 与归属头；后加的头可以覆盖它们。
pub fn get_identity_headers(
    model: &crate::types::Model,
    identity: Option<&ClientIdentity>,
) -> std::collections::BTreeMap<String, String> {
    let mut headers = std::collections::BTreeMap::new();
    headers.insert("User-Agent".to_string(), format_user_agent(identity));
    headers.extend(get_attribution_headers(
        ClientIdentityTarget {
            provider: &model.provider,
            base_url: &model.base_url,
        },
        identity,
    ));
    headers
}
