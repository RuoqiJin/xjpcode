//! 移植自上游 `packages/ai/test/message-types.test.ts`。
//!
//! 上游第二个用例（`rejects non-JSON detail types`）全部是 `expectTypeOf` 的编译期断言，
//! Rust 没有等价物，见 `PORTING.md` 差异表。
//!
//! 另加一个用例：用 pi 测试里现成的消息字面量验证字段名与字段值——读进类型再写出来，
//! 字段名必须逐字相同，数值必须相等（统一按 f64 归一后比较，`0` 与 `0.0` 视为同一个数）。

use serde::{Deserialize, Serialize};
use xjpcode_ai::types::{
    ConstrainedSampling, ConstrainedSamplingOff, Context, JsonValue, Message,
    ToolResultContentBlock, ToolResultMessage, ToolResultRole,
};

// ---------------------------------------------------------------------------
// 上游用例：JSON 兼容的 details 类型被原样保留
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Summary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lines: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Details {
    path: String,
    items: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<Summary>,
}

fn tool_result_message<TDetails>(details: TDetails) -> ToolResultMessage<TDetails> {
    ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "call-1".into(),
        tool_name: "read".into(),
        content: vec![],
        details: Some(details),
        usage: None,
        nested_calls: None,
        is_error: false,
        timestamp: 1,
        duration_ms: None,
    }
}

/// 走一遍 JSON，等价于上游把 `ToolResultMessage<Details>` 赋给 `Message`。
fn into_message<T: Serialize>(message: &T) -> Message {
    let value = serde_json::to_value(message).expect("encode message");
    serde_json::from_value(value).expect("decode message")
}

#[test]
fn json_compatible_details_are_preserved() {
    let object_message = tool_result_message(Details {
        path: "a.ts".into(),
        items: vec!["first".into()],
        summary: Some(Summary { lines: Some(3) }),
    });
    let array_message = tool_result_message(vec!["a".to_string(), "b".to_string()]);
    let primitive_message = tool_result_message("diagnostic".to_string());
    let null_message = tool_result_message(Option::<JsonValue>::None);

    assert_eq!(
        object_message
            .details
            .as_ref()
            .and_then(|d| d.summary.as_ref())
            .and_then(|s| s.lines),
        Some(3)
    );
    assert_eq!(
        array_message.details.as_deref(),
        Some(&["a".to_string(), "b".to_string()][..])
    );
    assert_eq!(primitive_message.details.as_deref(), Some("diagnostic"));
    assert_eq!(null_message.details, Some(Option::<JsonValue>::None));

    // 上游用 `expectTypeOf<ToolResultMessage["details"]>().toEqualTypeOf<JsonValue | undefined>()`
    // 断言默认参数下 details 就是 JSON；这里用实际编码验证同一件事。
    let Message::ToolResult(transcript_message) = into_message(&object_message) else {
        panic!("object message must decode as a tool result");
    };
    assert_eq!(
        transcript_message.details,
        Some(serde_json::json!({ "path": "a.ts", "items": ["first"], "summary": { "lines": 3 } }))
    );

    let context: Context = serde_json::from_value(serde_json::json!({
        "messages": [
            serde_json::to_value(&object_message).unwrap(),
            serde_json::to_value(&array_message).unwrap(),
        ],
    }))
    .expect("context decodes");
    assert_eq!(context.messages.len(), 2);
    assert!(context.messages.iter().all(|m| m.role() == "toolResult"));
}

// ---------------------------------------------------------------------------
// 字段名验证：pi 测试里现成的消息样例，读进类型再写出来
// ---------------------------------------------------------------------------

