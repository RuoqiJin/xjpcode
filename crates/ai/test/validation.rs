//! 移植自上游 `packages/ai/test/validation.test.ts`。
//!
//! 上游有几条 schema 是 TypeBox 现搭的；这里写成 TypeBox 给出的那份 JSON（结构与
//! `coerce_typebox.rs` 的记录一致）。`Type.Union` 是 `anyOf`，`Type.Object` 的必填属性写进
//! `required`。
//!
//! Rust 侧没有的三样，逐条注明：`globalThis.Function` 被禁（Rust 的检查根本不用代码生成，
//! 这条性质本来就成立）、`structuredClone`（参数按值复制）、断言返回的不是同一个对象引用
//! （Rust 的值总是复制出来的，只比内容）。

use serde_json::{json, Map, Value};
use xjpcode_ai::types::{Tool, ToolCall};
use xjpcode_ai::{validate_tool_arguments, validate_tool_call};

fn object(value: Value) -> Map<String, Value> {
    value.as_object().expect("an object").clone()
}

fn tool(parameters: Value) -> Tool {
    Tool {
        name: "echo".to_string(),
        description: "Echo tool".to_string(),
        parameters,
        constrained_sampling: None,
    }
}

fn call(arguments: Value) -> ToolCall {
    ToolCall::new("tool-1", "echo", object(arguments))
}

fn validate(parameters: Value, arguments: Value) -> Result<Value, String> {
    validate_tool_arguments(&tool(parameters), &call(arguments))
}

/// 上游 `createToolCallWithPlainSchema`：schema 放在 `value` 属性上，且 `value` 必填。
fn plain(schema: Value) -> Value {
    json!({
        "type": "object",
        "properties": { "value": schema },
        "required": ["value"]
    })
}

#[test]
fn still_validates_when_code_generation_is_unavailable() {
    // 上游把 `globalThis.Function` 换成一个抛 `EvalError` 的实现，证明检查不靠 `new Function`。
    // Rust 侧没有代码生成这条路，性质本来就成立；这里锁住这次纠正的结果。
    let parameters = json!({
        "type": "object",
        "properties": { "count": { "type": "number" } },
        "required": ["count"]
    });
    assert_eq!(
        validate(parameters, json!({ "count": "42" })).expect("valid"),
        json!({ "count": 42 })
    );
}

#[test]
fn coerces_serialized_plain_json_schemas_like_typebox_schemas() {
    let passing_cases: Vec<(Value, Value, Value)> = vec![
        (json!({ "type": "number" }), json!("42"), json!(42)),
        (json!({ "type": "number" }), json!(true), json!(1)),
        (json!({ "type": "number" }), json!(null), json!(0)),
        (json!({ "type": "integer" }), json!("42"), json!(42)),
        (json!({ "type": "boolean" }), json!("true"), json!(true)),
        (json!({ "type": "boolean" }), json!("false"), json!(false)),
        (json!({ "type": "boolean" }), json!(1), json!(true)),
        (json!({ "type": "boolean" }), json!(0), json!(false)),
        // TypeBox 的转换规则先走：必填字符串上的 null 变成 "null"。
        (json!({ "type": "string" }), json!(null), json!("null")),
        (json!({ "type": "string" }), json!(true), json!("true")),
        (json!({ "type": "null" }), json!(""), json!(null)),
        (json!({ "type": "null" }), json!(0), json!(null)),
        (json!({ "type": "null" }), json!(false), json!(null)),
        (json!({ "type": ["number", "string"] }), json!("1"), json!("1")),
        (json!({ "type": ["boolean", "number"] }), json!("1"), json!(1)),
        (json!({ "type": "boolean" }), json!("1"), json!(true)),
        (json!({ "type": "boolean" }), json!("0"), json!(false)),
        (json!({ "type": "null" }), json!("null"), json!(null)),
        (json!({ "type": "integer" }), json!("42.1"), json!(42)),
        (
            json!({ "type": "array", "items": { "type": "number" } }),
            json!("7"),
            json!([7]),
        ),
    ];
    for (schema, input, expected) in passing_cases {
        assert_eq!(
            validate(plain(schema.clone()), json!({ "value": input.clone() }))
                .unwrap_or_else(|error| panic!("{schema} / {input}: {error}")),
            json!({ "value": expected })
        );
    }
}

#[test]
fn treats_null_as_omission_for_optional_non_nullable_properties() {
    // `Type.Object({ path: Type.String(), offset: Type.Optional(Type.Number()),
    //   nullable: Type.Optional(Type.Union([Type.String(), Type.Null()])),
    //   metadata: Type.Object({ enabled: Type.Optional(Type.Boolean()) }) })`
    let parameters = json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "offset": { "type": "number" },
            "nullable": { "anyOf": [{ "type": "string" }, { "type": "null" }] },
            "metadata": {
                "type": "object",
                "properties": { "enabled": { "type": "boolean" } }
            }
        },
        "required": ["path"]
    });
    assert_eq!(
        validate(
            parameters,
            json!({ "path": "file.txt", "offset": null, "nullable": null, "metadata": { "enabled": null } })
        )
        .expect("valid"),
        json!({ "path": "file.txt", "nullable": null, "metadata": {} })
    );
}

#[test]
fn preserves_optional_nulls_whose_referenced_schema_is_nullable() {
    let parameters = json!({
        "type": "object",
        "properties": { "value": { "$ref": "#/$defs/value" } },
        "$defs": { "value": { "anyOf": [{ "type": "number" }, { "type": "null" }] } }
    });
    assert_eq!(
        validate(parameters, json!({ "value": null })).expect("valid"),
        json!({ "value": null })
    );
}

