//! 与上游逐条对账 `coerce()` 与 `parse()`（上游没有这几个文件：`scripts/port/` 是移植期的工具，
//! 见 `PORTING.md`）。
//!
//! 两批输入，两边给的纠正结果必须一字不差（按 `JSON.stringify` 的文本比，键序与数字写法都在内）：
//!
//! - `scripts/port/schema-coerce/cases.json`：手写用例，每个转换规则与两边天生不同的地方；
//! - 官方 JSON-Schema-Test-Suite（`test/fixtures`，与一致性测试同一批）。
//!
//! pi 那一侧的输出记在 `scripts/port/schema-coerce/pi.json` 与 `suite-pi.json`，
//! 重算用 `scripts/port/compare-schema-coerce.mjs`。

mod suite;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use xjpcode_schema::formats::index::formats;
use xjpcode_schema::js;
use xjpcode_schema::types::{
    CompileOptions, Draft, ParseResult, UnresolvedRefs, ValidationError,
};
use xjpcode_schema::compile;

fn port_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/port/schema-coerce")
}

fn read_json(path: &Path) -> Value {
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

/// 用例里的 `options`：只认上游 `CompileOptions` 的那几项。
fn options_of(case: &Value) -> CompileOptions {
    let Some(options) = case.get("options").and_then(Value::as_object) else {
        return CompileOptions {
            coerce: true,
            ..CompileOptions::default()
        };
    };
    let mut compiled = CompileOptions {
        coerce: true,
        ..CompileOptions::default()
    };
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

/// 数字的表示与 pi 交给下一个使用方的一致：`JSON.stringify` 把整数值写成 `10`，`serde_json` 会写
/// `10.0`；工具按整数读参数（`as_i64`/`as_u64`），所以整数位置必须是整数表示。数值相等由
/// `js::stringify` 逐字守住，这里只比“整数还是浮点”。
fn same_number_representation(ours: &Value, theirs: &Value) -> bool {
    match (ours, theirs) {
        (Value::Number(ours), Value::Number(theirs)) => {
            // 2^53 以上 pi 舍成 double、我们按整数精确保留（差异表 #65）：这一档不比表示。
            let beyond_safe = |number: &serde_json::Number| {
                number.as_f64().is_some_and(|x| x.abs() > 9007199254740991.0)
            };
            beyond_safe(ours)
                || beyond_safe(theirs)
                || (ours.is_i64() || ours.is_u64()) == (theirs.is_i64() || theirs.is_u64())
        }
        (Value::Array(ours), Value::Array(theirs)) => {
            ours.len() == theirs.len()
                && ours
                    .iter()
                    .zip(theirs)
                    .all(|(ours, theirs)| same_number_representation(ours, theirs))
        }
        (Value::Object(ours), Value::Object(theirs)) => {
            ours.len() == theirs.len()
                && ours.iter().all(|(key, ours)| {
                    theirs
                        .get(key)
                        .is_some_and(|theirs| same_number_representation(ours, theirs))
                })
        }
        _ => true,
    }
}

/// `parse` 的结果与记下的比：`{ ok: true, value }` / `{ ok: false, errors }`。
fn parse_matches(result: &ParseResult, recorded: &Value) -> bool {
    if recorded["ok"] == Value::Bool(true) {
        let ParseResult::Ok { value } = result else {
            return false;
        };
        return js::stringify(value) == js::stringify(&recorded["value"]);
    }
    let ParseResult::Errors { errors } = result else {
        return false;
    };
    as_json(errors) == recorded["errors"]
}

#[test]
fn coerce_and_parse_match_pi_case_by_case() {
    let cases = read_json(&port_dir().join("cases.json"));
    let cases = cases.as_array().expect("cases.json is an array");
    let recorded = read_json(&port_dir().join("pi.json"));
    let recorded = recorded.as_array().expect("pi.json is an array");

    assert_eq!(
        cases.len(),
        recorded.len(),
        "cases.json and pi.json must list the same cases; rerun scripts/port/compare-schema-coerce.mjs --update"
    );

    let mut mismatches: Vec<String> = Vec::new();
    for (case, pi) in cases.iter().zip(recorded.iter()) {
        let name = case["name"].as_str().expect("a case has a name");
        assert_eq!(name, pi["name"], "cases.json and pi.json are out of order");
        let options = options_of(case);
        let compiled = compile(&case["schema"], &options);
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
        if let Some(theirs) = pi.get("coerceError").and_then(Value::as_str) {
            mismatches.push(format!(
                "{name}\n  pi: coerce threw {theirs}; the port has no comparable value"
            ));
            continue;
        }
        let validator = match compiled {
            Ok(validator) => validator,
            Err(error) => {
                mismatches.push(format!("{name}\n  compile failed: {error}"));
                continue;
            }
        };
        let ours = match validator.coerce(&case["value"]) {
            Ok(value) => value,
            Err(error) => {
                mismatches.push(format!("{name}\n  coerce failed: {error}"));
                continue;
            }
        };
        let theirs = &pi["coerce"];
        if js::stringify(&ours) != js::stringify(theirs) {
            mismatches.push(format!(
                "{name}\n  pi:      {}\n  xjpcode: {}",
                js::stringify(theirs),
                js::stringify(&ours)
            ));
            continue;
        }
        if !same_number_representation(&ours, theirs) {
            mismatches.push(format!(
                "{name}\n  pi:      {} (integers as JSON integers)\n  xjpcode: {} (a float where pi has an integer)",
                js::stringify(theirs),
                js::stringify(&ours)
            ));
            continue;
        }
        if pi["parse"].is_null() {
            continue;
        }
        let ours = match validator.parse(&case["value"]) {
            Ok(result) => result,
            Err(error) => {
                mismatches.push(format!("{name}\n  parse failed: {error}"));
                continue;
            }
        };
        if !parse_matches(&ours, &pi["parse"]) {
            mismatches.push(format!(
                "{name}\n  pi:      {}\n  xjpcode: {}",
                serde_json::to_string(&pi["parse"]).expect("json"),
                match &ours {
                    ParseResult::Ok { value } => {
                        json!({ "ok": true, "value": value }).to_string()
                    }
                    ParseResult::Errors { errors } => {
                        json!({ "ok": false, "errors": as_json(errors) }).to_string()
                    }
                }
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
fn suite_coerce_matches_pi_case_by_case() {
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
                match validator.coerce(&test.data) {
                    Ok(value) => {
                        ours.insert(key, value);
                    }
                    Err(error) => failures.push(format!("{key}: coerce failed: {error}")),
                }
            }
        }
    }

    assert_eq!(
        ours.len(),
        recorded.len(),
        "the fixtures and suite-pi.json must list the same cases; rerun scripts/port/compare-schema-coerce.mjs --suite --update"
    );

    let mut mismatches: Vec<String> = Vec::new();
    for pi in recorded {
        let name = pi["name"].as_str().expect("a case has a name");
        let Some(found) = ours.get(name) else {
            mismatches.push(format!("{name}\n  missing from the fixtures"));
            continue;
        };
        let theirs = js::stringify(&pi["coerce"]);
        if js::stringify(found) != theirs {
            mismatches.push(format!(
                "{name}\n  pi:      {theirs}\n  xjpcode: {}",
                js::stringify(found)
            ));
        } else if !same_number_representation(found, &pi["coerce"]) {
            mismatches.push(format!(
                "{name}\n  pi:      {theirs} (integers as JSON integers)\n  xjpcode: {} (a float where pi has an integer)",
                js::stringify(found)
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
