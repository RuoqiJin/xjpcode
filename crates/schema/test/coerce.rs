//! 移植自上游 `packages/schema/test/coerce.test.ts`。
//!
//! `compile(schema, { coerce: true })` 的 `coerce()` 与 `parse()`，三趟纠正（可选属性上的 null、TypeBox
//! 的转换规则、按类型的原始纠正）、它们冲突时的先后，以及围绕它们的保证。
//!
//! 三处 JS 专有的东西没有对应物，略去并在 `PORTING.md` 记着：`Object.freeze` 的 schema（Rust 没有冻结
//! 这个概念）、`Function` 构造函数被禁（Rust 侧编译检查根本不用代码生成，这条性质本来就成立）、用一个
//! 自己引用自己的 JS 对象搭出的环状 schema（`serde_json::Value` 不能成环；`$ref` 的环有专门用例）。
//! 值为 bigint、undefined 的用例同样没有对应值。上游断言“值原样返回”“复用未改动的子对象”靠的是引用
//! 相同，Rust 侧的值总是复制出来的，所以这里只比值。

use serde_json::{json, Value};
use xjpcode_schema::formats::index::formats;
use xjpcode_schema::types::{CompileOptions, ParseResult, ValidationError};
use xjpcode_schema::{compile, equal, Validator};

fn options() -> CompileOptions {
    CompileOptions {
        formats: Some(formats().clone()),
        coerce: true,
        ..CompileOptions::default()
    }
}

fn compiled(schema: &Value) -> Validator {
    compile(schema, &options()).unwrap_or_else(|error| panic!("compile {schema}: {error}"))
}

fn coerce(schema: &Value, value: &Value) -> Value {
    compiled(schema)
        .coerce(value)
        .unwrap_or_else(|error| panic!("coerce {value} under {schema}: {error}"))
}

/// 一个属性 `value` 被 `{ value }` 包在 object schema 里纠正后的样子。
fn coerce_value(schema: &Value, value: &Value) -> Value {
    let root = json!({
        "type": "object",
        "properties": { "value": schema },
        "required": ["value"]
    });
    coerce(&root, &json!({ "value": value }))["value"].clone()
}

fn parse(schema: &Value, value: &Value) -> ParseResult {
    compiled(schema)
        .parse(value)
        .unwrap_or_else(|error| panic!("parse {value} under {schema}: {error}"))
}

fn assert_value(actual: &Value, expected: &Value) {
    assert!(
        equal(actual, expected),
        "expected {expected}\n     got {actual}"
    );
}

fn error(path: &str, keyword: &str, message: &str) -> ValidationError {
    ValidationError::new(path, keyword, message)
}

fn ok(value: Value) -> ParseResult {
    ParseResult::Ok { value }
}

fn errors(errors: Vec<ValidationError>) -> ParseResult {
    ParseResult::Errors { errors }
}

fn number() -> Value {
    json!({ "type": "number" })
}

fn string() -> Value {
    json!({ "type": "string" })
}

fn array(items: Value) -> Value {
    json!({ "type": "array", "items": items })
}

fn optional(schema: Value) -> (Value, bool) {
    (schema, false)
}

fn required_property(schema: Value) -> (Value, bool) {
    (schema, true)
}

/// `obj()`：`properties` 里的 `(schema, required)`，闭合对象。
fn closed_object(properties: &[(&str, (Value, bool))]) -> Value {
    let mut props = serde_json::Map::new();
    let mut required: Vec<Value> = Vec::new();
    for (name, (schema, is_required)) in properties {
        props.insert((*name).to_string(), schema.clone());
        if *is_required {
            required.push(Value::String((*name).to_string()));
        }
    }
    json!({
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false
    })
}

