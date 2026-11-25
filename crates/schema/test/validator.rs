//! 移植自上游 `packages/schema/test/validator.test.ts`。
//!
//! 只搬值能用 JSON 表达的那些用例：TypeBox 的静态形状、`Uint8Array`、bigint、`undefined`、
//! `NaN`/`Infinity` 和孤立代理项在 Rust 侧没有对应值（见 `PORTING.md`）。
//!
//! 上游用 TypeBox 现搭 schema 的两条（`compiles TypeBox schemas as they are`、`reject a TypeBox Ref
//! without its definition`）在 Rust 侧写成 TypeBox 给出的那份 JSON——断言只依赖那份 JSON。

use serde_json::{json, Value};
use xjpcode_schema::formats::index::formats;
use xjpcode_schema::types::{CompileOptions, Draft, ParseResult, UnresolvedRefs, ValidationError};
use xjpcode_schema::{compile, Validator};

/// 上游 `validator.test.ts` 里的 `read`。
fn read() -> Value {
    json!({
        "type": "object",
        "properties": { "path": { "type": "string" }, "offset": { "type": "integer", "minimum": 1 } },
        "required": ["path"],
        "additionalProperties": false
    })
}

/// 上游 `validator.test.ts` 里的 `block`。
fn block() -> Value {
    json!({
        "anyOf": [
            {
                "type": "object",
                "properties": { "kind": { "const": "text" }, "text": { "type": "string" } },
                "required": ["kind", "text"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": { "kind": { "const": "image" }, "width": { "type": "integer", "minimum": 0 } },
                "required": ["kind", "width"],
                "additionalProperties": false
            }
        ]
    })
}

fn compiled(schema: &Value, options: &CompileOptions) -> Validator {
    compile(schema, options).unwrap_or_else(|error| panic!("compile {schema}: {error}"))
}

fn check(schema: &Value, value: &Value) -> bool {
    compiled(schema, &CompileOptions::default()).check(value)
}

fn check_with(schema: &Value, options: &CompileOptions, value: &Value) -> bool {
    compiled(schema, options).check(value)
}

fn error(path: &str, keyword: &str, message: &str) -> ValidationError {
    ValidationError::new(path, keyword, message)
}

fn errors(schema: &Value, value: &Value) -> Vec<ValidationError> {
    errors_with(schema, &CompileOptions::default(), value)
}

fn errors_with(schema: &Value, options: &CompileOptions, value: &Value) -> Vec<ValidationError> {
    compiled(schema, options)
        .errors(value)
        .unwrap_or_else(|error| panic!("errors of {value} under {schema}: {error}"))
}

#[test]
fn checks_a_detached_check_function() {
    // 上游：`const { check } = compile(read); [{path:"a"},{path:1}].filter(check)`。
    let validator = compiled(&read(), &CompileOptions::default());
    let check = validator.check_fn();
    let values = [json!({ "path": "a" }), json!({ "path": 1 })];
    let valid: Vec<&Value> = values.iter().filter(|value| check(value)).collect();
    assert_eq!(valid, vec![&json!({ "path": "a" })]);
}

#[test]
fn checks_properties_named_like_object_prototype_members_as_own_properties() {
    let schema = json!({
        "type": "object",
        "properties": { "constructor": { "type": "string" }, "toString": { "type": "number" } },
        "required": ["constructor"],
        "additionalProperties": false
    });
    assert!(!check(&schema, &json!({})));
    assert!(check(&schema, &json!({ "constructor": "a" })));
    assert!(!check(
        &schema,
        &json!({ "constructor": "a", "toString": "b" })
    ));
}

#[test]
fn does_not_accept_every_value_for_a_type_named_like_an_object_prototype_member() {
    // 未知类型名不置任何类型位。
    assert!(!check(&json!({ "type": "constructor" }), &json!(1)));
}

#[test]
fn treats_format_as_an_annotation_unless_formats_are_given() {
    let schema = json!({ "type": "string", "format": "email" });
    assert!(check(&schema, &json!("not an email")));
    let options = CompileOptions {
        formats: Some(formats().clone()),
        ..CompileOptions::default()
    };
    assert!(!check_with(&schema, &options, &json!("not an email")));
    assert!(check_with(&schema, &options, &json!("a@example.com")));
}

#[test]
fn resolves_ref_to_registered_documents() {
    let point = json!({
        "type": "object",
        "properties": { "x": { "type": "number" } },
        "required": ["x"]
    });
    let mut options = CompileOptions::default();
    options
        .schemas
        .insert("https://example.com/point.json".to_string(), point);
    let schema = json!({ "$ref": "https://example.com/point.json" });
    assert!(check_with(&schema, &options, &json!({ "x": 1 })));
    assert!(!check_with(&schema, &options, &json!({})));
}

#[test]
fn fails_for_a_ref_that_names_nothing() {
    for schema in [
        json!({ "$ref": "https://example.com/missing.json" }),
        json!({ "$ref": "#/$defs/missing" }),
    ] {
        let error = compile(&schema, &CompileOptions::default()).expect_err("unresolved $ref");
        assert!(
            error.message().contains("Unresolved $ref"),
            "unexpected error: {error}"
        );
    }
}

#[test]
fn applies_draft_7_semantics_ref_overrides_its_siblings() {
    let schema = json!({
        "definitions": { "s": { "type": "string" } },
        "$ref": "#/definitions/s",
        "maxLength": 1
    });
    let draft7 = CompileOptions {
        draft: Draft::Draft7,
        ..CompileOptions::default()
    };
    assert!(check_with(&schema, &draft7, &json!("ab")));
    assert!(!check(&schema, &json!("ab")));
}

#[test]
fn compiles_a_recursive_schema() {
    let tree = json!({
        "type": "object",
        "properties": { "value": { "type": "number" }, "children": { "type": "array", "items": { "$ref": "#" } } },
        "required": ["value"]
    });
    assert!(check(
        &tree,
        &json!({ "value": 1, "children": [{ "value": 2, "children": [{ "value": 3 }] }] })
    ));
    assert!(!check(
        &tree,
        &json!({ "value": 1, "children": [{ "value": 2, "children": [{}] }] })
    ));
    assert_eq!(
        errors(
            &tree,
            &json!({ "value": 1, "children": [{ "value": 2, "children": [{}] }] })
        ),
        vec![error("/children/0/children/0/value", "required", "missing")]
    );
}

#[test]
fn applies_a_relative_root_id_once_when_resolving_local_references() {
    // 上游 #11697。
    let schema = json!({
        "$id": "schemas/tool.json",
        "type": "object",
        "properties": { "count": { "$ref": "#/$defs/count" } },
        "required": ["count"],
        "$defs": { "count": { "type": "integer" } }
    });
    assert!(check(&schema, &json!({ "count": 2 })));
    let rejecting = CompileOptions {
        unresolved_refs: UnresolvedRefs::Reject,
        ..CompileOptions::default()
    };
    assert!(check_with(&schema, &rejecting, &json!({ "count": 2 })));
    assert!(!check_with(&schema, &rejecting, &json!({ "count": "a" })));
    assert_eq!(
        errors_with(&schema, &rejecting, &json!({ "count": "a" })),
        vec![error("/count", "type", "expected integer, got string")]
    );
    let coercing = CompileOptions {
        coerce: true,
        ..CompileOptions::default()
    };
    assert_eq!(
        compile(&schema, &coercing)
            .expect("compile")
            .parse(&json!({ "count": "2" }))
            .expect("parse"),
        ParseResult::Ok {
            value: json!({ "count": 2 })
        }
    );
}

#[test]
fn applies_a_relative_root_id_once_in_schemas_with_unevaluated_keywords() {
    let props = json!({
        "$id": "schemas/tool.json",
        "type": "object",
        "properties": { "count": { "$ref": "#/$defs/count" } },
        "$defs": { "count": { "type": "integer" } },
        "unevaluatedProperties": false
    });
    let rejecting = CompileOptions {
        unresolved_refs: UnresolvedRefs::Reject,
        ..CompileOptions::default()
    };
    assert!(check_with(&props, &rejecting, &json!({ "count": 2 })));
    assert!(!check_with(
        &props,
        &rejecting,
        &json!({ "count": 2, "extra": 1 })
    ));
    assert_eq!(
        errors_with(&props, &rejecting, &json!({ "count": 2, "extra": 1 })),
        vec![error("/extra", "unevaluatedProperties", "unknown field")]
    );
    let items = json!({
        "$id": "schemas/list.json",
        "type": "array",
        "prefixItems": [{ "$ref": "#/$defs/count" }],
        "$defs": { "count": { "type": "integer" } },
        "unevaluatedItems": false
    });
    assert!(check_with(&items, &rejecting, &json!([2])));
    assert!(!check(&items, &json!([2, 3])));
}

#[test]
fn fails_for_a_malformed_schema() {
    let error = compile(
        &json!({ "properties": { "a": 1 } }),
        &CompileOptions::default(),
    )
    .expect_err("malformed schema");
    assert!(
        error.message().contains("must be an object or a boolean"),
        "unexpected error: {error}"
    );
}

#[test]
fn unresolved_references_fail_when_the_schema_compiles_by_default() {
    let dangling =
        json!({ "type": "object", "properties": { "t": { "$ref": "#/definitions/Missing" } } });
    let external = json!({
        "type": "object",
        "properties": { "t": { "$ref": "https://example.com/schemas/t.json" } }
    });
    for (schema, expected) in [
        (&dangling, "Unresolved $ref #/definitions/Missing"),
        (
            &external,
            "Unresolved $ref https://example.com/schemas/t.json",
        ),
    ] {
        let error = compile(schema, &CompileOptions::default()).expect_err("unresolved $ref");
        // 上游用 `toThrow(string)`，那是“包含”，不是整句相等。
        assert!(
            error.message().contains(expected),
            "unexpected error: {error}"
        );
    }
}

#[test]
fn unresolved_references_reject_only_values_at_their_place() {
    let dangling =
        json!({ "type": "object", "properties": { "t": { "$ref": "#/definitions/Missing" } } });
    let external = json!({
        "type": "object",
        "properties": { "t": { "$ref": "https://example.com/schemas/t.json" } }
    });
    let rejecting = CompileOptions {
        unresolved_refs: UnresolvedRefs::Reject,
        ..CompileOptions::default()
    };
    for schema in [&dangling, &external] {
        assert!(check_with(schema, &rejecting, &json!({})));
        assert!(!check_with(schema, &rejecting, &json!({ "t": 1 })));
    }
    assert_eq!(
        errors_with(&dangling, &rejecting, &json!({ "t": 1 })),
        vec![error(
            "/t",
            "$ref",
            "unresolved reference #/definitions/Missing"
        )]
    );
}

#[test]
fn leaves_unresolved_reference_values_uncoerced() {
    // 上游 `leave values at their place uncoerced`：指空的那一处 `$ref` 无论怎样都不纠正。
    let dangling =
        json!({ "type": "object", "properties": { "t": { "$ref": "#/definitions/Missing" } } });
    let coercing = CompileOptions {
        unresolved_refs: UnresolvedRefs::Reject,
        coerce: true,
        ..CompileOptions::default()
    };
    assert_eq!(
        compile(&dangling, &coercing)
            .expect("compile")
            .parse(&json!({ "t": "1" }))
            .expect("parse"),
        ParseResult::Errors {
            errors: vec![error(
                "/t",
                "$ref",
                "unresolved reference #/definitions/Missing"
            )]
        }
    );
}

#[test]
fn counts_string_length_in_code_points() {
    // 上游的孤立代理项用例在 Rust 里没有对应值（非 UTF-8），这里留下增补平面的那一半。
    let two = json!({ "type": "string", "minLength": 2 });
    let one = json!({ "type": "string", "maxLength": 1 });
    assert!(check(&one, &json!("😀")));
    assert!(!check(&two, &json!("😀")));
    assert!(check(&two, &json!("😀A")));
}

// ---------------------------------------------------------------- errors（上游 describe "errors"）

/// 上游 “publishes errors”：每条错误报在它不合格的那个值上，字段名与文字照抄。
#[test]
fn reports_a_missing_field_and_an_unknown_field_at_the_field() {
    assert_eq!(
        errors(&read(), &json!({ "offset": 0, "extra": true })),
        vec![
            error("/path", "required", "missing"),
            error("/offset", "minimum", "expected >= 1"),
            error("/extra", "additionalProperties", "unknown field"),
        ]
    );
}

#[test]
fn is_empty_for_a_valid_value() {
    assert_eq!(errors(&read(), &json!({ "path": "a.ts" })), vec![]);
}

#[test]
fn reports_the_branch_a_tagged_unions_tag_selects() {
    let schema = json!({ "type": "array", "items": block() });
    let validator = compiled(&schema, &CompileOptions::default());
    let errors = |value: &Value| {
        validator
            .errors(value)
            .unwrap_or_else(|error| panic!("errors of {value}: {error}"))
    };
    assert_eq!(
        errors(&json!([
            { "kind": "text", "text": "a" },
            { "kind": "image", "width": -1 }
        ])),
        vec![error("/1/width", "minimum", "expected >= 0")]
    );
    assert_eq!(
        errors(&json!([{ "kind": "video" }])),
        vec![error(
            "/0/kind",
            "anyOf",
            "expected one of [\"text\",\"image\"]"
        )]
    );
}

#[test]
fn escapes_tilde_and_slash_in_paths_as_json_pointer_does() {
    let schema = json!({ "type": "object", "properties": { "a/b~c": { "type": "string" } } });
    assert_eq!(
        errors(&schema, &json!({ "a/b~c": 1 })),
        vec![error("/a~1b~0c", "type", "expected string, got integer")]
    );
}

#[test]
fn follows_ref_into_the_referenced_schema() {
    let schema = json!({
        "$defs": { "point": { "type": "object", "properties": { "x": { "type": "number" } }, "required": ["x"] } },
        "type": "array",
        "items": { "$ref": "#/$defs/point" }
    });
    assert_eq!(
        errors(&schema, &json!([{ "x": 1 }, {}])),
        vec![error("/1/x", "required", "missing")]
    );
}

// ---------------------------------------------------------------- schemas

#[test]
fn never_modifies_a_schema() {
    // 上游这条还顺带测了 deep-frozen 的 schema：Rust 的值默认就不可变，这里只留“编译和解释都不改它”。
    let mut schema = block();
    schema["$defs"] = json!({ "s": { "type": "string", "pattern": "^a" } });
    let before = schema.clone();
    let validator = compiled(&schema, &CompileOptions::default());
    assert!(validator.check(&json!({ "kind": "text", "text": "x" })));
    assert_eq!(
        validator
            .errors(&json!({ "kind": "image", "width": -1 }))
            .expect("errors")
            .len(),
        1
    );
    assert_eq!(schema, before, "compiling and explaining must not touch it");
}

#[test]
fn explains_errors_under_nested_resources_with_a_relative_id() {
    let cloned = json!({
        "$id": "https://example.com/root.json",
        "allOf": [{ "$ref": "#/$defs/child" }],
        "$defs": {
            "child": {
                "$id": "schemas/tool.json",
                "$ref": "#/$defs/props",
                "unevaluatedProperties": false,
                "$defs": { "props": { "properties": { "count": { "type": "integer" } } } }
            }
        }
    });
    assert_eq!(
        errors(&cloned, &json!({ "count": 2, "extra": 1 })),
        vec![error("/extra", "unevaluatedProperties", "unknown field")]
    );
    let branch = json!({
        "$id": "https://example.com/root.json",
        "anyOf": [
            { "$id": "a/b.json", "$ref": "#/$defs/n", "$defs": { "n": { "type": "integer" } } },
            { "type": "boolean" }
        ]
    });
    assert_eq!(
        errors(&branch, &json!({})),
        vec![error("", "type", "expected integer, got object")]
    );
}

// ---------------------------------------------------------------- error messages（上游 describe "error messages"）

#[test]
fn lists_at_most_20_enum_values() {
    let values: Vec<Value> = (0..5000).map(|i| json!(format!("v{i}"))).collect();
    let schema =
        json!({ "type": "object", "properties": { "e": { "type": "string", "enum": values } } });
    let validator = compiled(&schema, &CompileOptions::default());
    assert!(validator.check(&json!({ "e": "v4999" })));
    let listed: Vec<String> = (0..20).map(|i| format!("\"v{i}\"")).collect();
    // 上游把 `JSON.stringify` 的末位 `]` 切掉再补一句：`"v19"` 后面的引号还在。
    let listed = format!("[{}]", listed.join(","));
    assert_eq!(
        validator.errors(&json!({ "e": "v5000" })).expect("errors")[0].message,
        format!(
            "expected one of {}, ... and 4980 more]",
            &listed[..listed.len() - 1]
        )
    );
    assert_eq!(
        errors(&json!({ "enum": ["a", "b"] }), &json!("c")),
        vec![error("", "enum", "expected one of [\"a\",\"b\"]")]
    );
}

#[test]
fn accepts_a_10000_value_enum() {
    let values: Vec<Value> = (0..10_000).map(|i| json!(format!("member{i}"))).collect();
    assert!(check(&json!({ "enum": values }), &json!("member9999")));
}

#[test]
fn describes_contains_bounds_in_words() {
    // 上游的 `at({type: "array", ...schema})`：数组外层加 contains，取第一条消息。
    let at = |schema: Value| {
        let mut outer = serde_json::Map::new();
        outer.insert("type".to_string(), json!("array"));
        for (key, value) in schema.as_object().expect("an object") {
            outer.insert(key.clone(), value.clone());
        }
        errors(&Value::Object(outer), &json!(["a"]))
            .first()
            .map(|error| error.message.clone())
    };
    assert_eq!(
        at(json!({ "contains": { "type": "number" } })),
        Some("expected at least 1 matching item".to_string())
    );
    assert_eq!(
        at(json!({ "contains": { "type": "number" }, "minContains": 2 })),
        Some("expected at least 2 matching items".to_string())
    );
    assert_eq!(
        at(json!({ "contains": { "type": "string" }, "maxContains": 0, "minContains": 0 })),
        Some("expected at most 0 matching items".to_string())
    );
    assert_eq!(
        at(json!({ "contains": { "type": "number" }, "minContains": 1, "maxContains": 3 })),
        Some("expected between 1 and 3 matching items".to_string())
    );
}

#[test]
fn blames_a_oneof_failure_on_the_branch_that_declares_most_of_the_objects_properties() {
    let filter = json!({
        "type": "object",
        "properties": { "where": { "$ref": "#/$defs/F" } },
        "$defs": {
            "F": {
                "oneOf": [
                    {
                        "type": "object",
                        "properties": { "field": { "type": "string" }, "eq": { "type": "string" } },
                        "required": ["field", "eq"],
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "properties": { "and": { "type": "array", "items": { "$ref": "#/$defs/F" } } },
                        "required": ["and"],
                        "additionalProperties": false
                    }
                ]
            }
        }
    });
    assert_eq!(
        errors(
            &filter,
            &json!({ "where": { "and": [{ "field": "a", "eq": 1, "x": 2 }] } })
        ),
        vec![
            error("/where/and/0/eq", "type", "expected string, got integer"),
            error("/where/and/0/x", "additionalProperties", "unknown field"),
        ]
    );
}

#[test]
fn explains_an_object_reached_through_the_same_reference_at_each_path() {
    // 上游这条里两个属性指同一个对象（JS 能共享，JSON 只能写成两份）。解释器对同一个 `$ref` 目标
    // 记的结果按路径分开用，两种写法结论相同；共用身份那一路由 `port_compare` 的对账用例守住。
    let schema = json!({
        "type": "object",
        "properties": { "a": { "$ref": "#/$defs/P" }, "b": { "$ref": "#/$defs/P" } },
        "$defs": { "P": { "type": "object", "properties": { "n": { "type": "integer" } } } }
    });
    assert_eq!(
        errors(&schema, &json!({ "a": { "n": "x" }, "b": { "n": "x" } })),
        vec![
            error("/a/n", "type", "expected integer, got string"),
            error("/b/n", "type", "expected integer, got string"),
        ]
    );
}

// ---------------------------------------------------------------- 值（上游 describe "values"）

/// 上游 “treats a property set to undefined as absent”：JSON 里没有 `undefined`，对应的一面是缺席。
#[test]
fn treats_a_missing_property_as_absent() {
    // 上游还有 `check({ path: undefined })`，那是同一个值在 JS 里的另一种写法。
    assert!(check(&read(), &json!({ "path": "a.ts" })));
    assert!(!check(&read(), &json!({})));
}

// ---------------------------------------------------------------- schemas（上游 describe "schemas"）

/// 上游 “compiles TypeBox schemas as they are”：`Type.Object({ name: Type.String(), tags:
/// Type.Array(Type.String()) })` 写出来就是下面这份 JSON（非可选属性进 `required`）。
#[test]
fn compiles_typebox_schemas_as_they_are() {
    let schema = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "tags": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["name", "tags"]
    });
    assert!(check(&schema, &json!({ "name": "a", "tags": [] })));
    assert_eq!(
        errors(&schema, &json!({ "name": "a", "tags": [1] })),
        vec![error("/tags/0", "type", "expected string, got integer")]
    );
}

