//! 移植自上游 `packages/schema/test/conformance.test.ts`。
//!
//! 官方 JSON-Schema-Test-Suite，drafts 2020-12 与 7：每个用例，含 optional 与 format。列在
//! `KNOWN_FAILURES` 里的用例必须不成立；有一条开始成立了，就把它从表里删掉。
//!
//! 上游这个文件每条用例有两个断言：`check()` 的结论，以及 `errors()` 与 `check()` 一致。两个都照搬。

mod suite;

use xjpcode_schema::compile;

const KNOWN_FAILURES: &[&str] = &[
    // 自定义 meta-schema 关掉的词汇表不支持。
    "draft2020-12/vocabulary > schema that uses custom metaschema with with no validation vocabulary > no validation: invalid number, but it still validates",
    "draft2020-12/optional/format-assertion > schema that uses custom metaschema with format-assertion: false > format-assertion: false: invalid string",
    "draft2020-12/optional/format-assertion > schema that uses custom metaschema with format-assertion: true > format-assertion: true: invalid string",
    // 到别的 draft 的文档去的 $ref 按本次编译的 draft 求值。
    "draft2020-12/optional/cross-draft > refs to historic drafts are processed as historic drafts > first item not a string is valid",
    "draft7/optional/cross-draft > refs to future drafts are processed as future drafts > missing bar is invalid",
    // contentMediaType 和 contentEncoding 是注解。
    "draft7/optional/content > validation of string-encoded content based on media type > an invalid JSON document",
    "draft7/optional/content > validation of binary string-encoding > an invalid base64 string (% is not a valid character)",
    "draft7/optional/content > validation of binary-encoded media type documents > a validly-encoded invalid JSON document",
    "draft7/optional/content > validation of binary-encoded media type documents > an invalid base64 string that is valid JSON",
];

/// 这份 fixture 的用例总数（drafts 2020-12 + 7）。换 fixture 时改这里。
const SUITE_CASES: usize = 4213;

#[test]
fn conformance() {
    let schemas = suite::suite_schemas();
    let mut ran = 0usize;
    let mut mismatches: Vec<String> = Vec::new();

    for file in suite::suite_files() {
        // 一份 schema 只编译一次：上游每条用例各编译一次，这里同一组的用例共用一份，结果相同。
        let options = suite::options_for(&file, &schemas);
        for group in &file.groups {
            // 一组一份 schema：同组用例的 schema 相同，编译一次就够。
            let validator = match compile(&group.schema, &options) {
                Ok(validator) => validator,
                Err(error) => {
                    mismatches.push(format!(
                        "{} > {} > (compile failed: {error})",
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
                ran += 1;
                let actual = validator.check(&test.data);
                let known = KNOWN_FAILURES.contains(&key.as_str());
                // 已知失败：pi 的结论与用例的期望相反（vitest 的 `it.fails`）。
                let expected = if known { !test.valid } else { test.valid };
                if actual != expected {
                    mismatches.push(format!(
                        "{key}: check() = {actual}, pi = {expected}{}",
                        if known { " (known failure)" } else { "" }
                    ));
                }
                // 上游同一份文件里每条用例还有第二个断言：`errors()` 与 `check()` 同结论。
                match validator.errors(&test.data) {
                    Ok(errors) => {
                        if errors.is_empty() != actual {
                            mismatches.push(format!(
                                "{key}: errors() = {} errors, check() = {actual}",
                                errors.len()
                            ));
                        }
                    }
                    Err(error) => {
                        mismatches.push(format!("{key}: errors() failed: {error}"));
                    }
                }
            }
        }
    }

    assert_eq!(
        ran, SUITE_CASES,
        "the vendored suite must run every case; the loader skipped or duplicated some"
    );
    assert!(
        mismatches.is_empty(),
        "{} of {} cases disagree with pi:\n{}",
        mismatches.len(),
        ran,
        mismatches.join("\n")
    );
}
