//! 对应上游 `api/constrained-sampling.ts` 里 OpenAI 兼容路径用到的两样：把工具 schema 改写成
//! provider 严格模式接受的子集，以及决定一个工具要不要按严格模式发。
//!
//! 上游的 grammar 工具（`resolveGrammarConstrainedSampling` 一族）没有搬（DeepSeek 不用，差异 #110）。

use serde_json::{json, Value};

use crate::types::{ConstrainedSampling, ConstrainedSamplingConfig, ConstrainedSamplingStrict, Tool};

/// 上游 `UNSUPPORTED_STRICT_SCHEMA_KEYS`
const UNSUPPORTED_STRICT_SCHEMA_KEYS: &[&str] = &[
    "$ref",
    "$defs",
    "definitions",
    "allOf",
    "oneOf",
    "patternProperties",
    "dependentSchemas",
    "dependencies",
    "unevaluatedProperties",
    "propertyNames",
    "contains",
    "prefixItems",
    "not",
    "if",
    "then",
    "else",
];

fn is_json_schema_object(value: &Value) -> bool {
    value.is_object()
}

fn schema_types(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(kind)) => vec![kind.as_str()],
        Some(Value::Array(kinds)) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn is_structured_schema(schema: &Value) -> bool {
    if !is_json_schema_object(schema) {
        return false;
    }
    let types = schema_types(schema);
    types.contains(&"object")
        || types.contains(&"array")
        || schema.get("properties").is_some()
        || schema.get("items").is_some()
}

fn schema_allows_null(schema: &Value) -> bool {
    if !is_json_schema_object(schema) {
        return false;
    }
    if schema.get("type").is_some_and(|kind| {
        kind == "null"
            || kind
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind == "null"))
    }) {
        return true;
    }
    if schema.get("const") == Some(&Value::Null) {
        return true;
    }
    if schema
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|values| values.contains(&Value::Null))
    {
        return true;
    }
    schema
        .get("anyOf")
        .and_then(Value::as_array)
        .is_some_and(|variants| variants.iter().any(schema_allows_null))
}

fn make_json_schema_node_strict(schema: &mut Value) -> Result<(), String> {
    let Some(object) = schema.as_object_mut() else {
        return Err("boolean schemas are unsupported".to_string());
    };
    for key in UNSUPPORTED_STRICT_SCHEMA_KEYS {
        if object.contains_key(*key) {
            return Err(format!("{key} schemas are unsupported"));
        }
    }
    // OpenAI 严格模式不接收没有 type 的节点；只有 enum 的算有类型，只有 const 的不算。
    if !object.contains_key("type")
        && !object.contains_key("anyOf")
        && !object.contains_key("enum")
    {
        return Err("schemas without a type are unsupported".to_string());
    }

    if let Some(any_of) = object.get_mut("anyOf") {
        let Some(variants) = any_of.as_array_mut() else {
            return Err("anyOf must contain at least one schema".to_string());
        };
        if variants.is_empty() {
            return Err("anyOf must contain at least one schema".to_string());
        }
        for variant in variants {
            if is_structured_schema(variant) {
                return Err("object and array unions are unsupported".to_string());
            }
            make_json_schema_node_strict(variant)?;
        }
    }

    if let Some(items) = object.get_mut("items") {
        if items.is_array() {
            return Err("tuple schemas are unsupported".to_string());
        }
        make_json_schema_node_strict(items)?;
    }

    let is_object_schema = object.get("type") == Some(&Value::String("object".to_string()));
    if object.contains_key("properties") && !is_object_schema {
        return Err("properties require type object".to_string());
    }
    if !is_object_schema {
        return Ok(());
    }
    if let Some(additional) = object.get("additionalProperties") {
        if additional != &Value::Bool(false) {
            return Err("schema-valued or true additionalProperties is unsupported".to_string());
        }
    }
    if object.get("properties").is_some_and(|value| !value.is_object()) {
        return Err("object properties must be a schema map".to_string());
    }
    if let Some(required) = object.get("required") {
        let valid = required
            .as_array()
            .is_some_and(|keys| keys.iter().all(Value::is_string));
        if !valid {
            return Err("object required must be a string array".to_string());
        }
    }

    let property_names: Vec<String> = object
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default();
    let required: Vec<String> = object
        .get("required")
        .and_then(Value::as_array)
        .map(|keys| {
            keys.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default();
    if required
        .iter()
        .any(|key| !property_names.iter().any(|name| name == key))
    {
        return Err("required contains an unknown property".to_string());
    }

    if let Some(Value::Object(properties)) = object.get_mut("properties") {
        for (key, property) in properties.iter_mut() {
            make_json_schema_node_strict(property)?;
            if !required.iter().any(|required| required == key) && !schema_allows_null(property) {
                *property = json!({ "anyOf": [property.clone(), { "type": "null" }] });
            }
        }
    }
    object.insert(
        "required".to_string(),
        Value::Array(property_names.into_iter().map(Value::String).collect()),
    );
    object.insert("additionalProperties".to_string(), Value::Bool(false));
    Ok(())
}

/// pi: `makeStrictJsonSchema`
pub fn make_strict_json_schema(schema: &Value) -> Result<Value, String> {
    let mut cloned = schema.clone();
    if !is_json_schema_object(&cloned) {
        return Err("root schema must have type object".to_string());
    }
    make_json_schema_node_strict(&mut cloned)?;
    if cloned.get("type") != Some(&Value::String("object".to_string())) {
        return Err("root schema must have type object".to_string());
    }
    Ok(cloned)
}

/// pi: `resolveJsonSchemaStrictSampling`
pub fn resolve_json_schema_strict_sampling(
    tool: &Tool,
    supports_strict_mode: bool,
) -> Result<Option<bool>, String> {
    let config = match &tool.constrained_sampling {
        Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema { strict })) => strict,
        _ => return Ok(None),
    };

    if supports_strict_mode {
        return match make_strict_json_schema(&tool.parameters) {
            Ok(_) => Ok(Some(true)),
            Err(error) => {
                if *config == ConstrainedSamplingStrict::Prefer {
                    Ok(None)
                } else {
                    Err(format!(
                        "Tool \"{}\" requires JSON-schema constrained sampling, but {error}.",
                        tool.name
                    ))
                }
            }
        };
    }
    if *config == ConstrainedSamplingStrict::Require {
        return Err(format!(
            "Tool \"{}\" requires JSON-schema constrained sampling, but strict tools are unsupported.",
            tool.name
        ));
    }
    Ok(None)
}

/// pi: `getJsonSchemaToolParameters`
pub fn get_json_schema_tool_parameters(tool: &Tool, strict: Option<bool>) -> Value {
    if strict == Some(true) {
        make_strict_json_schema(&tool.parameters).unwrap_or_else(|_| tool.parameters.clone())
    } else {
        tool.parameters.clone()
    }
}

/// 给 `convert_tools` 用的一个函数位：空对象里的字段按插入顺序（与上游一致）。
pub fn strict_tool_parameters(tool: &Tool, supports_strict_mode: bool) -> Result<(Value, bool), String> {
    let strict = resolve_json_schema_strict_sampling(tool, supports_strict_mode)?;
    Ok((get_json_schema_tool_parameters(tool, strict), strict == Some(true)))
}
