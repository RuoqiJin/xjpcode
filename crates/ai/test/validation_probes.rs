//! 移植自上游 `packages/ai/test/validation-probes.test.ts`（评审者探针）。
//!
//! 上游用 TypeBox 现搭 schema；这里跑的是 `scripts/port/ai-validation/probes.json`：
//! `scripts/port/extract-validation-probes.mjs` 在 pi 的仓库里跑上游那份测试，把每次
//! `validateToolArguments` 调用的（工具名、schema、参数、pi 的结论）记下来。结论的写法与上游测试里的
//! `outcome()` 相同：`ok <参数>` / `REJECT <第一行错误>` / `THROW <第一行>`。记下的就是 pi 在那时那刻
//! 的输出，所以每一条都是“与 pi 相同”的证据。
//!
//! 没有对应值的用例在抽取时略过（`PORTING.md` 差异表 #40、#51、#66、#68、#71）：`Type.BigInt()` 的
//! 结果（Rust 侧没有 bigint）、孤立代理项的参数（`serde_json` 装不下）、`Undefined from JSON null`
//! （pi 把 null 变成 undefined，JSON 里没有这个值）、`late`（pi 的 `WeakMap` 缓存让改过的 schema 仍用
//! 旧 validator；Rust 侧每次现编）、`mcp-invalid-regex`（两边正则引擎的报错文字不同）。
//!
//! 重新生成：`scripts/port/extract-validation-probes.mjs`（或 `compare-ai-validation.sh`）。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use xjpcode_ai::types::{Tool, ToolCall};
use xjpcode_ai::validate_tool_arguments;
use xjpcode_schema::js;

/// 记录文件里的用例数：抽取脚本漏读或漏记时，这条断言先报。
const PROBES: usize = 159;

fn cases_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/port/ai-validation/probes.json")
}

fn read_cases() -> Vec<Value> {
    let path = cases_path();
    let text =
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let document: Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
    document["cases"]
        .as_array()
        .expect("probes.json lists cases")
        .clone()
}

/// 上游测试里的 `outcome()`：一条调用给出的结论，写成一行。
fn outcome(name: &str, schema: Value, arguments: &serde_json::Map<String, Value>) -> String {
    let tool = Tool {
        name: name.to_string(),
        description: String::new(),
        parameters: schema,
        constrained_sampling: None,
    };
    let call = ToolCall::new("1", name, arguments.clone());
    match validate_tool_arguments(&tool, &call) {
        Ok(value) => format!("ok {}", js::stringify(&value)),
        Err(message) => {
            if message.starts_with("Validation failed") {
                let first = message.split('\n').nth(1).unwrap_or("").trim();
                format!("REJECT {first}")
            } else {
                let first = message.split('\n').next().unwrap_or("");
                format!("THROW {first}")
            }
        }
    }
}

#[test]
fn probes_give_the_outcomes_pi_gives() {
    let cases = read_cases();
    assert_eq!(
        cases.len(),
        PROBES,
        "probes.json must hold every recorded probe; rerun scripts/port/extract-validation-probes.mjs"
    );
    let mut mismatches: Vec<String> = Vec::new();
    for case in &cases {
        let name = case["name"].as_str().expect("a case has a name");
        let arguments = case["arguments"]
            .as_object()
            .expect("a case has an arguments object");
        let actual = outcome(name, case["schema"].clone(), arguments);
        let expected = case["expected"].as_str().expect("a case has an outcome");
        if actual != expected {
            mismatches.push(format!(
                "{name} {}\n  pi:      {expected}\n  xjpcode: {actual}",
                js::stringify(&case["arguments"])
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} of {} probes disagree with pi:\n\n{}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n\n")
    );
}
