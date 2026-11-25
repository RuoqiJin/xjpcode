//! 移植自上游 `packages/ai/test/system-message-replay.test.ts`。
//!
//! 固定用例直接抄自上游字面量（经 JSON 解码，顺带验证线上形状）；断言用序列化后的
//! JSON 比对，对应上游的 `toEqual({...})`。

use serde_json::{json, Value};

use xjpcode_ai::types::{
    ConstrainedSampling, ConstrainedSamplingOff, Context, Message, TranscriptContext,
};
use xjpcode_ai::utils::text::{get_system_message_text, render_system_message_update};
use xjpcode_ai::utils::transcript::{
    collapse_system_messages, declarations_equal, get_current_system_message,
    get_current_system_prompt, get_tool_state_changes, has_non_additive_tool_changes,
    has_tool_redefinitions, normalize_context,
};

fn message(value: Value) -> Message {
    serde_json::from_value(value).expect("fixture decodes")
}

fn tool_json(name: &str) -> Value {
    json!({ "name": name, "description": format!("{name} tool"), "parameters": {} })
}

fn transcript() -> TranscriptContext {
    normalize_context(Context {
        messages: vec![
            message(json!({
                "role": "system",
                "content": "base",
                "sections": { "a": "<a>1</a>", "b": "<b>1</b>" },
                "toolsAdded": [tool_json("first")],
                "timestamp": 10
            })),
            message(json!({ "role": "user", "content": "hello", "timestamp": 11 })),
            message(json!({ "role": "system", "content": "also do this", "timestamp": 12 })),
            message(json!({
                "role": "assistant",
                "content": [{ "type": "text", "text": "ok" }],
                "api": "openai-responses",
                "provider": "openai",
                "model": "m",
                "usage": {
                    "input": 0,
                    "output": 0,
                    "cacheRead": 0,
                    "cacheWrite": 0,
                    "totalTokens": 0,
                    "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
                },
                "stopReason": "stop",
                "timestamp": 13
            })),
            message(json!({
                "role": "system",
                "content": "",
                "sections": { "a": "<a>2</a>", "b": null, "c": "<c>1</c>" },
                "toolsRemoved": [{ "name": "first" }],
                "toolsAdded": [tool_json("second")],
                "timestamp": 14
            })),
        ],
        ..Context::default()
    })
}

#[test]
fn replays_content_sections_and_tools_into_one_leading_message() {
    let transcript = transcript();
    let current = get_current_system_message(&transcript.messages).expect("replayed");

    assert_eq!(
        serde_json::to_value(&current).expect("encodes"),
        json!({
            "role": "system",
            "content": "base\n\nalso do this",
            "sections": { "a": "<a>2</a>", "c": "<c>1</c>" },
            "toolsAdded": [tool_json("second")],
            "timestamp": 10
        })
    );
    assert_eq!(
        get_current_system_prompt(&transcript.messages),
        "base\n\nalso do this\n\n<a>2</a>\n\n<c>1</c>"
    );
}

#[test]
fn collapse_keeps_only_non_system_messages_after_the_replayed_head() {
    let transcript = transcript();
    let collapsed = collapse_system_messages(&transcript);

    let roles: Vec<&str> = collapsed
        .messages
        .iter()
        .map(|message| message.role())
        .collect();
    assert_eq!(roles, vec!["system", "user", "assistant"]);
    assert_eq!(collapse_system_messages(&collapsed), collapsed);
}

#[test]
fn replay_of_a_transcript_without_system_messages_is_empty() {
    let context = normalize_context(Context {
        messages: vec![message(
            json!({ "role": "user", "content": "hi", "timestamp": 1 }),
        )],
        ..Context::default()
    });

    assert!(get_current_system_message(&context.messages).is_none());
    assert_eq!(get_current_system_prompt(&context.messages), "");
    assert_eq!(
        collapse_system_messages(&context).messages,
        context.messages
    );
}

#[test]
fn a_late_full_patch_on_a_transcript_without_a_leading_message_replays_as_the_prompt() {
    let context = normalize_context(Context {
        messages: vec![
            message(json!({ "role": "user", "content": "old session", "timestamp": 1 })),
            message(json!({
                "role": "system",
                "content": "",
                "sections": { "preamble": "You are pi." },
                "toolsAdded": [tool_json("x")],
                "timestamp": 2
            })),
        ],
        ..Context::default()
    });

    assert_eq!(get_current_system_prompt(&context.messages), "You are pi.");
    let collapsed = collapse_system_messages(&context);
    assert_eq!(collapsed.messages[0].role(), "system");
    assert_eq!(
        serde_json::to_value(&collapsed.messages[0]).expect("encodes")["toolsAdded"],
        json!([tool_json("x")])
    );
}

#[test]
fn renders_complete_prompts_and_framed_updates() {
    let transcript = transcript();
    let leading = match &transcript.messages[0] {
        Message::System(message) => message,
        other => panic!("expected a system message, got {}", other.role()),
    };
    let update = match &transcript.messages[4] {
        Message::System(message) => message,
        other => panic!("expected a system message, got {}", other.role()),
    };

    assert_eq!(
        get_system_message_text(leading),
        "base\n\n<a>1</a>\n\n<b>1</b>"
    );
    assert_eq!(
        render_system_message_update(update),
        [
            "Updated system prompt section \"a\":\n\n<a>2</a>",
            "Removed system prompt section \"b\".",
            "Updated system prompt section \"c\":\n\n<c>1</c>",
        ]
        .join("\n\n")
    );
}