#[test]
fn compiles_with_coerce_without_changing_check() {
    // 上游在这里断言不带选项的 validator 没有 `coerce`/`parse`；Rust 没有“按选项决定有没有这个方法”的
    // 类型，两个方法一直在（见 `PORTING.md` 差异表），所以只锁住 check 不变。
    let schema = closed_object(&[("n", required_property(number()))]);
    assert!(compiled(&schema).check(&json!({ "n": 1 })));
    let plain = compile(&schema, &CompileOptions::default()).expect("compile");
    assert!(plain.check(&json!({ "n": 1 })));
}

#[test]
fn never_modifies_the_value() {
    let value = json!({ "count": "42", "tags": "a" });
    let schema = closed_object(&[
        ("count", required_property(number())),
        ("tags", required_property(array(string()))),
    ]);
    assert_value(&coerce(&schema, &value), &json!({ "count": 42, "tags": ["a"] }));
    assert_value(&value, &json!({ "count": "42", "tags": "a" }));
}

#[test]
fn leaves_an_unchanged_value_unchanged() {
    let value = json!({ "a": { "b": [1, 2] }, "c": "x" });
    let schema = closed_object(&[
        ("a", required_property(closed_object(&[("b", required_property(array(number()))) ]))),
        ("c", required_property(string())),
    ]);
    assert_value(&coerce(&schema, &value), &value);
}

#[test]
fn copies_only_the_changed_paths() {
    let value = json!({ "a": { "b": [1, 2] }, "c": { "d": "1" } });
    let schema = closed_object(&[
        ("a", required_property(closed_object(&[("b", required_property(array(number()))) ]))),
        ("c", required_property(closed_object(&[("d", required_property(number()))]))),
    ]);
    let coerced = coerce(&schema, &value);
    assert_value(
        &coerced,
        &json!({ "a": { "b": [1, 2] }, "c": { "d": 1 } }),
    );
    assert_value(&value, &json!({ "a": { "b": [1, 2] }, "c": { "d": "1" } }));
}

#[test]
fn leaves_values_it_cannot_coerce_for_the_check_to_report() {
    let schema = closed_object(&[("count", required_property(number()))]);
    let validator = compiled(&schema);
    let coerced = validator.coerce(&json!({ "count": "many" })).expect("coerce");
    assert_value(&coerced, &json!({ "count": "many" }));
    assert!(!validator.check(&coerced));
}

#[test]
fn parse_returns_the_coerced_value_when_it_is_valid() {
    let schema = closed_object(&[
        ("count", required_property(number())),
        ("tags", required_property(array(string()))),
    ]);
    assert_eq!(
        parse(&schema, &json!({ "count": "42", "tags": "a" })),
        ok(json!({ "count": 42, "tags": ["a"] }))
    );
}

#[test]
fn parse_returns_the_errors_of_the_coerced_value_when_it_is_invalid() {
    let schema = closed_object(&[
        ("count", required_property(number())),
        ("name", required_property(string())),
    ]);
    assert_eq!(
        parse(&schema, &json!({ "count": "many", "name": 7 })),
        errors(vec![error(
            "/count",
            "type",
            "expected number, got string"
        )])
    );
}

#[test]
fn validate_checks_without_coercing_with_or_without_the_coerce_option() {
    let schema = closed_object(&[
        ("count", required_property(number())),
        ("tags", required_property(array(string()))),
    ]);
    let value = json!({ "count": 42, "tags": ["a"] });
    let plain = compile(&schema, &CompileOptions::default()).expect("compile");
    for validator in [plain, compiled(&schema)] {
        assert_eq!(
            validator.validate(&value).expect("validate"),
            ok(value.clone())
        );
        // parse() 会把 "42" 变成 42、"a" 变成 ["a"]；validate() 两个都报。
        assert_eq!(
            validator
                .validate(&json!({ "count": "42", "tags": "a" }))
                .expect("validate"),
            errors(vec![
                error("/count", "type", "expected number, got string"),
                error("/tags", "type", "expected array, got string"),
            ])
        );
    }
}

