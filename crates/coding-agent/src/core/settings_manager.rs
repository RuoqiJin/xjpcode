//! 对应上游 `src/core/settings-manager.ts` 的这一里程碑用到的部分：重试、压缩，
//! 以及新开一场要用的缺省模型与缺省思考档。
//!
//! 上游读两份：`~/.pi/agent/settings.json` 与项目里的 `.pi/settings.json`，后者的字段盖过前者。
//! xjpcode 的项目配置目录沿用 `.xjpcode`（里程碑三差异 #14），用户目录照旧读 pi 的那份，这样
//! owner 已经调过的重试与压缩设置、`defaultThinkingLevel` 直接生效。这里只读不写：pi 换模型、
//! 换思考档时会往那份文件里写缺省值，xjpcode 不跟着写（见 `PORTING.md` 差异表）。

use std::path::Path;

use serde::Deserialize;
use xjpcode_ai::utils::retry::{RetryPolicy, DEFAULT_MAX_AGENT_RETRY_DELAY_MS};

use crate::core::compaction::{CompactionSettings, DEFAULT_COMPACTION_SETTINGS};

/// pi: `SETTINGS_DEFAULTS.retry`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrySettings {
    pub enabled: bool,
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_agent_delay_ms: Option<u64>,
}

impl Default for RetrySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 2000,
            max_agent_delay_ms: Some(DEFAULT_MAX_AGENT_RETRY_DELAY_MS),
        }
    }
}

impl RetrySettings {
    pub fn policy(&self) -> RetryPolicy {
        RetryPolicy {
            enabled: self.enabled,
            max_retries: self.max_retries,
            base_delay_ms: self.base_delay_ms,
            max_agent_delay_ms: self.max_agent_delay_ms,
        }
    }
}

/// 这一层读写的那几项设置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub retry: RetrySettings,
    pub compaction: CompactionSettings,
    /// `defaultProvider` / `defaultModel`（pi 的缺省模型设置）。
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    /// `defaultThinkingLevel`——新开一场的思考档；缺省见 `DEFAULT_THINKING_LEVEL`。
    pub default_thinking_level: Option<String>,
    /// `modelThinkingLevels["provider/id"]`——某个模型单独记住的思考档。
    pub model_thinking_levels: std::collections::BTreeMap<String, String>,
}

/// pi: `DEFAULT_THINKING_LEVEL`（`core/defaults.ts`）
pub const DEFAULT_THINKING_LEVEL: &str = "medium";

impl Default for Settings {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl Settings {
    /// 内存里的一份（测试与缺省）。
    pub fn in_memory() -> Self {
        Self {
            retry: RetrySettings::default(),
            compaction: DEFAULT_COMPACTION_SETTINGS,
            default_provider: None,
            default_model: None,
            default_thinking_level: None,
            model_thinking_levels: std::collections::BTreeMap::new(),
        }
    }

    /// pi: `getModelThinkingLevel(provider, modelId)`
    pub fn model_thinking_level(&self, provider: &str, model_id: &str) -> Option<&str> {
        self.model_thinking_levels
            .get(&format!("{provider}/{model_id}"))
            .map(String::as_str)
    }

    pub fn retry_policy(&self) -> RetryPolicy {
        self.retry.policy()
    }

    /// pi: `getCompactionSettings(model)`——模型单独的覆盖没搬。
    pub fn compaction_settings(&self) -> CompactionSettings {
        self.compaction.clone()
    }

    /// 读用户目录与项目目录的两份设置，后者的字段盖过前者。文件不存在或坏掉就用默认值。
    pub fn load(agent_dir: &Path, cwd: &Path) -> Self {
        let mut settings = Self::in_memory();
        for path in [
            agent_dir.join("settings.json"),
            cwd.join(crate::config::CONFIG_DIR_NAME).join("settings.json"),
        ] {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(partial) = serde_json::from_str::<PartialSettings>(&content) {
                    settings.apply(partial);
                }
            }
        }
        settings
    }

    fn apply(&mut self, partial: PartialSettings) {
        if let Some(retry) = partial.retry {
            if let Some(enabled) = retry.enabled {
                self.retry.enabled = enabled;
            }
            if let Some(max_retries) = retry.max_retries {
                self.retry.max_retries = max_retries;
            }
            if let Some(base_delay_ms) = retry.base_delay_ms {
                self.retry.base_delay_ms = base_delay_ms;
            }
            if let Some(max_agent_delay_ms) = retry.max_agent_delay_ms {
                self.retry.max_agent_delay_ms = Some(max_agent_delay_ms);
            }
        }
        if let Some(provider) = partial.default_provider {
            self.default_provider = Some(provider);
        }
        if let Some(model) = partial.default_model {
            self.default_model = Some(model);
        }
        if let Some(level) = partial.default_thinking_level {
            self.default_thinking_level = Some(level);
        }
        if let Some(levels) = partial.model_thinking_levels {
            self.model_thinking_levels = levels;
        }
        if let Some(compaction) = partial.compaction {
            if let Some(enabled) = compaction.enabled {
                self.compaction.enabled = enabled;
            }
            if let Some(reserve_tokens) = compaction.reserve_tokens {
                self.compaction.reserve_tokens = reserve_tokens;
            }
            if let Some(keep_recent_tokens) = compaction.keep_recent_tokens {
                self.compaction.keep_recent_tokens = keep_recent_tokens;
            }
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct PartialSettings {
    retry: Option<PartialRetry>,
    compaction: Option<PartialCompaction>,
    default_provider: Option<String>,
    default_model: Option<String>,
    default_thinking_level: Option<String>,
    model_thinking_levels: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct PartialRetry {
    enabled: Option<bool>,
    max_retries: Option<u32>,
    base_delay_ms: Option<u64>,
    max_agent_delay_ms: Option<u64>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct PartialCompaction {
    enabled: Option<bool>,
    reserve_tokens: Option<u64>,
    keep_recent_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn defaults_match_pi() {
        let settings = Settings::in_memory();
        assert!(settings.retry.enabled);
        assert_eq!(settings.retry.max_retries, 3);
        assert_eq!(settings.retry.base_delay_ms, 2000);
        assert_eq!(settings.retry.max_agent_delay_ms, Some(60_000));
        assert!(settings.compaction.enabled);
        assert_eq!(settings.compaction.reserve_tokens, 16_384);
        assert_eq!(settings.compaction.keep_recent_tokens, 20_000);
    }

    #[test]
    fn the_project_settings_override_the_user_settings() {
        let agent = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        std::fs::write(
            agent.path().join("settings.json"),
            r#"{"retry":{"maxRetries":5,"baseDelayMs":10},"compaction":{"keepRecentTokens":1}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(cwd.path().join(".xjpcode")).unwrap();
        std::fs::write(
            cwd.path().join(".xjpcode/settings.json"),
            r#"{"retry":{"maxRetries":2,"enabled":false}}"#,
        )
        .unwrap();
        let settings = Settings::load(agent.path(), cwd.path());
        assert_eq!(settings.retry.max_retries, 2);
        assert!(!settings.retry.enabled);
        assert_eq!(settings.retry.base_delay_ms, 10);
        assert_eq!(settings.compaction.keep_recent_tokens, 1);
    }

    #[test]
    fn broken_settings_fall_back_to_defaults() {
        let agent = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        std::fs::write(agent.path().join("settings.json"), "{not json").unwrap();
        assert_eq!(Settings::load(agent.path(), cwd.path()), Settings::in_memory());
    }
}
