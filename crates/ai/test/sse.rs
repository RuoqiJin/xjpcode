//! `crates/ai/src/utils/sse.rs` 的测试。上游没有这个文件（OpenAI 路径由 SDK 切帧），
//! 规矩照 Anthropic 路径的 `iterateSseMessages`：多行 `data:`、`\r\n`/`\r`、注释、EOF flush、
//! UTF-8 跨块。

use xjpcode_ai::utils::sse::{SseDecoder, SseEvent};

fn event(data: &str) -> SseEvent {
    SseEvent {
        data: data.to_string(),
        ..SseEvent::default()
    }
}

#[test]
fn splits_events_on_blank_lines() {
    let mut decoder = SseDecoder::new();
    let events = decoder.push_bytes(b"data: one\n\ndata: two\n\n");
    assert_eq!(events, vec![event("one"), event("two")]);
}

#[test]
fn joins_multiple_data_lines_with_newlines() {
    let mut decoder = SseDecoder::new();
    let events = decoder.push_bytes(b"data: one\ndata: two\n\n");
    assert_eq!(events, vec![event("one\ntwo")]);
}

#[test]
fn accepts_crlf_and_lone_cr_line_endings() {
    let mut decoder = SseDecoder::new();
    assert_eq!(decoder.push_bytes(b"data: a\r\n\r\n"), vec![event("a")]);
    // 末尾那个 `\r` 可能是被劈开的 `\r\n`，所以先等着；EOF 时按空行收尾。
    assert!(decoder.push_bytes(b"data: b\r\r").is_empty());
    assert_eq!(decoder.finish(), vec![event("b")]);
}

#[test]
fn waits_for_the_second_byte_of_a_split_crlf() {
    let mut decoder = SseDecoder::new();
    // 一段在 `\r` 与 `\n` 之间断开：这一行要等下一段。
    assert!(decoder.push_bytes(b"data: a\r").is_empty());
    assert_eq!(decoder.push_bytes(b"\n\r\n"), vec![event("a")]);
}

#[test]
fn keeps_a_multibyte_character_split_across_chunks() {
    let mut decoder = SseDecoder::new();
    let line = "data: 你好\n\n".as_bytes().to_vec();
    // 劈在「你」的第二个字节之后。
    assert!(decoder.push_bytes(&line[..8]).is_empty());
    let events = decoder.push_bytes(&line[8..]);
    assert_eq!(events, vec![event("你好")]);
}

#[test]
fn ignores_comments_and_records_event_and_id() {
    let mut decoder = SseDecoder::new();
    let events = decoder.push_bytes(b": keep-alive\nevent: message\nid: 7\ndata: payload\n\n");
    assert_eq!(
        events,
        vec![SseEvent {
            event: Some("message".to_string()),
            id: Some("7".to_string()),
            data: "payload".to_string(),
        }]
    );
}

#[test]
fn only_one_optional_space_after_the_colon_is_stripped() {
    let mut decoder = SseDecoder::new();
    assert_eq!(decoder.push_bytes(b"data:  two\n\n"), vec![event(" two")]);
    assert_eq!(decoder.push_bytes(b"data:three\n\n"), vec![event("three")]);
}

#[test]
fn flushes_the_last_event_at_eof() {
    let mut decoder = SseDecoder::new();
    assert!(decoder.push_bytes(b"data: last").is_empty());
    assert_eq!(decoder.finish(), vec![event("last")]);
}

#[test]
fn an_eof_without_data_flushes_nothing() {
    let mut decoder = SseDecoder::new();
    assert!(decoder.push_bytes(b": comment\n").is_empty());
    assert!(decoder.finish().is_empty());
}

#[test]
fn data_may_be_empty_and_still_counts_as_an_event() {
    let mut decoder = SseDecoder::new();
    assert_eq!(decoder.push_bytes(b"data:\n\n"), vec![event("")]);
}