#[test]
fn pass_1_drops_null_in_optional_non_nullable_properties_also_nested() {
    let metadata = closed_object(&[("enabled", optional(json!({ "type": "boolean" })))]);
    let nullable = json!({ "anyOf": [string(), json!({ "type": "null" })] });
    let mut schema = closed_object(&[
        ("path", required_property(string())),
        ("offset", optional(number())),
        ("nullable", optional(nullable)),
        ("metadata", required_property(metadata)),
    ]);
    // `obj(..., { open: true })`：不写 additionalProperties。
    schema
        .as_object_mut()
        .expect("object")
        .remove("additionalProperties");
    assert_value(
        &coerce(
            &schema,
            &json!({
                "path": "file.txt",
                "offset": null,
                "nullable": null,
                "metadata": { "enabled": null }
            }),
        ),
        &json!({ "path": "file.txt", "nullable": null, "metadata": {} }),
    );
}

#[test]
fn pass_1_keeps_a_null_whose_property_is_a_ref() {
    let schema = json!({
        "type": "object",
        "properties": { "value": { "$ref": "#/$defs/value" } },
        "$defs": { "value": { "anyOf": [{ "type": "number" }, { "type": "null" }] } }
    });
    assert_value(
        &coerce(&schema, &json!({ "value": null })),
        &json!({ "value": null }),
    );
}

#[test]
fn pass_1_walks_array_items() {
    let item = closed_object(&[
        ("a", required_property(string())),
        ("b", optional(json!({ "type": "integer" }))),
    ]);
    let schema = array(item);
    assert_value(
        &coerce(&schema, &json!([{ "a": "x", "b": null }])),
        &json!([{ "a": "x" }]),
    );
}

#[test]
fn pass_2_converts_primitives_by_type() {
    assert_value(&coerce_value(&number(), &json!("42")), &json!(42));
    assert_value(&coerce_value(&number(), &json!("")), &json!(0));
    assert_value(&coerce_value(&number(), &json!("true")), &json!(1));
    assert_value(
        &coerce_value(&json!({ "type": "integer" }), &json!("3.5")),
        &json!(3),
    );
    assert_value(
        &coerce_value(&json!({ "type": "integer" }), &json!(1.5)),
        &json!(1),
    );
    assert_value(
        &coerce_value(&json!({ "type": "boolean" }), &json!("1")),
        &json!(true),
    );
    assert_value(
        &coerce_value(&json!({ "type": "boolean" }), &json!("FALSE")),
        &json!(false),
    );
    assert_value(
        &coerce_value(&json!({ "type": "null" }), &json!("null")),
        &Value::Null,
    );
    assert_value(
        &coerce_value(&json!({ "type": "string" }), &json!(42)),
        &json!("42"),
    );
}

#[test]
fn pass_2_wraps_a_single_value_into_an_array_and_converts_the_items() {
    assert_value(
        &coerce_value(&array(number()), &json!("1")),
        &json!([1]),
    );
    assert_value(
        &coerce_value(&array(string()), &Value::Null),
        &json!(["null"]),
    );
}

#[test]
fn pass_2_converts_tuple_items_in_place_and_never_wraps_for_a_tuple() {
    let tuple = json!({
        "type": "array",
        "prefixItems": [{ "type": "number" }, { "type": "boolean" }],
        "items": false
    });
    assert_value(&coerce_value(&tuple, &json!(["1", "true"])), &json!([1, true]));
    assert_value(&coerce_value(&tuple, &json!("1")), &json!("1"));
}

#[test]
fn pass_2_converts_const_and_enum_values_only_when_the_result_is_a_member() {
    assert_value(&coerce_value(&json!({ "const": 1 }), &json!("1")), &json!(1));
    assert_value(&coerce_value(&json!({ "const": 1 }), &json!("2")), &json!("2"));
    assert_value(
        &coerce_value(&json!({ "enum": [1, 2, "hello"] }), &json!("2")),
        &json!(2),
    );
    assert_value(
        &coerce_value(&json!({ "enum": [1, 2, "hello"] }), &json!("world")),
        &json!("world"),
    );
}