// ---------------------------------------------------------------- unresolved references

/// 上游 “reject a TypeBox Ref without its definition”：`Type.Object({ a: Type.Ref("A") })`。
#[test]
fn rejects_a_typebox_ref_without_its_definition() {
    let schema = json!({
        "type": "object",
        "properties": { "a": { "$ref": "A" } },
        "required": ["a"]
    });
    let rejecting = CompileOptions {
        unresolved_refs: UnresolvedRefs::Reject,
        ..CompileOptions::default()
    };
    assert_eq!(
        errors_with(&schema, &rejecting, &json!({ "a": { "x": "1" } })),
        vec![error("/a", "$ref", "unresolved reference A")]
    );
}

// ---------------------------------------------------------------- leaf checks（上游 describe "leaf checks inlined by object and array loops"）

/// 上游 “agree with the standalone check as a property, a pattern property and an item”：同一张叶子
/// 检查表放进闭合对象、开放对象和数组，结论必须与单独用那一个检查器一样。
///
/// 上游表里的 `Uint8Array` 两个值在 JSON 里没有对应值（见 `PORTING.md` 差表），其余照搬。
#[test]
fn leaf_checks_agree_as_a_property_a_pattern_property_and_an_item() {
    let leaves: Vec<(Value, Vec<Value>)> = vec![
        (
            json!({ "const": "a" }),
            vec![json!("a"), json!("b"), json!(1), Value::Null],
        ),
        (
            json!({ "const": null }),
            vec![Value::Null, json!(0), json!(false), json!("null")],
        ),
        (
            json!({ "enum": ["a", 1, null] }),
            vec![
                json!("a"),
                json!(1),
                Value::Null,
                json!("b"),
                json!(2),
                json!(true),
            ],
        ),
        (
            json!({ "type": ["string", "null"] }),
            vec![json!("a"), Value::Null, json!(1), json!({}), json!([])],
        ),
        (
            json!({ "type": ["integer", "boolean"] }),
            vec![json!(1), json!(1.5), json!(true), json!("1")],
        ),
        (
            json!({ "type": ["number", "array"] }),
            vec![json!(1.5), json!(2), json!([]), json!({})],
        ),
        (
            json!({ "type": ["object", "bytes"] }),
            vec![json!({}), json!([]), Value::Null],
        ),
        (
            json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] }),
            vec![json!("a"), Value::Null, json!(1)],
        ),
        (
            json!({ "anyOf": [{ "type": "integer" }, { "type": "string", "minLength": 2 }] }),
            vec![json!(1), json!(1.5), json!("ab"), json!("a")],
        ),
        (
            json!({ "type": "integer", "minimum": 1, "exclusiveMaximum": 5 }),
            vec![json!(1), json!(4), json!(5), json!(0), json!(2.5), json!("2")],
        ),
        (
            json!({ "type": "number", "exclusiveMinimum": 0, "maximum": 1 }),
            vec![json!(0.5), json!(1), json!(0), json!(1.5), json!("0.5")],
        ),
        (
            json!({ "type": "integer", "multipleOf": 2 }),
            vec![json!(2), json!(3), json!(2.5)],
        ),
        (
            json!({ "type": "string", "minLength": 2, "maxLength": 3 }),
            vec![
                json!("ab"),
                json!("abc"),
                json!("a"),
                json!("abcd"),
                json!("😀😀"),
                json!(12),
            ],
        ),
        (
            json!({ "type": "string", "pattern": "^a+$" }),
            vec![json!("aa"), json!("ab"), json!(1)],
        ),
        (json!({ "minimum": 1 }), vec![json!(1), json!(0), json!("x"), Value::Null]),
    ];

    for (schema, values) in leaves {
        let alone = compiled(&schema, &CompileOptions::default()).check_fn();
        let closed = compiled(
            &json!({ "type": "object", "properties": { "p": schema }, "additionalProperties": false }),
            &CompileOptions::default(),
        )
        .check_fn();
        let open = compiled(
            &json!({ "type": "object", "properties": { "p": schema }, "required": ["q"] }),
            &CompileOptions::default(),
        )
        .check_fn();
        let items = compiled(
            &json!({ "type": "array", "items": schema }),
            &CompileOptions::default(),
        )
        .check_fn();
        for value in values {
            let expected = alone(&value);
            let label = format!("{schema} against {value}");
            assert_eq!(closed(&json!({ "p": value })), expected, "{label}: closed object");
            assert_eq!(
                open(&json!({ "p": value, "q": 1 })),
                expected,
                "{label}: open object"
            );
            assert_eq!(items(&json!([value])), expected, "{label}: item");
        }
    }
}

