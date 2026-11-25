//! 与上游逐条对账（上游没有这几个文件：`scripts/port/` 是移植期的工具，见 `PORTING.md`）。
//!
//! 两批输入，两边给的 `check` 与 `errors` 都必须一字不差：
//!
//! - `scripts/port/schema-errors/cases.json`：手写用例，覆盖每个关键字与每种消息写法；
//! - 官方 JSON-Schema-Test-Suite（`test/fixtures`，与一致性测试同一批）。
//!
//! pi 那一侧的输出记在 `scripts/port/schema-errors/pi.json` 与 `suite-pi.json`，
//! 重算用 `scripts/port/compare-schema-errors.sh`。

mod suite;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use xjpcode_schema::compile;
use xjpcode_schema::formats::index::formats;
use xjpcode_schema::types::{CompileOptions, Draft, UnresolvedRefs, ValidationError};

fn port_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/port/schema-errors")
}

fn read_json(path: &Path) -> Value {
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

/// 用例里的 `options`：只认上游 `CompileOptions` 的那几项。
fn options_of(case: &Value) -> CompileOptions {
    let Some(options) = case.get("options").and_then(Value::as_object) else {
        return CompileOptions::default();
    };
    let mut compiled = CompileOptions::default();
    if options.get("draft").and_then(Value::as_str) == Some("7") {
        compiled.draft = Draft::Draft7;
    }
    if options.get("formats") == Some(&Value::Bool(true)) {
        compiled.formats = Some(formats().clone());
    }
    if options.get("unresolvedRefs").and_then(Value::as_str) == Some("reject") {
        compiled.unresolved_refs = UnresolvedRefs::Reject;
    }
    if let Some(schemas) = options.get("schemas").and_then(Value::as_object) {
        for (uri, schema) in schemas {
            compiled.schemas.insert(uri.clone(), schema.clone());
        }
    }
    compiled
}

fn as_json(errors: &[ValidationError]) -> Value {
    Value::Array(
        errors
            .iter()
            .map(|error| {
                json!({
                    "path": error.path,
                    "keyword": error.keyword,
                    "message": error.message,
                })
            })
            .collect(),
    )
}

#[test]
fn errors_match_pi_case_by_case() {
    let cases = read_json(&port_dir().join("cases.json"));
    let cases = cases.as_array().expect("cases.json is an array");
    let recorded = read_json(&port_dir().join("pi.json"));
    let recorded = recorded.as_array().expect("pi.json is an array");

    assert_eq!(
        cases.len(),
        recorded.len(),
        "cases.json and pi.json must list the same cases; rerun scripts/port/compare-schema-errors.sh"
    );

    let mut mismatches: Vec<String> = Vec::new();
    for (case, pi) in cases.iter().zip(recorded.iter()) {
        let name = case["name"].as_str().expect("a case has a name");
        assert_eq!(name, pi["name"], "cases.json and pi.json are out of order");
        let options = options_of(case);
        let compiled = compile(&case["schema"], &options);
        // schema 本身写坏的那几条：pi 的 compile 抛错，比的是那句话。
        if let Some(theirs) = pi.get("compileError").and_then(Value::as_str) {
            match compiled {
                Ok(_) => mismatches.push(format!(
                    "{name}\n  pi:      compile error {theirs}\n  xjpcode: compiled"
                )),
                Err(error) if error.message() != theirs => mismatches.push(format!(
                    "{name}\n  pi:      compile error {theirs}\n  xjpcode: compile error {}",
                    error.message()
                )),
                Err(_) => {}
            }
            continue;
        }
        let validator = match compiled {
            Ok(validator) => validator,
            Err(error) => {
                mismatches.push(format!("{name}\n  compile failed: {error}"));
                continue;
            }
        };
        let check = validator.check(&case["value"]);
        let errors = match validator.errors(&case["value"]) {
            Ok(errors) => errors,
            Err(error) => {
                mismatches.push(format!("{name}\n  errors() failed: {error}"));
                continue;
            }
        };
        let ours = json!({ "check": check, "errors": as_json(&errors) });
        let theirs = json!({ "check": pi["check"], "errors": pi["errors"] });
        if ours != theirs {
            mismatches.push(format!(
                "{name}\n  pi:      {theirs}\n  xjpcode: {}",
                serde_json::to_string(&ours).expect("json")
            ));
        }
    }

    assert!(
        mismatches.is_empty(),
        "{} of {} cases disagree with pi:\n\n{}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n\n")
    );
}

#[test]
fn suite_errors_match_pi_case_by_case() {
    let recorded = read_json(&port_dir().join("suite-pi.json"));
    let recorded = recorded.as_array().expect("suite-pi.json is an array");

    let schemas = suite::suite_schemas();
    let mut ours: BTreeMap<String, Value> = BTreeMap::new();
    let mut failures: Vec<String> = Vec::new();
    for file in suite::suite_files() {
        let options = suite::options_for(&file, &schemas);
        for group in &file.groups {
            let validator = match compile(&group.schema, &options) {
                Ok(validator) => validator,
                Err(error) => {
                    failures.push(format!(
                        "{} > {}: compile failed: {error}",
                        file.name, group.description
                    ));
                    continue;
                }
            };
            for test in &group.tests {
                let key = format!(
                    "{} > {} > {}",
                    file.name, group.description, test.description
                );
                let check = validator.check(&test.data);
                match validator.errors(&test.data) {
                    Ok(errors) => {
                        ours.insert(key, json!({ "check": check, "errors": as_json(&errors) }));
                    }
                    Err(error) => failures.push(format!("{key}: errors() failed: {error}")),
                }
            }
        }
    }

    assert_eq!(
        ours.len(),
        recorded.len(),
        "the fixtures and suite-pi.json must list the same cases; rerun scripts/port/compare-schema-errors.sh"
    );

    let mut mismatches: Vec<String> = Vec::new();
    for pi in recorded {
        let name = pi["name"].as_str().expect("a case has a name");
        let Some(found) = ours.get(name) else {
            mismatches.push(format!("{name}\n  missing from the fixtures"));
            continue;
        };
        // pi 在环上抛 `RangeError` 的那几条记成 errors: null，没有可比的一侧。
        if pi["errors"].is_null() {
            continue;
        }
        let theirs = json!({ "check": pi["check"], "errors": pi["errors"] });
        if *found != theirs {
            mismatches.push(format!(
                "{name}\n  pi:      {theirs}\n  xjpcode: {}",
                serde_json::to_string(found).expect("json")
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} case(s) could not run:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
    assert!(
        mismatches.is_empty(),
        "{} of {} cases disagree with pi:\n\n{}",
        mismatches.len(),
        ours.len(),
        mismatches.join("\n\n")
    );
}