#[test]
fn pass_2_keeps_a_value_one_union_branch_accepts() {
    let union = json!({ "anyOf": [number(), json!({ "type": "null" })] });
    assert_value(&coerce_value(&union, &Value::Null), &Value::Null);
    assert_value(&coerce_value(&union, &json!("42")), &json!(42));
    assert_value(&coerce_value(&union, &json!("null")), &Value::Null);
    assert_value(
        &coerce_value(
            &json!({ "oneOf": [json!({ "type": "boolean" }), number()] }),
            &json!("1"),
        ),
        &json!(true),
    );
}

#[test]
fn pass_2_converts_all_of_with_each_member_in_turn() {
    let mut first = closed_object(&[("a", required_property(number()))]);
    first.as_object_mut().expect("object").remove("additionalProperties");
    let mut second = closed_object(&[("b", required_property(json!({ "type": "boolean" })))]);
    second.as_object_mut().expect("object").remove("additionalProperties");
    let schema = json!({ "allOf": [first, second] });
    assert_value(
        &coerce(&schema, &json!({ "a": "1", "b": "true" })),
        &json!({ "a": 1, "b": true }),
    );
}

#[test]
fn pass_2_follows_a_ref() {
    let schema = json!({
        "type": "object",
        "properties": { "item": { "$ref": "#/$defs/item" } },
        "$defs": { "item": { "type": "object", "properties": { "id": { "type": "integer" } } } }
    });
    assert_value(
        &coerce(&schema, &json!({ "item": { "id": "7" } })),
        &json!({ "item": { "id": 7 } }),
    );
}

#[test]
fn pass_2_follows_a_relative_ref_from_the_root_id_on_the_first_call_of_a_schema_with_dynamic_ref() {
    let schema = json!({
        "$id": "https://example.com/root.json",
        "$dynamicAnchor": "node",
        "type": "object",
        "properties": {
            "count": { "$ref": "defs/n.json" },
            "children": { "type": "array", "items": { "$dynamicRef": "#node" } }
        }
    });
    let mut options = options();
    options.schemas.insert(
        "https://example.com/defs/n.json".to_string(),
        json!({ "type": "integer" }),
    );
    let validator = compile(&schema, &options).expect("compile");
    assert_value(
        &validator.coerce(&json!({ "count": "2" })).expect("coerce"),
        &json!({ "count": 2 }),
    );
    assert_eq!(
        validator.parse(&json!({ "count": "2" })).expect("parse"),
        ok(json!({ "count": 2 }))
    );
    assert_eq!(
        validator.parse(&json!({ "count": "2" })).expect("parse"),
        ok(json!({ "count": 2 }))
    );
}

#[test]
fn pass_2_converts_pattern_and_additional_properties() {
    let schema = json!({
        "type": "object",
        "properties": { "a": { "type": "number" } },
        "patternProperties": { "^n_": { "type": "number" } },
        "additionalProperties": { "type": "boolean" }
    });
    assert_value(
        &coerce(&schema, &json!({ "a": "1", "n_x": "2", "other": "true" })),
        &json!({ "a": 1, "n_x": 2, "other": true }),
    );
}

#[test]
fn pass_3_coerces_like_ajv_coerce_types() {
    let cases: Vec<(Value, Value, Value)> = vec![
        (number(), json!(true), json!(1)),
        (number(), Value::Null, json!(0)),
        (json!({ "type": "integer" }), json!("42"), json!(42)),
        (json!({ "type": "boolean" }), json!("true"), json!(true)),
        (json!({ "type": "boolean" }), json!(0), json!(false)),
        (string(), json!(true), json!("true")),
        (json!({ "type": "null" }), json!(""), Value::Null),
        (json!({ "type": "null" }), json!(0), Value::Null),
        (json!({ "type": "null" }), json!(false), Value::Null),
    ];
    for (schema, input, expected) in cases {
        assert_value(&coerce_value(&schema, &input), &expected);
    }
}

