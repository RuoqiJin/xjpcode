//! 移植自上游 `packages/schema/test/suite.ts`。
//!
//! 读取随源码搬进来的 JSON-Schema-Test-Suite：测试文件、远程文档和官方 meta-schema。
//! 辅助模块，不是测试目标，因此不在 `Cargo.toml` 的 `[[test]]` 里登记。
//!
//! 几个测试各自 `mod suite;` 进来，各用其中一部分（一致性测试用 `valid`，对照程序不用）。

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use xjpcode_schema::formats::index::formats;
use xjpcode_schema::types::{CompileOptions, Draft, JsonSchema};

/// 一个测试文件：它在 `tests/` 之下的路径和它的分组。
pub struct SuiteFile {
    pub name: String,
    pub draft: Draft,
    pub groups: Vec<SuiteGroup>,
}

/// 一份 schema 和一组值，以及每个值合不合格。
pub struct SuiteGroup {
    pub description: String,
    pub schema: JsonSchema,
    pub tests: Vec<SuiteTest>,
}

/// 一个值，以及它合不合格。
pub struct SuiteTest {
    pub description: String,
    pub data: Value,
    pub valid: bool,
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test")
        .join("fixtures")
}

fn suite_root() -> PathBuf {
    fixtures().join("json-schema-test-suite")
}

/// drafts 2020-12 与 7 的每个测试文件。
pub fn suite_files() -> Vec<SuiteFile> {
    let mut files = Vec::new();
    for (directory, draft) in [
        ("draft2020-12", Draft::Draft202012),
        ("draft7", Draft::Draft7),
    ] {
        for path in json_files(&suite_root().join("tests").join(directory)) {
            let name = path
                .strip_prefix(suite_root().join("tests"))
                .expect("below tests/")
                .to_string_lossy()
                .replace('\\', "/")
                .trim_end_matches(".json")
                .to_string();
            files.push(SuiteFile {
                name,
                draft,
                groups: parse_groups(&read_json(&path)),
            });
        }
    }
    files
}

/// 测试引用到的远程文档和 meta-schema，按 URI（每次都是新副本）。
pub fn suite_schemas() -> BTreeMap<String, JsonSchema> {
    let mut schemas = BTreeMap::new();
    let remotes = suite_root().join("remotes");
    for path in json_files(&remotes) {
        let name = path
            .strip_prefix(&remotes)
            .expect("below remotes/")
            .to_string_lossy()
            .replace('\\', "/");
        schemas.insert(format!("http://localhost:1234/{name}"), read_json(&path));
    }
    let meta = fixtures().join("meta-schemas");
    schemas.insert(
        "http://json-schema.org/draft-07/schema".to_string(),
        read_json(&meta.join("draft-07-schema.json")),
    );
    schemas.insert(
        "https://json-schema.org/draft/2020-12/schema".to_string(),
        read_json(&meta.join("draft2020-12-schema.json")),
    );
    for vocabulary in [
        "core",
        "applicator",
        "unevaluated",
        "validation",
        "meta-data",
        "format-annotation",
        "content",
    ] {
        schemas.insert(
            format!("https://json-schema.org/draft/2020-12/meta/{vocabulary}"),
            read_json(&meta.join(format!("draft2020-12-meta-{vocabulary}.json"))),
        );
    }
    schemas
}

/// 一份 schema 的编译选项：draft、已登记的文档，`format` 测试才给格式检查。
pub fn options_for(file: &SuiteFile, schemas: &BTreeMap<String, JsonSchema>) -> CompileOptions {
    let mut options = CompileOptions::new(file.draft);
    options.schemas = schemas.clone();
    // format 测试是在断言 format；别处 `format` 是注解，与 draft 2020-12 的规定一致。
    if file.name.contains("/optional/format/") {
        options.formats = Some(formats().clone());
    }
    options
}

fn parse_groups(document: &Value) -> Vec<SuiteGroup> {
    document
        .as_array()
        .expect("suite file is an array of groups")
        .iter()
        .map(|group| SuiteGroup {
            description: string_of(&group["description"]),
            schema: group["schema"].clone(),
            tests: group["tests"]
                .as_array()
                .expect("group has tests")
                .iter()
                .map(|test| SuiteTest {
                    description: string_of(&test["description"]),
                    data: test["data"].clone(),
                    valid: test["valid"].as_bool().expect("valid is a boolean"),
                })
                .collect(),
        })
        .collect()
}

fn string_of(value: &Value) -> String {
    value.as_str().expect("string").to_string()
}

fn read_json(path: &Path) -> Value {
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn json_files(directory: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read dir {}: {error}", directory.display()))
        .map(|entry| entry.expect("dir entry").path())
        .collect();
    entries.sort();
    let mut out = Vec::new();
    for path in entries {
        if path.is_dir() {
            out.extend(json_files(&path));
        } else if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            out.push(path);
        }
    }
    out
}