#[test]
fn normalizes_the_legacy_prompt_and_tool_fields_into_a_leading_system_message() {
    let messages = vec![message(
        json!({ "role": "user", "content": "hi", "timestamp": 1 }),
    )];

    let untouched = normalize_context(Context {
        messages: messages.clone(),
        ..Context::default()
    });
    assert_eq!(untouched.messages, messages);

    let empty_fields = normalize_context(Context {
        system_prompt: Some(String::new()),
        tools: Some(Vec::new()),
        messages: messages.clone(),
    });
    assert_eq!(empty_fields.messages, messages);

    let with_prompt = normalize_context(Context {
        system_prompt: Some("be brief".into()),
        tools: Some(vec![xjpcode_ai::types::Tool {
            name: "a".into(),
            description: "a tool".into(),
            parameters: json!({}),
            constrained_sampling: None,
        }]),
        messages: messages.clone(),
    });
    assert_eq!(
        with_prompt.messages[0],
        message(json!({
            "role": "system",
            "content": "be brief",
            "toolsAdded": [tool_json("a")],
            "timestamp": 0
        }))
    );
    assert_eq!(with_prompt.messages.len(), messages.len() + 1);
}

#[test]
fn compares_tool_declarations_without_executable_or_undefined_fields() {
    let plain = message(json!({
        "role": "system",
        "content": "",
        "toolsAdded": [{ "name": "a", "description": "a tool", "parameters": {} }],
        "timestamp": 0
    }));
    let Message::System(system) = plain else {
        panic!("expected a system message");
    };
    let base = system.tools_added.expect("tools")[0].clone();

    // 上游比对的是一个带了 `execute` 和 `constrainedSampling: undefined` 的对象；
    // Rust 的 `Tool` 没有可执行字段，等价物是 `constrained_sampling: None`。
    let mut executable = base.clone();
    executable.constrained_sampling = None;
    assert!(declarations_equal(&executable, &base));

    let mut changed = base.clone();
    changed.description = "changed".into();
    assert!(!declarations_equal(&base, &changed));

    let mut constrained = base.clone();
    constrained.constrained_sampling = Some(ConstrainedSampling::Off(ConstrainedSamplingOff));
    assert!(!declarations_equal(&base, &constrained));
}

#[test]
fn tool_state_changes_treat_changed_definitions_as_removal_plus_addition() {
    let Message::System(system) = message(json!({
        "role": "system",
        "content": "",
        "toolsAdded": [
            { "name": "a", "description": "a tool", "parameters": {} },
            { "name": "b", "description": "b tool", "parameters": {} },
            { "name": "c", "description": "c tool", "parameters": {} }
        ],
        "timestamp": 0
    })) else {
        panic!("expected a system message");
    };
    let tools = system.tools_added.expect("tools");
    let (a, b, c) = (tools[0].clone(), tools[1].clone(), tools[2].clone());

    let mut changed_b = b.clone();
    changed_b.description = "changed".into();

    let changes = get_tool_state_changes(&[a.clone(), b.clone()], &[changed_b, c]);
    assert_eq!(changes.tools_added.len(), 2);
    assert_eq!(changes.tools_removed.len(), 2);
    assert_eq!(
        serde_json::to_value(&changes.tools_removed).expect("encodes"),
        json!([{ "name": "a" }, { "name": "b" }])
    );

    let unchanged = get_tool_state_changes(&[a.clone()], &[a]);
    assert!(unchanged.tools_added.is_empty());
    assert!(unchanged.tools_removed.is_empty());
}

#[test]
fn detects_non_additive_tool_history_and_redefinitions() {
    let transcript = transcript();
    assert!(has_non_additive_tool_changes(&transcript.messages));
    assert!(!has_tool_redefinitions(&transcript.messages));

    let additive = normalize_context(Context {
        messages: vec![
            message(
                json!({ "role": "system", "content": "", "toolsAdded": [tool_json("a")], "timestamp": 1 }),
            ),
            message(
                json!({ "role": "system", "content": "", "toolsAdded": [tool_json("b")], "timestamp": 2 }),
            ),
        ],
        ..Context::default()
    });
    assert!(!has_non_additive_tool_changes(&additive.messages));

    let redeclared = normalize_context(Context {
        messages: vec![
            message(
                json!({ "role": "system", "content": "", "toolsAdded": [tool_json("a")], "timestamp": 1 }),
            ),
            message(json!({
                "role": "system",
                "content": "",
                "toolsAdded": [{ "name": "a", "description": "changed", "parameters": {} }],
                "timestamp": 2
            })),
        ],
        ..Context::default()
    });
    assert!(has_non_additive_tool_changes(&redeclared.messages));
    assert!(has_tool_redefinitions(&redeclared.messages));
}

#[test]
fn renders_sections_in_declaration_order() {
    // 上游测试没覆盖这一点（它的固定用例里片段名恰好按字母序），但 `Object.entries` 是
    // 插入顺序，而插入顺序会进入渲染出来的提示词。这条锁住 `IndexMap` + `preserve_order`
    // 的选择：排序实现会输出 "<a>1</a>\n\n<z>1</z>"。
    let context = normalize_context(Context {
        messages: vec![message(json!({
            "role": "system",
            "content": "",
            "sections": { "zulu": "<z>1</z>", "alpha": "<a>1</a>" },
            "timestamp": 1
        }))],
        ..Context::default()
    });

    let current = get_current_system_message(&context.messages).expect("replayed");
    assert_eq!(get_system_message_text(&current), "<z>1</z>\n\n<a>1</a>");
}