#[test]
fn pass_3_keeps_a_value_that_matches_one_of_a_type_arrays_types() {
    assert_value(
        &coerce_value(&json!({ "type": ["number", "string"] }), &json!("1")),
        &json!("1"),
    );
    assert_value(
        &coerce_value(&json!({ "type": ["boolean", "number"] }), &json!("1")),
        &json!(1),
    );
    assert_value(
        &coerce_value(&json!({ "type": ["integer", "null"] }), &json!("5")),
        &json!(5),
    );
}

#[test]
fn pass_3_accepts_null_for_nullable_array_schemas_with_items() {
    let schema = json!({ "type": ["array", "null"], "items": { "type": "string" } });
    assert_value(&coerce_value(&schema, &Value::Null), &Value::Null);
}

#[test]
fn pass_3_coerces_nullable_unions_when_the_value_matches_no_branch() {
    assert_value(
        &coerce_value(
            &json!({ "anyOf": [number(), json!({ "type": "null" })] }),
            &json!("42"),
        ),
        &json!(42),
    );
    assert_value(
        &coerce_value(
            &json!({ "oneOf": [number(), json!({ "type": "null" })] }),
            &Value::Null,
        ),
        &Value::Null,
    );
}

#[test]
fn pass_3_leaves_values_no_rule_coerces() {
    let schema = json!({
        "type": "object",
        "properties": { "value": { "type": "integer" } },
        "required": ["value"]
    });
    let validator = compiled(&schema);
    for input in [json!("abc"), json!("4x"), json!({}), json!(["1", "2"])] {
        let coerced = validator
            .coerce(&json!({ "value": input }))
            .expect("coerce");
        assert!(!validator.check(&coerced));
    }
}

#[test]
fn where_the_passes_conflict_the_conversion_rules_win() {
    // 第 3 趟单独跑会给 ""，第 2 趟给 "null"。
    assert_value(&coerce_value(&string(), &Value::Null), &json!("null"));
    // 第 3 趟会留下空串。
    assert_value(&coerce_value(&number(), &json!("")), &json!(0));
    // 第 3 趟只认 "true"/"false"/1/0。
    assert_value(
        &coerce_value(&json!({ "type": "boolean" }), &json!("0")),
        &json!(false),
    );
    // 第 3 趟会拒绝 "42.1"。
    assert_value(
        &coerce_value(&json!({ "type": "integer" }), &json!("42.1")),
        &json!(42),
    );
    // 第 2 趟单独跑会给 "null"。
    let schema = closed_object(&[("name", optional(string()))]);
    assert_value(&coerce(&schema, &json!({ "name": null })), &json!({}));
}

fn tree() -> Value {
    json!({
        "type": "object",
        "properties": { "t": { "$ref": "#/$defs/Tree" } },
        "$defs": {
            "Tree": {
                "anyOf": [
                    { "type": "string" },
                    { "type": "array", "items": { "$ref": "#/$defs/Tree" } }
                ]
            }
        }
    })
}

fn json_value_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "data": { "$ref": "#/$defs/Json" } },
        "required": ["data"],
        "$defs": {
            "Json": {
                "anyOf": [
                    { "type": "string" },
                    { "type": "number" },
                    { "type": "boolean" },
                    { "type": "array", "items": { "$ref": "#/$defs/Json" } },
                    { "type": "object", "additionalProperties": { "$ref": "#/$defs/Json" } }
                ]
            }
        }
    })
}

#[test]
fn recursively_converts_through_a_union_by_its_first_accepting_branch() {
    let validator = compiled(&tree());
    assert_eq!(
        validator.parse(&json!({ "t": ["a", ["b"]] })).expect("parse"),
        ok(json!({ "t": ["a", ["b"]] }))
    );
    assert_eq!(
        validator.parse(&json!({ "t": 5 })).expect("parse"),
        ok(json!({ "t": "5" }))
    );
    assert_eq!(
        validator.parse(&json!({ "t": [5] })).expect("parse"),
        ok(json!({ "t": ["5"] }))
    );
    assert_eq!(
        compiled(&json_value_schema())
            .parse(&json!({ "data": { "a": [1, null] } }))
            .expect("parse"),
        ok(json!({ "data": { "a": [1, "null"] } }))
    );
}

