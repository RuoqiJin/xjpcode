//! `crates/ai/src/utils/error_body.rs` 的测试。
//!
//! 上游出处：`test/error-body.test.ts`。上游从 SdkErrorShape 上「探」状态与正文；Rust 侧由传输
//! 层显式构造，所以那几条「字段形状」的用例没有对应物（见 `PORTING.md` 差异表 #112）。这里搬
//! 的是同一套拼法与截断：截断上限、空正文、带/不带前缀、消息里已有正文、cause 链。

use xjpcode_ai::utils::error_body::{
    format_provider_error, http_error, plain_error, transport_error, truncate_error_text,
    MAX_PROVIDER_ERROR_BODY_CHARS,
};

/// 上游：`truncates the body at the cap`。
#[test]
fn truncates_the_body_at_the_cap() {
    let long = "a".repeat(MAX_PROVIDER_ERROR_BODY_CHARS + 1_000);
    let truncated = truncate_error_text(&long, MAX_PROVIDER_ERROR_BODY_CHARS);
    assert!(truncated.starts_with(&"a".repeat(MAX_PROVIDER_ERROR_BODY_CHARS)));
    assert!(truncated.ends_with("... [truncated 1000 chars]"));
}

#[test]
fn leaves_a_body_at_the_cap_unchanged() {
    let exact = "a".repeat(MAX_PROVIDER_ERROR_BODY_CHARS);
    assert_eq!(truncate_error_text(&exact, MAX_PROVIDER_ERROR_BODY_CHARS), exact);
}

/// 上游：`surfaces status and body without a prefix`。
#[test]
fn surfaces_status_and_body_without_a_prefix() {
    let error = http_error(403, Some(r#"{"error":"blocked by gateway WAF"}"#));
    assert_eq!(
        format_provider_error(&error, None),
        r#"403: {"error":"blocked by gateway WAF"}"#
    );
}

/// 上游：`applies a provider prefix with status and body`。
#[test]
fn applies_a_provider_prefix_with_status_and_body() {
    let error = http_error(500, Some("upstream exploded"));
    assert_eq!(
        format_provider_error(&error, Some("DeepSeek API error")),
        "DeepSeek API error (500): upstream exploded"
    );
}

/// 上游：`sets messageCarriesBody when the message already contains the extracted body`。
#[test]
fn marks_when_the_message_already_carries_the_body() {
    let error = plain_error("400: bad request");
    assert!(error.message_carries_body);
    assert_eq!(format_provider_error(&error, None), "400: bad request");
}

/// 上游：`treats an empty parsed body object as no body`。
#[test]
fn treats_an_empty_parsed_body_object_as_no_body() {
    let error = http_error(500, Some("{}"));
    assert_eq!(error.body, None);
    assert_eq!(format_provider_error(&error, None), "500 status code (no body)");
}

/// 上游：`appends the deepest cause of a connection error with no status or body`。
#[test]
fn appends_the_deepest_cause_of_a_connection_error() {
    let error = transport_error(
        "Connection error.",
        &["fetch failed".to_string(), "connect ECONNREFUSED 127.0.0.1:1".to_string()],
    );
    assert_eq!(
        format_provider_error(&error, None),
        "Connection error.: connect ECONNREFUSED 127.0.0.1:1"
    );
}

/// 上游：`leaves errors without a cause unchanged`。
#[test]
fn leaves_errors_without_a_cause_unchanged() {
    let error = transport_error("Connection error.", &[]);
    assert_eq!(format_provider_error(&error, None), "Connection error.");
}

#[test]
fn does_not_repeat_a_cause_that_is_already_in_the_message() {
    let error = transport_error(
        "failed to connect: denied",
        &["failed to connect: denied".to_string()],
    );
    assert_eq!(format_provider_error(&error, None), "failed to connect: denied");
}
