//! 移植自上游 `packages/schema/test/shared.test.ts`。
//!
//! 上游第二个用例靠 V8 的 gc 与 FinalizationRegistry 证明“正则被回收后缓存条目消失”。Rust 侧
//! 没有对应的回收钩子：缓存持的是 `Weak`，正则被回收时弱引用自然失效，条目在下一次插入时清掉
//! （见 `PORTING.md` 差异表）。因此这里测共享本身，并补上本仓的既定约束：检查器可跨任务共用。

use std::thread;

use serde_json::{json, Value};
use xjpcode_schema::shared::shared_regex_count;
use xjpcode_schema::types::{Check, CompileOptions, JsonSchema};
use xjpcode_schema::{compile, Validator};

fn compiled(schema: &JsonSchema) -> Validator {
    compile(schema, &CompileOptions::default())
        .unwrap_or_else(|error| panic!("compile {schema}: {error}"))
}

#[test]
fn shared_checks() {
    // 上游：`compile({type:"string", description:"a"}).check` 与带 `title` 的那份是同一个函数。
    let a = compiled(&json!({ "type": "string", "description": "a" }));
    let b = compiled(&json!({ "type": "string", "title": "b" }));
    assert!(
        std::sync::Arc::ptr_eq(&a.check_fn(), &b.check_fn()),
        "a bare type's check must be the module-level one"
    );

    // 上游：50 个不同 pattern 进缓存，再编译一次已有 pattern 不会新增条目；最后一个 schema 仍能用它
    // 检查值。（上游接着用 gc 断言其余 49 个被回收，这里用弱引用失效代替。）
    let before = shared_regex_count();
    let mut validators: Vec<Validator> = (0..50)
        .map(|i| compiled(&json!({ "type": "string", "pattern": format!("^unique-{i}$") })))
        .collect();
    let again = compiled(&json!({ "type": "string", "pattern": "^unique-0$" }));
    assert_eq!(shared_regex_count(), before + 50);
    assert!(again.check(&json!("unique-0")));

    validators.clear();
    // 条目已被丢弃的弱引用在下一次插入时清掉：只剩还活着的 ^unique-0$ 和这次新加的。
    let _ = compiled(&json!({ "type": "string", "pattern": "^fresh$" }));
    assert_eq!(shared_regex_count(), before + 2);
}

#[test]
fn compiled_checks_are_shared_between_tasks() {
    // 编译期约束：检查器可以跨线程共用（循环会同时跑几个工具）。
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Check>();
    assert_send_sync::<Validator>();

    let schema: JsonSchema = json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
        "required": ["path"],
        "additionalProperties": false
    });
    let validator = compiled(&schema);
    let check = validator.check_fn();
    let handles: Vec<_> = (0..4)
        .map(|worker| {
            let check = check.clone();
            thread::spawn(move || {
                let good: Value = json!({ "path": "a.ts" });
                let bad: Value = json!({ "path": 1 });
                (0..100).all(|_| check(&good) && !check(&bad)) && worker < 4
            })
        })
        .collect();
    for handle in handles {
        assert!(handle.join().expect("worker thread"));
    }
}
