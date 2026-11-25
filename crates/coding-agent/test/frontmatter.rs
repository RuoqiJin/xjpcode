//! 移植自上游 `test/frontmatter.test.ts`（`utils/frontmatter.ts`）。
//!
//! 上游返回一张任意键的表；这里只留技能读的三项，所以 `foo-bar` 那条只验证"不报错、
//! 正文照旧"，不比对未知键（见 `docs/port/milestone-4.md`）。

use xjpcode_coding_agent::core::skills::{parse_frontmatter, strip_frontmatter};

#[test]
fn parses_keys_strips_quotes_and_returns_body() {
    let input = "---\nname: \"skill-name\"\ndescription: 'A desc'\nfoo-bar: value\n---\n\nBody text";
    let (frontmatter, body) = parse_frontmatter(input).unwrap();
    assert_eq!(frontmatter.name.as_deref(), Some("skill-name"));
    assert_eq!(frontmatter.description.as_deref(), Some("A desc"));
    assert_eq!(body, "Body text");
}

#[test]
fn normalizes_newlines_and_handles_crlf() {
    let input = "---\r\nname: test\r\n---\r\nLine one\r\nLine two";
    let (_, body) = parse_frontmatter(input).unwrap();
    assert_eq!(body, "Line one\nLine two");
}

#[test]
fn rejects_invalid_yaml_frontmatter() {
    let input = "---\nfoo: [bar\n---\nBody";
    let error = parse_frontmatter(input).unwrap_err();
    assert!(error.contains("line 1"), "{error}");
}

#[test]
fn parses_multiline_yaml_syntax() {
    let input = "---\ndescription: |\n  Line one\n  Line two\n---\n\nBody";
    let (frontmatter, body) = parse_frontmatter(input).unwrap();
    // pi 的 `yaml` 保留字面量块末尾的换行；serde_yaml 去掉了它。技能里的 description
    // 随后会 trim，所以读出来的技能内容相同（见 `docs/port/milestone-4.md`）。
    assert_eq!(frontmatter.description.as_deref(), Some("Line one\nLine two"));
    assert_eq!(body, "Body");
}

#[test]
fn returns_original_content_when_frontmatter_is_missing_or_unterminated() {
    let no_frontmatter = "Just text\nsecond line";
    let missing_end = "---\nname: test\nBody without terminator";
    assert_eq!(
        parse_frontmatter(no_frontmatter).unwrap().1,
        "Just text\nsecond line"
    );
    assert_eq!(
        parse_frontmatter(missing_end).unwrap().1,
        "---\nname: test\nBody without terminator"
    );
}

#[test]
fn returns_empty_frontmatter_for_empty_or_comment_only_matter() {
    let input = "---\n# just a comment\n---\nBody";
    let (frontmatter, body) = parse_frontmatter(input).unwrap();
    assert_eq!(frontmatter.name, None);
    assert_eq!(frontmatter.description, None);
    assert_eq!(body, "Body");
}

#[test]
fn strip_frontmatter_removes_matter_and_trims_body() {
    let input = "---\nkey: value\n---\n\nBody\n";
    assert_eq!(strip_frontmatter(input), "Body");
}

#[test]
fn strip_frontmatter_returns_body_when_no_frontmatter_present() {
    let input = "\n  No frontmatter body  \n";
    assert_eq!(strip_frontmatter(input), input);
}
