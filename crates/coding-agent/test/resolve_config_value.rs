//! 对应上游 `packages/coding-agent/test/resolve-config-value.test.ts`（没有 Windows 那两段）。

use std::collections::BTreeMap;

use xjpcode_coding_agent::core::resolve_config_value::{
    clear_config_value_cache, get_config_value_env_var_names, is_command_config_value,
    resolve_config_value, resolve_config_value_or_throw, resolve_config_value_uncached,
};

fn scoped(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

#[test]
fn resolves_literals_environment_templates_and_escapes() {
    std::env::set_var("TEST_CONFIG_LEFT", "left");
    std::env::set_var("TEST_CONFIG_RIGHT", "right");
    assert_eq!(
        resolve_config_value("literal-key", None).as_deref(),
        Some("literal-key")
    );
    assert_eq!(
        resolve_config_value("$TEST_CONFIG_LEFT", None).as_deref(),
        Some("left")
    );
    assert_eq!(
        resolve_config_value("${TEST_CONFIG_LEFT}_$TEST_CONFIG_RIGHT", None).as_deref(),
        Some("left_right")
    );
    assert_eq!(
        resolve_config_value("$$TEST_CONFIG_LEFT", None).as_deref(),
        Some("$TEST_CONFIG_LEFT")
    );
    assert_eq!(
        resolve_config_value("$!literal-$TEST_CONFIG_RIGHT", None).as_deref(),
        Some("!literal-right")
    );
    std::env::remove_var("TEST_CONFIG_LEFT");
    std::env::remove_var("TEST_CONFIG_RIGHT");
}

#[test]
fn uses_the_credential_scoped_environment_before_the_process_environment() {
    std::env::set_var("TEST_CONFIG_SCOPED", "process");
    let env = scoped(&[("TEST_CONFIG_SCOPED", "credential")]);
    assert_eq!(
        resolve_config_value("$TEST_CONFIG_SCOPED", Some(&env)).as_deref(),
        Some("credential")
    );
    std::env::remove_var("TEST_CONFIG_SCOPED");
}

#[test]
fn executes_shell_commands_and_trims_their_output() {
    clear_config_value_cache();
    assert_eq!(
        resolve_config_value("!echo '  spaced-key  '", None).as_deref(),
        Some("spaced-key")
    );
    assert_eq!(
        resolve_config_value("!printf 'line1\\nline2'", None).as_deref(),
        Some("line1\nline2")
    );
    assert_eq!(
        resolve_config_value("!echo 'hello world' | tr ' ' '-'", None).as_deref(),
        Some("hello-world")
    );
}

#[test]
fn returns_none_when_a_command_fails() {
    clear_config_value_cache();
    for command in ["!exit 1", "!nonexistent-command-12345", "!printf ''"] {
        assert_eq!(resolve_config_value(command, None), None, "{command}");
    }
}

#[test]
fn caches_successful_and_failed_commands_until_cleared() {
    clear_config_value_cache();
    let dir = tempfile::TempDir::new().unwrap();
    let counter = dir.path().join("counter");
    std::fs::write(&counter, "0").unwrap();
    let path = counter.to_string_lossy().replace('\\', "/");
    let success = format!(
        "!sh -c 'count=$(cat \"{path}\"); echo $((count + 1)) > \"{path}\"; echo value'"
    );
    assert_eq!(resolve_config_value(&success, None).as_deref(), Some("value"));
    assert_eq!(resolve_config_value(&success, None).as_deref(), Some("value"));
    assert_eq!(std::fs::read_to_string(&counter).unwrap().trim(), "1");
    clear_config_value_cache();
    assert_eq!(resolve_config_value(&success, None).as_deref(), Some("value"));
    assert_eq!(std::fs::read_to_string(&counter).unwrap().trim(), "2");

    let failure = format!(
        "!sh -c 'count=$(cat \"{path}\"); echo $((count + 1)) > \"{path}\"; exit 1'"
    );
    assert_eq!(resolve_config_value(&failure, None), None);
    assert_eq!(resolve_config_value(&failure, None), None);
    assert_eq!(std::fs::read_to_string(&counter).unwrap().trim(), "3");
}

#[test]
fn does_not_cache_environment_values() {
    std::env::set_var("TEST_CONFIG_DYNAMIC", "first");
    assert_eq!(
        resolve_config_value("$TEST_CONFIG_DYNAMIC", None).as_deref(),
        Some("first")
    );
    std::env::set_var("TEST_CONFIG_DYNAMIC", "second");
    assert_eq!(
        resolve_config_value("$TEST_CONFIG_DYNAMIC", None).as_deref(),
        Some("second")
    );
    std::env::remove_var("TEST_CONFIG_DYNAMIC");
}

#[test]
fn uncached_resolution_executes_the_command_every_time() {
    let dir = tempfile::TempDir::new().unwrap();
    let counter = dir.path().join("uncached-counter");
    std::fs::write(&counter, "0").unwrap();
    let path = counter.to_string_lossy().replace('\\', "/");
    let command = format!(
        "!sh -c 'count=$(cat \"{path}\"); echo $((count + 1)) > \"{path}\"; echo value'"
    );
    assert_eq!(resolve_config_value_uncached(&command, None).as_deref(), Some("value"));
    assert_eq!(resolve_config_value_uncached(&command, None).as_deref(), Some("value"));
    assert_eq!(std::fs::read_to_string(&counter).unwrap().trim(), "2");
}

#[test]
fn classifies_and_lists_environment_references() {
    assert!(is_command_config_value("!echo key"));
    assert!(!is_command_config_value("$KEY"));
    assert_eq!(
        get_config_value_env_var_names("${A}-$B-$A"),
        vec!["A".to_string(), "B".to_string()]
    );
    assert!(get_config_value_env_var_names("!echo $A").is_empty());
}

#[test]
fn throws_useful_messages_for_unresolvable_values() {
    std::env::remove_var("TEST_CONFIG_MISSING");
    assert_eq!(
        resolve_config_value_or_throw("$TEST_CONFIG_MISSING", "API key", None).unwrap_err(),
        "Failed to resolve API key from environment variable: TEST_CONFIG_MISSING"
    );
    assert_eq!(
        resolve_config_value_or_throw("!exit 1", "API key", None).unwrap_err(),
        "Failed to resolve API key from shell command: exit 1"
    );
}