#[test]
fn a_value_a_recursive_schema_cannot_convert_is_left_for_the_check_to_report() {
    let validator = compiled(&tree());
    assert!(matches!(
        validator.parse(&json!({ "t": {} })).expect("parse"),
        ParseResult::Errors { .. }
    ));
    assert_value(
        &validator.coerce(&json!({ "t": { "a": 1 } })).expect("coerce"),
        &json!({ "t": { "a": 1 } }),
    );
}

#[test]
fn converts_a_scalar_or_array_schema_that_refers_to_itself() {
    let schema = json!({
        "type": "object",
        "properties": {
            "value": {
                "anyOf": [
                    { "type": "number" },
                    { "type": "array", "items": { "$ref": "#/properties/value" } }
                ]
            }
        },
        "required": ["value"]
    });
    let validator = compiled(&schema);
    assert_eq!(
        validator.parse(&json!({ "value": "42" })).expect("parse"),
        ok(json!({ "value": 42 }))
    );
    assert_eq!(
        validator.parse(&json!({ "value": ["1", ["2"]] })).expect("parse"),
        ok(json!({ "value": [1, [2]] }))
    );
    assert!(matches!(
        validator.parse(&json!({ "value": {} })).expect("parse"),
        ParseResult::Errors { .. }
    ));
}

#[test]
fn resets_the_re_entry_state_between_calls_and_leaves_the_input_unchanged() {
    let schema = json!({
        "anyOf": [
            { "type": "number" },
            { "type": "array", "items": { "$ref": "#" } }
        ]
    });
    let validator = compiled(&schema);
    for _ in 0..20 {
        assert!(matches!(
            validator.parse(&json!({})).expect("parse"),
            ParseResult::Errors { .. }
        ));
        assert_eq!(
            validator.parse(&json!("42")).expect("parse"),
            ok(json!(42))
        );
        let input = json!([1, "2", ["3"]]);
        assert_eq!(
            validator.parse(&input).expect("parse"),
            ok(json!([1, 2, [3]]))
        );
        assert_value(&input, &json!([1, "2", ["3"]]));
    }
}

#[test]
fn converts_and_explains_a_deep_value_every_branch_rejects_in_linear_time() {
    // 每个对象分支都转换并解释每个子值：不按一次调用记住结果的话，这是 2^深度 的工作。
    let s = json!({
        "anyOf": [
            { "type": "object", "additionalProperties": { "$ref": "#/$defs/S" }, "required": ["a"] },
            { "type": "object", "additionalProperties": { "$ref": "#/$defs/S" }, "required": ["b"] }
        ]
    });
    let schema = json!({
        "type": "object",
        "properties": { "s": { "$ref": "#/$defs/S" } },
        "$defs": { "S": s }
    });
    let validator = compiled(&schema);
    let mut value = json!({ "x": 1 });
    for _ in 0..40 {
        value = json!({ "c": value });
    }
    let result = validator.parse(&json!({ "s": value })).expect("parse");
    let mut expected: Vec<ValidationError> = Vec::new();
    for depth in 0..41 {
        expected.push(error(
            &format!("/s{}/a", "/c".repeat(depth)),
            "required",
            "missing",
        ));
    }
    expected.push(error(
        &format!("/s{}/x", "/c".repeat(40)),
        "type",
        "expected object, got integer",
    ));
    assert_eq!(result, errors(expected));
}