/// 上游 “find properties in any order”。
#[test]
fn finds_properties_in_any_order() {
    let schema = json!({
        "type": "object",
        "properties": {
            "a": { "const": 1 },
            "b": { "type": "string" },
            "c": { "type": "integer", "minimum": 0 }
        },
        "required": ["a", "c"],
        "additionalProperties": false
    });
    let check = compiled(&schema, &CompileOptions::default()).check_fn();
    assert!(check(&json!({ "a": 1, "b": "x", "c": 0 })));
    assert!(check(&json!({ "c": 0, "b": "x", "a": 1 })));
    assert!(check(&json!({ "b": "x", "c": 0, "a": 1 })));
    assert!(!check(&json!({ "a": 1, "c": 0, "a2": 1 })));
    assert!(!check(&json!({ "c": -1, "a": 1 })));
    assert!(!check(&json!({ "c": 0, "b": 1, "a": 1 })));
    // 上游这一行是 `{ c: 0, b: undefined, a: 1 }`：JSON 里没有 `undefined`，写出来就是两项。
    assert!(check(&json!({ "c": 0, "a": 1 })));
    assert!(!check(&json!({ "c": 0, "b": "x" })));
}

// ---------------------------------------------------------------- TypeBox's extra formats

/// 上游 “check url and json-pointer-uri-fragment”。
#[test]
fn checks_url_and_json_pointer_uri_fragment() {
    let options = CompileOptions {
        formats: Some(formats().clone()),
        ..CompileOptions::default()
    };
    let url = json!({ "type": "string", "format": "url" });
    assert!(check_with(&url, &options, &json!("https://example.com/a?b=1")));
    assert!(!check_with(&url, &options, &json!("not a url")));
    let fragment = json!({ "type": "string", "format": "json-pointer-uri-fragment" });
    assert!(check_with(
        &fragment,
        &options,
        &json!("#/a/b~1c")
    ));
    assert!(!check_with(&fragment, &options, &json!("#/a b")));
    assert!(!check_with(&fragment, &options, &json!("/a")));
}
