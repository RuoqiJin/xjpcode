//! 对应上游 `test/constrained-sampling.test.ts` 里与 OpenAI 兼容路径有关的部分：
//! `makeStrictJsonSchema` 与 `resolveJsonSchemaStrictSampling`。
//!
//! 上游的 grammar 工具与 Responses 那条链没有搬（差异 #110）。

use serde_json::{json, Value};
use xjpcode_ai::api::constrained_sampling::{
    make_strict_json_schema, resolve_json_schema_strict_sampling,
};
use xjpcode_ai::types::ConstrainedSamplingStrict;
use xjpcode_ai::{
    ConstrainedSampling, ConstrainedSamplingConfig, JsonValue, Tool,
};

fn tool(parameters: JsonValue, strict: ConstrainedSamplingStrict) -> Tool {
    Tool {
        name: "sample_tool".to_string(),
        description: "Sample tool".to_string(),
        parameters,
        constrained_sampling: Some(ConstrainedSampling::Config(
            ConstrainedSamplingConfig::JsonSchema { strict },
        )),
    }
}

#[test]
fn derives_strict_provider_schemas_without_changing_tool_definitions() {
    let parameters = json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "offset": { "type": "number" },
            "metadata": {
                "type": "object",
                "properties": { "enabled": { "type": "boolean" } }
            },
            "nullable": { "anyOf": [{ "type": "string" }, { "type": "null" }] }
        },
        "required": ["path", "metadata"]
    });

    let strict = make_strict_json_schema(&parameters).unwrap();

    assert!(parameters.get("additionalProperties").is_none());
    assert_eq!(parameters["required"], json!(["path", "metadata"]));
    assert_eq!(strict["additionalProperties"], false);
    assert_eq!(
        strict["required"],
        json!(["path", "offset", "metadata", "nullable"])
    );
    assert_eq!(
        strict["properties"]["offset"],
        json!({"anyOf": [{"type": "number"}, {"type": "null"}]})
    );
    assert_eq!(strict["properties"]["metadata"]["additionalProperties"], false);
    assert_eq!(strict["properties"]["metadata"]["required"], json!(["enabled"]));
    assert_eq!(
        strict["properties"]["metadata"]["properties"]["enabled"],
        json!({"anyOf": [{"type": "boolean"}, {"type": "null"}]})
    );
    assert_eq!(
        strict["properties"]["nullable"],
        json!({"anyOf": [{"type": "string"}, {"type": "null"}]})
    );
}

#[test]
fn keeps_strict_mode_for_enum_only_schemas() {
    let parameters = json!({
        "type": "object",
        "properties": { "level": { "enum": ["low", "high"] } },
        "required": ["level"]
    });
    let strict = make_strict_json_schema(&parameters).unwrap();
    assert_eq!(strict["properties"]["level"], json!({"enum": ["low", "high"]}));
    assert_eq!(
        resolve_json_schema_strict_sampling(
            &tool(parameters, ConstrainedSamplingStrict::Prefer),
            true
        )
        .unwrap(),
        Some(true)
    );
}

#[test]
fn falls_back_or_rejects_schemas_that_cannot_be_safely_converted() {
    let cases: Vec<(Value, &str)> = vec![
        (
            json!({
                "type": "object",
                "properties": {
                    "metadata": { "type": "object", "additionalProperties": { "type": "string" } }
                }
            }),
            "additionalProperties is unsupported",
        ),
        (
            json!({ "allOf": [{ "type": "object" }] }),
            "allOf schemas are unsupported",
        ),
        (
            json!({
                "type": "object",
                "properties": {
                    "value": {
                        "anyOf": [
                            { "type": "object", "properties": { "nested": { "type": "string" } } },
                            { "type": "null" }
                        ]
                    }
                }
            }),
            "object and array unions are unsupported",
        ),
        (
            json!({
                "type": "object",
                "properties": { "child": { "$ref": "https://example.com/child.json" } },
                "required": ["child"]
            }),
            "$ref schemas are unsupported",
        ),
        (
            json!({ "type": "object", "properties": { "value": { "description": "Any JSON value" } } }),
            "schemas without a type are unsupported",
        ),
        (
            json!({ "type": "object", "properties": { "mode": { "const": "fast" } } }),
            "schemas without a type are unsupported",
        ),
    ];

    for (parameters, error) in cases {
        let prefer = tool(parameters.clone(), ConstrainedSamplingStrict::Prefer);
        assert!(
            make_strict_json_schema(&parameters).unwrap_err().contains(error),
            "{error}"
        );
        assert_eq!(resolve_json_schema_strict_sampling(&prefer, true).unwrap(), None);

        let require = tool(parameters, ConstrainedSamplingStrict::Require);
        let message = resolve_json_schema_strict_sampling(&require, true).unwrap_err();
        assert!(message.contains(error), "{message}");
        assert!(message.starts_with("Tool \"sample_tool\" requires JSON-schema"));
    }
}

#[test]
fn rejects_require_when_the_provider_has_no_strict_mode() {
    let require = tool(
        json!({ "type": "object", "properties": {} }),
        ConstrainedSamplingStrict::Require,
    );
    assert_eq!(
        resolve_json_schema_strict_sampling(&require, false).unwrap_err(),
        "Tool \"sample_tool\" requires JSON-schema constrained sampling, but strict tools are unsupported."
    );
    let prefer = tool(
        json!({ "type": "object", "properties": {} }),
        ConstrainedSamplingStrict::Prefer,
    );
    assert_eq!(
        resolve_json_schema_strict_sampling(&prefer, false).unwrap(),
        None
    );
}

#[test]
fn tools_without_constrained_sampling_are_left_alone() {
    let plain = Tool {
        name: "plain".to_string(),
        description: "Plain".to_string(),
        parameters: json!({ "type": "object", "properties": {} }),
        constrained_sampling: None,
    };
    assert_eq!(
        resolve_json_schema_strict_sampling(&plain, true).unwrap(),
        None
    );
}