/// 每条样例都注明出处；除 `model`/`timestamp` 换成固定字面量外，字段与字面量
/// 与上游测试完全一致。
const PI_MESSAGE_SAMPLES: &[(&str, &str)] = &[
    // packages/ai/test/system-message-replay.test.ts: transcript[0]
    (
        "system + sections + toolsAdded",
        r#"{"role":"system","content":"base","sections":{"a":"<a>1</a>","b":"<b>1</b>"},"toolsAdded":[{"name":"first","description":"first tool","parameters":{}}],"timestamp":10}"#,
    ),
    // packages/ai/test/system-message-replay.test.ts: transcript[1]
    (
        "user string content",
        r#"{"role":"user","content":"hello","timestamp":11}"#,
    ),
    // packages/ai/test/system-message-replay.test.ts: transcript[2]
    (
        "system plain content",
        r#"{"role":"system","content":"also do this","timestamp":12}"#,
    ),
    // packages/agent/test/agent.test.ts: createAssistantMessage
    (
        "assistant text content",
        r#"{"role":"assistant","content":[{"type":"text","text":"ok"}],"api":"openai-responses","provider":"openai","model":"mock","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":13}"#,
    ),
    // packages/ai/test/system-message-replay.test.ts: transcript[4]
    (
        "system sections null + toolsRemoved",
        r#"{"role":"system","content":"","sections":{"a":"<a>2</a>","b":null,"c":"<c>1</c>"},"toolsRemoved":[{"name":"first"}],"toolsAdded":[{"name":"second","description":"second tool","parameters":{}}],"timestamp":14}"#,
    ),
    // packages/ai/test/message-types.test.ts: `base`
    (
        "toolResult empty content",
        r#"{"role":"toolResult","toolCallId":"call-1","toolName":"read","content":[],"isError":false,"timestamp":1}"#,
    ),
    // packages/ai/test/message-types.test.ts: objectMessage
    (
        "toolResult with JSON details",
        r#"{"role":"toolResult","toolCallId":"call-1","toolName":"read","content":[],"details":{"path":"a.ts","items":["first"],"summary":{"lines":3}},"isError":false,"timestamp":1}"#,
    ),
    // packages/agent/test/agent-loop.test.ts: first tool call
    (
        "assistant toolCall content",
        r#"{"role":"assistant","content":[{"type":"toolCall","id":"tool-1","name":"echo","arguments":{"value":"hello"}}],"api":"openai-responses","provider":"openai","model":"mock","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"toolUse","timestamp":15}"#,
    ),
    // packages/coding-agent/test/compaction-nested-calls.test.ts: `result`
    (
        "toolResult with nestedCalls",
        r#"{"role":"toolResult","toolCallId":"codemode-1","toolName":"codemode","content":[],"isError":false,"timestamp":0,"nestedCalls":{"calls":[{"id":"codemode-1/1","name":"read","arguments":{"path":"a.ts"},"status":"ok"},{"id":"codemode-1/2","name":"edit","arguments":{"path":"b.ts","edits":[]},"status":"ok"},{"id":"codemode-1/3","name":"write","argumentsBytes":40000,"status":"ok"}],"complete":false}}"#,
    ),
    // packages/ai/test/azure-openai-completions.test.ts: `assistant`
    (
        "assistant thinking + text content",
        r#"{"role":"assistant","content":[{"type":"thinking","thinking":"internal reasoning","thinkingSignature":"reasoning_content"},{"type":"text","text":"answer"}],"api":"openai-completions","provider":"azure","model":"deepseek-v4-pro","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":16}"#,
    ),
    // packages/ai/test/bedrock-convert-messages.test.ts: toolResult for call-1
    (
        "toolResult text + image content",
        r#"{"role":"toolResult","toolCallId":"call-1","toolName":"codemode","content":[{"type":"text","text":"Script failed"},{"type":"image","data":"AAAA","mimeType":"image/png"},{"type":"text","text":"Script error"}],"isError":true,"timestamp":1}"#,
    ),
];

