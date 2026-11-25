//! 移植自上游 `packages/schema/test/coerce-typebox.test.ts`（TypeBox 1.3.27 的 `Value.Convert` 测试，
//! MIT，(c) Haydn Paterson）。
//!
//! 上游用 TypeBox 现搭 schema。TypeBox 1.x 的 schema 就是 JSON Schema，转换只看 JSON 结构，不看 TypeBox
//! 的 kind 标记，所以这里逐条重跑的是同一批 schema 与值。
//!
//! 用例来自 `scripts/port/schema-coerce/typebox-cases.json`：`scripts/port/extract-coerce-typebox.mjs`
//! 在 pi 的仓库里跑上游那份测试，把每次 `Value.Convert` / `Value.Check` 的（schema、值、结果）记下来。
//! 记下的结果就是 pi 当时的输出，所以每一条都是“与 pi 相同”的证据；重新生成的办法见脚本开头。
//!
//! 用 bigint、undefined、Symbol、函数的值在 Rust 侧没有对应值（`PORTING.md` 差异表 #40），抽取时略过。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use xjpcode_schema::coerce::Coercer;
use xjpcode_schema::compiler::Compiler;
use xjpcode_schema::formats::index::formats;
use xjpcode_schema::js;
use xjpcode_schema::types::CompileOptions;
use xjpcode_schema::{compile, equal};

fn cases_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/port/schema-coerce/typebox-cases.json")
}

fn read_cases() -> Vec<Value> {
    let path = cases_path();
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let document: Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
    document["cases"]
        .as_array()
        .expect("typebox-cases.json lists cases")
        .clone()
}

fn options() -> CompileOptions {
    CompileOptions {
        formats: Some(formats().clone()),
        ..CompileOptions::default()
    }
}

/// 上游 `Value.Convert`，即第 2 趟单独跑。
fn convert(schema: &Value, value: &Value) -> Value {
    let options = options();
    let compiler = Compiler::new(schema, &options, false);
    Coercer::new(compiler, schema).convert_only(value)
}

#[test]
fn converts_like_typebox_value_convert() {
    let cases = read_cases();
    assert_eq!(
        cases.len(),
        207,
        "typebox-cases.json must hold every recorded case; rerun scripts/port/extract-coerce-typebox.mjs"
    );
    let mut mismatches: Vec<String> = Vec::new();
    for case in &cases {
        let kind = case["kind"].as_str().expect("a kind");
        let name = format!(
            "{} > {}",
            case["group"].as_str().unwrap_or(""),
            case["name"].as_str().unwrap_or("")
        );
        let schema = &case["schema"];
        let value = &case["value"];
        match kind {
            "convert" => {
                let actual = convert(schema, value);
                let expected = &case["expected"];
                if !equal(&actual, expected) {
                    mismatches.push(format!(
                        "{name}\n  schema:   {}\n  value:    {}\n  pi:       {}\n  xjpcode:  {}",
                        js::stringify(schema),
                        js::stringify(value),
                        js::stringify(expected),
                        js::stringify(&actual)
                    ));
                }
            }
            "check" => {
                let options = options();
                let validator = compile(schema, &options)
                    .unwrap_or_else(|error| panic!("{name}: compile: {error}"));
                let actual = validator.check(value);
                let expected = case["expected"].as_bool().expect("a check result");
                if actual != expected {
                    mismatches.push(format!(
                        "{name}\n  schema:   {}\n  value:    {}\n  pi:       check {expected}\n  xjpcode:  check {actual}",
                        js::stringify(schema),
                        js::stringify(value)
                    ));
                }
            }
            other => panic!("unknown case kind {other}"),
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} of {} TypeBox cases disagree with pi:\n\n{}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n\n")
    );
}
