//! 递归引用的两个语言差异，上游没有对应测试。
//!
//! 上游 `packages/schema/` 里没有测过 `$ref` 绕回自己的 schema，也没有测过编译出来的检查器能不能被
//! 回收：JS 的垃圾回收顺着引用把环收掉，两件事都自然成立。Rust 侧两条都成立不了——无限递归会让
//! 整个进程 abort，`Arc` 的环永远收不掉——所以这里各自看住（见 `PORTING.md` 差异表）。
//!
//! 每个用例的对照结论都用 node 跑过上游同一份源码：值能走完的，两边结论逐条相同；走不完的，上游抛
//! `RangeError: Maximum call stack size exceeded`。

use std::sync::Arc;
use std::thread;

use serde_json::{json, Value};
use xjpcode_schema::types::{CompileOptions, Draft, JsonSchema};
use xjpcode_schema::{compile, Validator};

fn compiled(schema: &JsonSchema, options: &CompileOptions) -> Validator {
    compile(schema, options).unwrap_or_else(|error| panic!("compile {schema}: {error}"))
}

fn check(schema: &JsonSchema, value: &Value) -> bool {
    compiled(schema, &CompileOptions::default()).check(value)
}

/// 一层套一层的树：递归经过 `children` 的项，每下一层值都小一点。
fn tree() -> Value {
    json!({
        "type": "object",
        "properties": {
            "value": { "type": "number" },
            "children": { "type": "array", "items": { "$ref": "#" } }
        },
        "required": ["value"]
    })
}

#[test]
fn rejects_a_ref_chain_that_only_points_back_at_itself() {
    // 上游对 `{"$ref":"#"}` 编译得过，检查任意值都抛 `RangeError`：这份 schema 的检查就是它自己。
    // Rust 里同样的无限递归会让进程 abort（不是能接住的错），所以挪到编译期报错。
    for schema in [
        json!({ "$ref": "#" }),
        json!({ "$ref": "#/$defs/a", "$defs": { "a": { "$ref": "#/$defs/a" } } }),
        json!({
            "$ref": "#/$defs/a",
            "$defs": { "a": { "$ref": "#/$defs/b" }, "b": { "$ref": "#/$defs/a" } }
        }),
        json!({ "$ref": "#/$defs/a", "$defs": { "a": { "$ref": "#" } } }),
    ] {
        let error = compile(&schema, &CompileOptions::default()).expect_err("pure $ref cycle");
        assert!(
            error.message().contains("Unresolvable $ref cycle"),
            "unexpected error for {schema}: {error}"
        );
    }

    // draft 7 里 `$ref` 让兄弟关键字失效，带 `$ref` 的 schema 就是一次转发：加了 `type` 照样成环。
    let draft7 = CompileOptions {
        draft: Draft::Draft7,
        ..CompileOptions::default()
    };
    let error = compile(&json!({ "$ref": "#", "type": "object" }), &draft7)
        .expect_err("draft 7 pure $ref cycle");
    assert!(
        error.message().contains("Unresolvable $ref cycle"),
        "unexpected error: {error}"
    );

    // 环里有一次取值下降就照常编译：`$defs/node` 进 `properties.next`，每层换一个值。
    let recursing = json!({
        "$ref": "#/$defs/node",
        "$defs": {
            "node": {
                "type": "object",
                "properties": { "value": { "type": "number" }, "next": { "$ref": "#/$defs/node" } },
                "required": ["value"]
            }
        }
    });
    assert!(check(
        &recursing,
        &json!({ "value": 1, "next": { "value": 2 } })
    ));
    assert!(!check(&recursing, &json!({ "value": 1, "next": {} })));
}

#[test]
fn frees_a_recursive_schemas_checks_when_its_validator_is_dropped() {
    // 上游的槽直接持有检查器本身，环由 JS 的垃圾回收收掉；Rust 的 `Arc` 环永远收不掉，所以槽里只装
    // 弱引用（见差异表）。这里用弱引用证明：根检查器一丢，整棵检查树就没了。
    let unevaluated = json!({
        "type": "object",
        "properties": { "value": { "type": "number" }, "next": { "$ref": "#" } },
        "unevaluatedProperties": false
    });
    for (schema, good, bad) in [
        (
            tree(),
            json!({ "value": 1, "children": [{ "value": 2 }] }),
            json!({ "value": 1, "children": [{}] }),
        ),
        (
            unevaluated,
            json!({ "value": 1, "next": { "value": 2 } }),
            json!({ "value": 1, "next": { "extra": 1 } }),
        ),
    ] {
        let validator = compiled(&schema, &CompileOptions::default());
        let check = validator.check_fn();
        // 还活着的时候照常能查：弱引用不能让递归的那一侧变成“什么都不合格”。
        assert!(check(&good), "unexpected fail for {schema} on {good}");
        assert!(!check(&bad), "unexpected pass for {schema} on {bad}");

        let weak = Arc::downgrade(&check);
        drop(validator);
        drop(check);
        assert!(
            weak.upgrade().is_none(),
            "dropping the validator must free the whole check tree ({schema})"
        );
    }
}