#[test]
fn converts_a_typebox_cyclic_union() {
    // `Type.Cyclic({ Tree: Type.Union([Type.String(), Type.Array(Type.Ref("Tree"))]) }, "Tree")`
    // 给出的 JSON（见 `crates/schema/test/coerce_typebox.rs` 的同一份记录）。
    let tree = json!({
        "$defs": {
            "Tree": {
                "anyOf": [
                    { "type": "string" },
                    { "type": "array", "items": { "$ref": "Tree" } }
                ],
                "$id": "Tree"
            }
        },
        "$ref": "Tree"
    });
    let schema = json!({
        "type": "object",
        "required": ["t"],
        "properties": { "t": tree }
    });
    let validator = compiled(&schema);
    assert_eq!(
        validator.parse(&json!({ "t": [5] })).expect("parse"),
        ok(json!({ "t": ["5"] }))
    );
    assert!(matches!(
        validator.parse(&json!({ "t": { "a": 1 } })).expect("parse"),
        ParseResult::Errors { .. }
    ));
}

#[test]
fn member_keywords_keep_a_pattern_matched_property_from_additional_properties_in_every_pass() {
    // `Type.Record(Type.String(), Type.Number(), { additionalProperties: Type.String() })` 给出的 JSON。
    let record = json!({
        "type": "object",
        "patternProperties": { "^.*$": { "type": "number" } },
        "additionalProperties": { "type": "string" }
    });
    assert_eq!(
        compiled(&record).parse(&json!({ "n1": "1" })).expect("parse"),
        ok(json!({ "n1": 1 }))
    );
    let plain = json!({
        "type": "object",
        "patternProperties": { "^n_": { "type": ["integer", "null"] } },
        "additionalProperties": { "type": "string" }
    });
    assert_value(
        &coerce(&plain, &json!({ "n_a": "5", "other": 7 })),
        &json!({ "n_a": 5, "other": "7" }),
    );
}

#[test]
fn member_keywords_drop_optional_nulls_inside_declared_pattern_and_additional_property_values() {
    let item = closed_object(&[(
        "n",
        optional(json!({ "type": "integer", "minimum": 1 })),
    )]);
    let schemas = [
        closed_object(&[("entry", required_property(item.clone()))]),
        json!({ "type": "object", "additionalProperties": item }),
        json!({
            "type": "object",
            "patternProperties": { "^entry$": item },
            "additionalProperties": false
        }),
    ];
    for schema in schemas {
        let input = json!({ "entry": { "n": null } });
        assert_eq!(
            compiled(&schema).parse(&input).expect("parse"),
            ok(json!({ "entry": {} }))
        );
        assert_value(&input, &json!({ "entry": { "n": null } }));
    }
}

#[test]
fn member_keywords_keep_a_null_under_pattern_and_additional_properties() {
    let schema = json!({
        "type": "object",
        "additionalProperties": { "type": ["integer", "null"] }
    });
    assert_eq!(
        compiled(&schema)
            .parse(&json!({ "n": null }))
            .expect("parse"),
        ok(json!({ "n": null }))
    );
}

#[test]
fn member_keywords_drop_optional_nulls_in_tuple_items_as_in_array_items() {
    let item = closed_object(&[("n", optional(json!({ "type": "integer" })))]);
    assert_value(&coerce(&array(item.clone()), &json!([{ "n": null }])), &json!([{}]));
    let tuple = json!({
        "type": "array",
        "prefixItems": [item],
        "items": false,
        "minItems": 1
    });
    assert_value(&coerce(&tuple, &json!([{ "n": null }])), &json!([{}]));
}

#[test]
fn member_keywords_coerce_tuple_items_by_type_in_pass_3() {
    let schema = json!({
        "type": "array",
        "prefixItems": [
            { "type": ["integer", "null"] },
            { "type": ["boolean", "null"] }
        ]
    });
    assert_value(&coerce(&schema, &json!(["5", "true"])), &json!([5, true]));
}

#[test]
fn member_keywords_convert_the_properties_a_union_node_declares_besides_its_branches() {
    let schema = json!({
        "type": "object",
        "properties": { "a": { "type": "integer" }, "b": { "type": "integer" } },
        "anyOf": [{ "required": ["a"] }, { "required": ["b"] }]
    });
    assert_eq!(
        compiled(&schema).parse(&json!({ "a": "3.5" })).expect("parse"),
        ok(json!({ "a": 3 }))
    );
}