/// 把 JSON 里所有数值归一成 `f64`。
///
/// pi 的 `number` 是 IEEE-754 double，`JSON.stringify(0)` 写成 `0`；Rust 的 `f64`
/// 序列化 `0.0` 写成 `0.0`。两者是同一个数，字段名与取值都相同，因此比较时归一。
fn normalize_numbers(value: &JsonValue) -> JsonValue {
    match value {
        JsonValue::Number(number) => {
            let float = number.as_f64().expect("sample numbers are finite");
            JsonValue::Number(serde_json::Number::from_f64(float).expect("finite"))
        }
        JsonValue::Array(items) => JsonValue::Array(items.iter().map(normalize_numbers).collect()),
        JsonValue::Object(fields) => JsonValue::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), normalize_numbers(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[test]
fn pi_message_samples_round_trip() {
    for (label, json) in PI_MESSAGE_SAMPLES {
        let original: JsonValue = serde_json::from_str(json)
            .unwrap_or_else(|error| panic!("{label}: sample is not JSON: {error}"));

        let message: Message = serde_json::from_value(original.clone())
            .unwrap_or_else(|error| panic!("{label}: decode failed: {error}"));

        let expected_role = original["role"].as_str().expect("sample has a role");
        assert_eq!(message.role(), expected_role, "{label}: wrong variant");

        let written = serde_json::to_value(&message)
            .unwrap_or_else(|error| panic!("{label}: encode failed: {error}"));
        assert_eq!(
            normalize_numbers(&written),
            normalize_numbers(&original),
            "{label}: field names, optionality or values changed"
        );
    }
}

// ---------------------------------------------------------------------------
// 内容块的判别字段
// ---------------------------------------------------------------------------

#[test]
fn image_blocks_keep_their_mime_type_field() {
    let Message::ToolResult(message) = serde_json::from_str::<Message>(
        r#"{"role":"toolResult","toolCallId":"c","toolName":"t","content":[{"type":"image","data":"AAAA","mimeType":"image/png"}],"isError":false,"timestamp":1}"#,
    )
    .expect("decodes") else {
        panic!("expected a tool result");
    };
    let ToolResultContentBlock::Image(image) = &message.content[0] else {
        panic!("expected an image block");
    };
    assert_eq!(image.mime_type, "image/png");
    assert_eq!(image.data, "AAAA");
}

// ---------------------------------------------------------------------------
// constrainedSampling：`false` 与"没写"必须区分
// ---------------------------------------------------------------------------

#[test]
fn constrained_sampling_keeps_the_explicit_false() {
    let message: Message = serde_json::from_value(serde_json::json!({
        "role": "system",
        "content": "",
        "toolsAdded": [
            { "name": "off", "description": "explicitly off", "parameters": {}, "constrainedSampling": false },
            { "name": "default", "description": "not declared", "parameters": {} },
            {
                "name": "grammar",
                "description": "constrained",
                "parameters": {},
                "constrainedSampling": { "type": "grammar", "variants": { "openai_lark": "start: /a/" } }
            }
        ],
        "timestamp": 1
    }))
    .expect("decodes");

    let Message::System(system) = message else {
        panic!("expected a system message");
    };
    let tools = system.tools_added.clone().expect("tools");
    assert_eq!(
        tools[0].constrained_sampling,
        Some(ConstrainedSampling::Off(ConstrainedSamplingOff))
    );
    assert_eq!(tools[1].constrained_sampling, None);
    assert!(matches!(
        tools[2].constrained_sampling,
        Some(ConstrainedSampling::Config(_))
    ));

    // 三种状态在 JSON 上同样要区分：显式 false 必须写回去，缺席必须保持缺席。
    let written = serde_json::to_value(Message::System(system)).expect("encodes");
    assert_eq!(
        written["toolsAdded"][0]["constrainedSampling"],
        serde_json::json!(false)
    );
    assert!(written["toolsAdded"][1]
        .get("constrainedSampling")
        .is_none());
}

#[test]
fn constrained_sampling_rejects_true() {
    let decoded = serde_json::from_value::<Message>(serde_json::json!({
        "role": "system",
        "content": "",
        "toolsAdded": [
            { "name": "bad", "description": "d", "parameters": {}, "constrainedSampling": true }
        ],
        "timestamp": 1
    }));
    assert!(decoded.is_err(), "`true` is not part of pi's type");
}

// ---------------------------------------------------------------------------
// 数值的最后一位
// ---------------------------------------------------------------------------

/// 上游没有这一条：它守的是“值就是 JS 的 double”这件事在 Rust 侧的样子。
///
/// `JSON.parse` 给的是最近的那个 double，`JSON.stringify` 写回它的最短往返写法；值会写进会话文件，
/// 最后一位不能动。`serde_json` 默认的浮点解析是快速近似，会差一位（见 `PORTING.md` 差异表）。
#[test]
fn keeps_every_digit_of_a_number() {
    let sample = r#"{"role":"toolResult","toolCallId":"c","toolName":"t","content":[],"details":{"slow":0.013009999999999999,"far":123456789012345680000},"isError":false,"timestamp":1}"#;
    let message: Message = serde_json::from_str(sample).expect("a message");

    // 读完再写出去，最后一位不能动。
    let written = serde_json::to_string(&message).expect("json");
    assert!(
        written.contains(r#""slow":0.013009999999999999"#),
        "0.013009999999999999 was rewritten: {written}"
    );

    // 1.2345678901234568e20 的值相同，写法与 `JSON.stringify` 不同（差异表 #22 登记的代价）。
    let details = serde_json::to_value(&message).expect("json")["details"].clone();
    assert_eq!(details["slow"].as_f64(), Some(0.013009999999999999));
    assert_eq!(details["far"].as_f64(), Some(1.2345678901234568e20));
}