#[test]
fn stops_a_cycle_it_cannot_see_at_compile_time_instead_of_looping() {
    // 这些形状里环外面还裹着别的关键字，编译期分不出“检查时会不会走进来”；上游在检查的那一步抛
    // `RangeError`。Rust 的检查器只回布尔，改为“不合格”：进程不能因为一份 schema 就 abort。
    for (schema, value) in [
        (
            json!({ "allOf": [{ "$ref": "#" }, { "$ref": "#" }] }),
            json!(1),
        ),
        (
            json!({ "type": "string", "allOf": [{ "$ref": "#" }] }),
            json!("a"),
        ),
        (json!({ "not": { "$ref": "#" } }), json!(1)),
        (
            json!({ "anyOf": [{ "type": "string" }, { "$ref": "#" }] }),
            json!(1),
        ),
        (
            json!({ "dependentSchemas": { "a": { "$ref": "#" } } }),
            json!({ "a": 1 }),
        ),
    ] {
        assert!(!check(&schema, &value), "unexpected pass for {schema}");
    }

    // 环之前的那个分支有结论时不受影响，与上游相同（上游这份 schema 查 `"a"` 也回 true）。
    let any_of = json!({ "anyOf": [{ "type": "string" }, { "$ref": "#" }] });
    assert!(check(&any_of, &json!("a")));

    // 守卫在第一次绕回来时就停，不吃栈：这份 schema 在 256 KB 的线程上也不会把栈用光。
    let vicious = json!({ "allOf": [{ "$ref": "#" }, { "$ref": "#" }] });
    let handled = thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || check(&vicious, &json!(1)))
        .expect("spawn")
        .join()
        .expect("the guard must not blow the stack");
    assert!(!handled);
}

#[test]
fn checks_a_deep_value_through_a_recursive_ref() {
    // 守卫按“同一个引用、同一个值”判环，不按深度：往下走一层就换一个值，200 层照样查得完
    // （上游同样回 true）。
    let mut deep = json!({ "value": 200 });
    for level in (1..200).rev() {
        deep = json!({ "value": level, "children": [deep] });
    }
    let schema = tree();
    assert!(check(&schema, &deep));

    // 最深的那一层不合格，错误要能传上来。
    let mut bad = json!({});
    for level in (0..200).rev() {
        bad = json!({ "value": level, "children": [bad] });
    }
    assert!(!check(&schema, &bad));
}

#[test]
fn stops_the_explainer_in_a_cycle_instead_of_blowing_the_stack() {
    // 这些 schema 的环编译期看不出来，检查期靠守卫回“不合格”。解释失败时要沿同一批引用再走一遍：
    // 上游在那里同样会无限递归（`RangeError`），Rust 侧走到同一个“引用目标 + 值”第二次就停
    // （见 `PORTING.md` 差异表），所以结论一定收敛。
    for (schema, value) in [
        (
            json!({ "allOf": [{ "$ref": "#" }, { "$ref": "#" }] }),
            json!(1),
        ),
        (json!({ "not": { "$ref": "#" } }), json!(1)),
        (
            json!({ "anyOf": [{ "type": "string" }, { "$ref": "#" }] }),
            json!(1),
        ),
        (
            json!({ "dependentSchemas": { "a": { "$ref": "#" } } }),
            json!({ "a": 1 }),
        ),
    ] {
        // 256 KB 的栈，与 `stops_a_cycle_it_cannot_see_at_compile_time_instead_of_looping` 同一把尺子：
        // 守卫要是在环上多吃一层就溢出。
        let errors = thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                let validator = compiled(&schema, &CompileOptions::default());
                validator.errors(&value)
            })
            .expect("spawn")
            .join()
            .expect("the guard must not blow the stack")
            .unwrap_or_else(|error| panic!("errors: {error}"));
        assert!(
            !errors.is_empty(),
            "a value that failed the check must still get at least one error"
        );
    }

    // 守卫按“同一个引用、同一个值”判环：往下走一层就换一个值，深值照样一层层解释完。解释每层要走
    // 几个栈帧，所以要的栈跟着深度走（上游同样如此：它在那里抛 `RangeError`）；128 层是
    // `serde_json` 解析出来的值最深能有的层数。
    let mut bad = json!({});
    for level in (0..128).rev() {
        bad = json!({ "value": level, "children": [bad] });
    }
    let errors = compiled(&tree(), &CompileOptions::default())
        .errors(&bad)
        .expect("errors");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].keyword, "required");
    // 最深那层的 `value` 缺了，错误报在它身上。
    assert_eq!(errors[0].path, "/children/0".repeat(128) + "/value");
}