#[test]
fn member_keywords_check_union_candidates_after_the_nodes_own_properties_convert() {
    for keyword in ["anyOf", "oneOf"] {
        let schema = json!({
            "type": "object",
            (keyword): [
                {
                    "type": "object",
                    "properties": { "a": { "type": "array", "items": { "type": "integer" } } },
                    "required": ["a"]
                },
                {
                    "type": "object",
                    "properties": { "b": { "type": "boolean" } },
                    "required": ["b"]
                }
            ],
            "properties": { "c": { "type": "integer" } },
            "required": ["c"]
        });
        let input = json!({ "a": "3", "c": "2" });
        assert_eq!(
            compiled(&schema).parse(&input).expect("parse"),
            ok(json!({ "a": [3], "c": 2 }))
        );
        assert_value(&input, &json!({ "a": "3", "c": "2" }));
    }
}

#[test]
fn a_ref_that_only_points_back_at_itself_stops_at_the_re_entry() {
    // pi 在这里抛 `RangeError: Maximum call stack size exceeded`（差异表 #56、#61）；Rust 的栈溢出
    // 接不住，只能停住。停住的结果是值原样返回，parse 随后在环上给出结论——两条路都不带走进程。
    let validator = compiled(&json!({ "allOf": [{ "$ref": "#" }] }));
    assert_value(&validator.coerce(&json!({})).expect("coerce"), &json!({}));
    assert_value(
        &validator.coerce(&json!([1, "2"])).expect("coerce"),
        &json!([1, "2"]),
    );
    assert!(matches!(
        validator.parse(&json!({})).expect("parse"),
        ParseResult::Errors { .. }
    ));
}

#[test]
fn an_integral_float_comes_back_as_an_integer() {
    // 工具按整数读参数（`as_i64`/`as_u64`）：pi 的每个数都是 double，交给工具的是 10；
    // `serde_json` 会保留输入里的 10.0 浮点写法（差异表 #70）。
    let schema = json!({
        "type": "object",
        "properties": { "limit": { "type": "integer" } }
    });
    let value = compiled(&schema)
        .coerce(&json!({ "limit": 10.0 }))
        .expect("coerce");
    assert_eq!(value["limit"].as_i64(), Some(10));
    assert_value(&value, &json!({ "limit": 10 }));

    // 没有 type 的地方也一样：pi 的写法由 double 决定，不由 schema 决定。
    let plain = compiled(&json!({ "type": "object" }))
        .coerce(&json!({ "n": 10.0, "m": 1.5, "z": -0.0 }))
        .expect("coerce");
    assert_eq!(plain["n"].as_i64(), Some(10));
    assert_eq!(plain["m"].as_f64(), Some(1.5));
    assert_eq!(plain["z"].as_i64(), Some(0));

    // 已经是整数的值原样留着：2^53 以上的 u64 不被舍成 double（差异表 #65）。
    let big = compiled(&json!({ "type": "object" }))
        .coerce(&json!({ "n": 9007199254740993u64 }))
        .expect("coerce");
    assert_eq!(big["n"].as_u64(), Some(9007199254740993));
}

#[test]
fn dropping_a_null_keeps_the_order_of_the_remaining_keys() {
    // JS 的 `delete` 不挪动其余的键；serde_json 的 `Map::remove` 在 `preserve_order` 下是
    // `IndexMap::swap_remove`，会把最后一项换到空出来的位置上（差异表 #64）。
    let schema = json!({
        "type": "object",
        "properties": {
            "a": { "type": "string" },
            "b": { "type": "number" },
            "c": { "type": "number" }
        }
    });
    let value = compiled(&schema)
        .coerce(&json!({ "a": null, "b": 1, "c": 2 }))
        .expect("coerce");
    assert_value(&value, &json!({ "b": 1, "c": 2 }));
    let keys: Vec<&String> = value.as_object().expect("object").keys().collect();
    assert_eq!(keys, ["b", "c"]);
}
