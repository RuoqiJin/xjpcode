//! 对应上游 `packages/coding-agent/src/core/resolve-config-value.ts`。
//!
//! `auth.json` 里的一把钥匙可以是：字面值、带 `$ENV` 的模板（`$$` 与 `$!` 是转义），
//! 或者 `!命令`（跑一次 shell，取 stdout）。命令结果按配置串缓存到进程结束。

use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
enum TemplatePart {
    Literal(String),
    Env(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfigValueReference {
    Command(String),
    Template(Vec<TemplatePart>),
}

static COMMAND_RESULT_CACHE: LazyLock<Mutex<BTreeMap<String, Option<String>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn append_literal(parts: &mut Vec<TemplatePart>, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Some(TemplatePart::Literal(previous)) = parts.last_mut() {
        previous.push_str(value);
        return;
    }
    parts.push(TemplatePart::Literal(value.to_string()));
}

fn env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn env_var_name_prefix(text: &str) -> Option<String> {
    let mut chars = text.chars().peekable();
    let first = *chars.peek()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let mut name = String::new();
    while let Some(ch) = chars.peek() {
        if ch.is_ascii_alphanumeric() || *ch == '_' {
            name.push(*ch);
            chars.next();
        } else {
            break;
        }
    }
    Some(name)
}

fn parse_template(config: &str) -> Vec<TemplatePart> {
    let mut parts = Vec::new();
    let characters: Vec<char> = config.chars().collect();
    let mut index = 0usize;
    while index < characters.len() {
        let Some(offset) = characters[index..].iter().position(|ch| *ch == '$') else {
            let rest: String = characters[index..].iter().collect();
            append_literal(&mut parts, &rest);
            break;
        };
        let dollar_index = index + offset;
        if dollar_index > index {
            let literal: String = characters[index..dollar_index].iter().collect();
            append_literal(&mut parts, &literal);
        }
        let next_char = characters.get(dollar_index + 1).copied();
        match next_char {
            Some('$') | Some('!') => {
                append_literal(&mut parts, &next_char.unwrap().to_string());
                index = dollar_index + 2;
            }
            Some('{') => {
                let end = characters[dollar_index + 2..]
                    .iter()
                    .position(|ch| *ch == '}')
                    .map(|position| dollar_index + 2 + position);
                match end {
                    Some(end_index) => {
                        let name: String =
                            characters[dollar_index + 2..end_index].iter().collect();
                        if env_var_name(&name) {
                            parts.push(TemplatePart::Env(name));
                        } else {
                            let literal: String =
                                characters[dollar_index..=end_index].iter().collect();
                            append_literal(&mut parts, &literal);
                        }
                        index = end_index + 1;
                    }
                    None => {
                        append_literal(&mut parts, "$");
                        index = dollar_index + 1;
                    }
                }
            }
            _ => {
                let rest: String = characters[dollar_index + 1..].iter().collect();
                match env_var_name_prefix(&rest) {
                    Some(name) => {
                        index = dollar_index + 1 + name.chars().count();
                        parts.push(TemplatePart::Env(name));
                    }
                    None => {
                        append_literal(&mut parts, "$");
                        index = dollar_index + 1;
                    }
                }
            }
        }
    }
    parts
}

fn parse_reference(config: &str) -> ConfigValueReference {
    if config.starts_with('!') {
        ConfigValueReference::Command(config.to_string())
    } else {
        ConfigValueReference::Template(parse_template(config))
    }
}

fn resolve_env(name: &str, env: Option<&BTreeMap<String, String>>) -> Option<String> {
    env.and_then(|env| env.get(name).cloned().filter(|value| !value.is_empty()))
        .or_else(|| std::env::var(name).ok().filter(|value| !value.is_empty()))
}

fn resolve_template(
    parts: &[TemplatePart],
    env: Option<&BTreeMap<String, String>>,
) -> Option<String> {
    let mut resolved = String::new();
    for part in parts {
        match part {
            TemplatePart::Literal(value) => resolved.push_str(value),
            TemplatePart::Env(name) => resolved.push_str(&resolve_env(name, env)?),
        }
    }
    Some(resolved)
}

/// pi: `isCommandConfigValue`
pub fn is_command_config_value(config: &str) -> bool {
    matches!(parse_reference(config), ConfigValueReference::Command(_))
}

/// pi: `getConfigValueEnvVarNames`
pub fn get_config_value_env_var_names(config: &str) -> Vec<String> {
    match parse_reference(config) {
        ConfigValueReference::Template(parts) => {
            let mut names = Vec::new();
            for part in parts {
                if let TemplatePart::Env(name) = part {
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
            names
        }
        ConfigValueReference::Command(_) => Vec::new(),
    }
}

fn execute_command_uncached(command_config: &str) -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let output = Command::new(shell)
        .arg("-lc")
        .arg(&command_config[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn execute_command(command_config: &str) -> Option<String> {
    if let Some(cached) = COMMAND_RESULT_CACHE
        .lock()
        .expect("config value cache")
        .get(command_config)
    {
        return cached.clone();
    }
    let result = execute_command_uncached(command_config);
    COMMAND_RESULT_CACHE
        .lock()
        .expect("config value cache")
        .insert(command_config.to_string(), result.clone());
    result
}

/// pi: `resolveConfigValue`
pub fn resolve_config_value(
    config: &str,
    env: Option<&BTreeMap<String, String>>,
) -> Option<String> {
    match parse_reference(config) {
        ConfigValueReference::Command(command) => execute_command(&command),
        ConfigValueReference::Template(parts) => resolve_template(&parts, env),
    }
}

/// pi: `resolveConfigValueUncached`
pub fn resolve_config_value_uncached(
    config: &str,
    env: Option<&BTreeMap<String, String>>,
) -> Option<String> {
    match parse_reference(config) {
        ConfigValueReference::Command(command) => execute_command_uncached(&command),
        ConfigValueReference::Template(parts) => resolve_template(&parts, env),
    }
}

/// pi: `resolveConfigValueOrThrow`
pub fn resolve_config_value_or_throw(
    config: &str,
    description: &str,
    env: Option<&BTreeMap<String, String>>,
) -> Result<String, String> {
    if let Some(value) = resolve_config_value_uncached(config, env) {
        return Ok(value);
    }
    match parse_reference(config) {
        ConfigValueReference::Command(command) => Err(format!(
            "Failed to resolve {description} from shell command: {}",
            &command[1..]
        )),
        ConfigValueReference::Template(_) => {
            let missing: Vec<String> = get_config_value_env_var_names(config)
                .into_iter()
                .filter(|name| resolve_env(name, env).is_none())
                .collect();
            if missing.len() == 1 {
                Err(format!(
                    "Failed to resolve {description} from environment variable: {}",
                    missing[0]
                ))
            } else if missing.len() > 1 {
                Err(format!(
                    "Failed to resolve {description} from environment variables: {}",
                    missing.join(", ")
                ))
            } else {
                Err(format!("Failed to resolve {description}"))
            }
        }
    }
}

/// pi: `clearConfigValueCache`
pub fn clear_config_value_cache() {
    COMMAND_RESULT_CACHE
        .lock()
        .expect("config value cache")
        .clear();
}

/// 把一份值写进输出（`resolve_config_value_or_throw` 的调用方用不到；留给以后）。
#[allow(dead_code)]
fn write_value(value: &str) -> std::io::Result<()> {
    std::io::stdout().write_all(value.as_bytes())
}
