//! 与上游逐条对账（上游没有这两个文件：`scripts/port/` 是移植期的工具，见 `PORTING.md`）。
//!
//! 两批输入，两边读到的值都必须一字不差（按 `JSON.stringify` 的文本比：键序与数字写法都在内）：
//!
//! - `scripts/port/ai-streaming-json/cases.json`：手写用例，覆盖每条读取规矩与每处已知差异，
//!   连每一段的改动一起比；
//! - 上游 `test/data/json-test-suite.json` 的 316 份文档，加上用上游那份生成器造的 160 份随机文档。
//!
//! pi 那一侧的输出记在 `scripts/port/ai-streaming-json/pi.json` 与 `suite-pi.json`，重算用
//! `scripts/port/compare-ai-streaming-json.sh`。pi 读得出来、Rust 装不下的输入（`Infinity` 之类的大数、
//! 孤立代理项、被分段拆开的原始代理对）记在 `skipped` 里：能记的输入照样重放，断言我们读得下去、
//! 读出来的是普通 JSON；剩下的由 `streaming_json.rs` 的用例守着（差异表 #101、#104）。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use xjpcode_ai::utils::streaming_json::{
    JsonChange, JsonPath, JsonPathSegment, StreamingJsonParser,
};
use xjpcode_schema::js;

fn port_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/port/ai-streaming-json")
}

fn read_json(path: &Path) -> Value {
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn path_json(path: &JsonPath) -> Value {
    Value::Array(
        path.iter()
            .map(|segment| match segment {
                JsonPathSegment::Key(key) => Value::String(key.clone()),
                JsonPathSegment::Index(index) => json!(index),
            })
            .collect(),
    )
}

fn change_json(change: &JsonChange) -> Value {
    match change {
        JsonChange::Set { path, value } => json!({
            "kind": "set",
            "path": path_json(path),
            "value": js::stringify(value),
        }),
        JsonChange::Append { path, text } => json!({
            "kind": "append",
            "path": path_json(path),
            "text": text,
        }),
    }
}

/// 重放一份记录里的分段，返回每一段之后的快照（按 pi 的写法）与改动。
fn replay(pieces: &[Value]) -> Vec<(Option<String>, Vec<Value>)> {
    replay_lenient(pieces)
        .into_iter()
        .map(|(snapshot, changes, error)| {
            if let Some(error) = error {
                panic!("append failed: {error}");
            }
            (snapshot, changes)
        })
        .collect()
}

/// 同上，但把读不了的记下来（`skipped` 那一批里有 pi 读成 `Infinity`、Rust 读不了的大数）。
fn replay_lenient(pieces: &[Value]) -> Vec<(Option<String>, Vec<Value>, Option<String>)> {
    let mut parser = StreamingJsonParser::new();
    let mut steps = Vec::new();
    for piece in pieces {
        let piece = piece.as_str().expect("a recorded piece is a string");
        let (changes, error) = match parser.append(piece) {
            Ok(changes) => (changes, None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        let snapshot = parser.snapshot().map(|value| js::stringify(&value));
        steps.push((snapshot, changes.iter().map(change_json).collect(), error));
    }
    steps
}

#[test]
fn reads_the_hand_written_cases_exactly_like_pi() {
    let recorded = read_json(&port_dir().join("pi.json"));
    let cases = recorded["cases"].as_array().expect("cases");
    let cases_file = read_json(&port_dir().join("cases.json"));
    assert_eq!(
        cases.len(),
        cases_file.as_array().expect("cases.json").len(),
        "both files must list the same cases; rerun scripts/port/compare-ai-streaming-json.sh"
    );
    let mut compared = 0;
    for case in cases {
        let name = case["name"].as_str().expect("a name");
        for run in case["runs"].as_array().expect("runs") {
            let chunk = &run["chunk"];
            let pieces = run["pieces"].as_array().expect("pieces");
            let expected = run["steps"].as_array().expect("steps");
            assert_eq!(pieces.len(), expected.len(), "{name} (chunk {chunk})");
            let steps = replay(pieces);
            for (index, (snapshot, changes)) in steps.iter().enumerate() {
                assert_eq!(
                    snapshot,
                    &expected[index]["snapshot"].as_str().map(str::to_string),
                    "{name} (chunk {chunk}): snapshot after piece {index}"
                );
                assert_eq!(
                    Value::Array(changes.clone()),
                    expected[index]["changes"],
                    "{name} (chunk {chunk}): changes after piece {index}"
                );
                compared += 1;
            }
        }
    }
    assert!(compared > 1000, "only {compared} steps compared");
}

#[test]
fn reads_the_suite_and_random_documents_exactly_like_pi() {
    let recorded = read_json(&port_dir().join("suite-pi.json"));
    let documents = recorded["documents"].as_array().expect("documents");
    assert_eq!(
        documents.len(),
        316 + 160,
        "the suite has 316 documents and the random corpus 160; rerun the script"
    );
    let mut compared = 0;
    for document in documents {
        let name = document["name"].as_str().expect("a name");
        let text = document["text"].as_str().expect("text");
        for run in document["runs"].as_array().expect("runs") {
            let chunk = &run["chunk"];
            let pieces = run["pieces"].as_array().expect("pieces");
            let expected = run["snapshots"].as_array().expect("snapshots");
            assert_eq!(pieces.len(), expected.len(), "{name} (chunk {chunk})");
            for (index, (snapshot, _)) in replay(pieces).iter().enumerate() {
                assert_eq!(
                    snapshot,
                    &expected[index].as_str().map(str::to_string),
                    "{name} (chunk {chunk}): snapshot after piece {index}: {text:?}"
                );
                compared += 1;
            }
        }
    }
    assert!(compared > 5000, "only {compared} steps compared");
}

#[test]
fn reads_the_inputs_pi_can_represent_but_rust_cannot_without_panicking() {
    let recorded = read_json(&port_dir().join("suite-pi.json"));
    let skipped = recorded["skipped"].as_array().expect("skipped");
    assert!(
        !skipped.is_empty(),
        "the suite has inputs Rust cannot represent"
    );
    let cases = read_json(&port_dir().join("pi.json"));
    let skipped_cases = cases["skipped"].as_array().expect("skipped cases");
    let mut replayed = 0;
    for entry in skipped.iter().chain(skipped_cases) {
        let reason = entry["reason"].as_str().expect("a reason");
        assert!(
            reason.starts_with("a snapshot holds a value Rust cannot represent")
                || reason.starts_with("a piece splits a surrogate pair")
                || reason.starts_with("deeper than serde_json's recursion limit"),
            "unexpected skip reason: {reason}"
        );
        // 分段拆开代理对的那一批记不了分段（记下来就不是 Rust 装得下的文字了），跳过。
        let Some(pieces) = entry["pieces"].as_array() else {
            continue;
        };
        // 孤立代理项我们读成 U+FFFD；`Infinity` 那一档、深过 127 层的文字按 Rust 的读法报错
        // （差异表 #101、#102）。
        for (_, _, error) in replay_lenient(pieces) {
            if let Some(error) = error {
                assert!(
                    reason.starts_with("a snapshot holds a value Rust cannot represent")
                        || reason.starts_with("deeper than serde_json's recursion limit"),
                    "{reason}: {error}"
                );
            }
            replayed += 1;
        }
    }
    assert!(replayed > 0, "some skipped runs are replayable");
}