#[test]
fn preserves_a_value_that_already_matches_a_nullable_union_arm() {
    // `Type.Object({ value: Type.Union([Type.Number(), Type.Null()]) })`
    let parameters = json!({
        "type": "object",
        "properties": { "value": { "anyOf": [{ "type": "number" }, { "type": "null" }] } },
        "required": ["value"]
    });
    assert_eq!(
        validate(parameters, json!({ "value": null })).expect("valid"),
        json!({ "value": null })
    );
}

#[test]
fn preserves_a_value_that_already_matches_a_oneof_nullable_union_arm() {
    let schema = json!({ "oneOf": [{ "type": "number" }, { "type": "null" }] });
    assert_eq!(
        validate(plain(schema), json!({ "value": null })).expect("valid"),
        json!({ "value": null })
    );
}

#[test]
fn still_coerces_nullable_unions_when_the_original_value_does_not_match_any_arm() {
    let schema = json!({ "anyOf": [{ "type": "number" }, { "type": "null" }] });
    assert_eq!(
        validate(plain(schema), json!({ "value": "42" })).expect("valid"),
        json!({ "value": 42 })
    );
}

#[test]
fn accepts_null_for_nullable_array_schemas_with_items() {
    let schema = json!({ "type": ["array", "null"], "items": { "type": "string" } });
    assert_eq!(
        validate(plain(schema), json!({ "value": null })).expect("valid"),
        json!({ "value": null })
    );
}

#[test]
fn rejects_invalid_coercions_for_serialized_plain_json_schemas() {
    let failing_cases: Vec<(Value, Value)> = vec![
        (json!({ "type": "boolean" }), json!("yes")),
        (json!({ "type": "number" }), json!("abc")),
        (json!({ "type": "null" }), json!("nil")),
        (json!({ "type": "integer" }), json!("4x")),
    ];
    for (schema, input) in failing_cases {
        let error = validate(plain(schema), json!({ "value": input }))
            .expect_err("must be rejected");
        assert!(
            error.starts_with("Validation failed for tool \"echo\":"),
            "unexpected message: {error}"
        );
    }
}

#[test]
fn lists_the_errors_then_the_arguments_it_received() {
    let error = validate(
        plain(json!({ "type": "number" })),
        json!({ "value": "many" }),
    )
    .expect_err("must be rejected");
    assert_eq!(
        error,
        "Validation failed for tool \"echo\":\n  - value: expected number, got string\n\nReceived arguments:\n{\n  \"value\": \"many\"\n}"
    );
}

#[test]
fn reports_a_missing_required_property_at_the_property() {
    let error = validate(plain(json!({ "type": "string" })), json!({}))
        .expect_err("must be rejected");
    assert_eq!(
        error,
        "Validation failed for tool \"echo\":\n  - value: missing\n\nReceived arguments:\n{}"
    );
}

#[test]
fn returns_a_copy_of_the_arguments_also_when_nothing_is_coerced() {
    let tool_call = call(json!({ "value": "x" }));
    let args = validate_tool_arguments(&tool(plain(json!({ "type": "string" }))), &tool_call)
        .expect("valid");
    assert_eq!(args, json!({ "value": "x" }));
    // 上游断言 `args !== toolCall.arguments`（引用不同）；Rust 的值本来就是复制出来的，只比值。
    assert_eq!(tool_call.arguments, object(json!({ "value": "x" })));
}

#[test]
fn reports_malformed_arguments_json_instead_of_validating_empty_arguments() {
    let mut tool_call = ToolCall::new("tool-1", "echo", Map::new());
    tool_call.arguments_error = Some("Unexpected token".to_string());
    let error = validate_tool_arguments(
        &tool(json!({
            "type": "object",
            "properties": { "value": { "type": "string" } }
        })),
        &tool_call,
    )
    .expect_err("must be rejected");
    assert_eq!(
        error,
        "Invalid arguments for tool \"echo\": Unexpected token"
    );
}

#[test]
fn finds_a_tool_by_name_or_says_it_is_missing() {
    let tools = vec![tool(plain(json!({ "type": "string" })))];
    assert_eq!(
        validate_tool_call(&tools, &call(json!({ "value": "x" }))).expect("valid"),
        json!({ "value": "x" })
    );
    let missing = ToolCall::new("tool-2", "other", Map::new());
    assert_eq!(
        validate_tool_call(&tools, &missing).expect_err("missing"),
        "Tool \"other\" not found"
    );
}

#[test]
fn hands_tools_the_arguments_pi_hands_them() {
    // 工具按自己的读法读参数：整数用 `as_i64` 读、键按顺序读。三处纠正因此都要在这里对一次。
    let parameters = json!({
        "type": "object",
        "properties": { "limit": { "type": "integer" } }
    });
    let args = validate(parameters, json!({ "limit": 10.0 })).expect("valid");
    assert_eq!(args["limit"].as_i64(), Some(10));

    // 可选属性上的 null 被丢掉以后，其余键的顺序与 pi 相同。
    let parameters = json!({
        "type": "object",
        "properties": {
            "a": { "type": "string" },
            "b": { "type": "number" },
            "c": { "type": "number" }
        }
    });
    let args = validate(parameters, json!({ "a": null, "b": 1, "c": 2 })).expect("valid");
    assert_eq!(args, json!({ "b": 1, "c": 2 }));
    let keys: Vec<&String> = args.as_object().expect("object").keys().collect();
    assert_eq!(keys, ["b", "c"]);

    // 绕回自己的格式不会带走进程：pi 在这里抛 RangeError（差异表 #56），我们给它一个结论。
    let parameters = json!({ "allOf": [{ "$ref": "#" }] });
    let error = validate(parameters, json!({})).expect_err("no value passes the cycle");
    assert!(
        error.starts_with("Validation failed for tool \"echo\":"),
        "unexpected message: {error}"
    );
}
