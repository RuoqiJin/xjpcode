//! 对应上游 `packages/schema/src/annotations.ts`。
//!
//! `unevaluatedProperties` 和 `unevaluatedItems` 的注解追踪：标注型检查在通过时记下它评过哪些属性和
//! 哪些位置的项。只有用到这两个关键字的 schema 才会编译成这些闭包。

use std::collections::HashSet;
use std::sync::Arc;

use regress::Regex;
use serde_json::Value;

use crate::checks::{enter_ref, enter_scope, is_object, Slot};
use crate::types::Check;

/// 一个 schema 评过什么：属性名（或全部属性）、评过的项前缀、`contains` 命中的项。
#[derive(Debug, Clone, Default)]
pub struct Annotations {
    pub props: Option<HashSet<String>>,
    pub all_props: bool,
    pub items: usize,
    pub all_items: bool,
    pub indexes: Option<HashSet<usize>>,
}

/// 通过时把评过的东西记进 `a` 的检查。
pub type AnnotatingCheck = Arc<dyn Fn(&Value, &mut Annotations) -> bool + Send + Sync>;

/// 一个标注型检查的间接槽。
pub type AnnotatingSlot = Slot<dyn Fn(&Value, &mut Annotations) -> bool + Send + Sync>;

/// 空白注解。
pub fn new_annotations() -> Annotations {
    Annotations::default()
}

fn merge(a: &mut Annotations, b: Annotations) {
    if let Some(props) = b.props {
        for key in props {
            add_prop(a, key);
        }
    }
    a.all_props = a.all_props || b.all_props;
    if b.items > a.items {
        a.items = b.items;
    }
    a.all_items = a.all_items || b.all_items;
    if let Some(indexes) = b.indexes {
        for index in indexes {
            add_index(a, index);
        }
    }
}

fn add_prop(a: &mut Annotations, key: String) {
    a.props.get_or_insert_with(HashSet::new).insert(key);
}

fn add_index(a: &mut Annotations, index: usize) {
    a.indexes.get_or_insert_with(HashSet::new).insert(index);
}

/// 始终通过（`true` schema）。
pub fn annotating_true() -> AnnotatingCheck {
    Arc::new(|_: &Value, _: &mut Annotations| true)
}

/// 始终不通过（`false` schema）。
pub fn annotating_false() -> AnnotatingCheck {
    Arc::new(|_: &Value, _: &mut Annotations| false)
}

/// 递归引用走的那层间接。
pub fn indirect_annotating(slot: Arc<AnnotatingSlot>) -> AnnotatingCheck {
    let id = Arc::as_ptr(&slot) as *const () as usize;
    Arc::new(move |v: &Value, a: &mut Annotations| {
        let Some(check) = slot.get() else {
            return false;
        };
        let Some(_in_flight) = enter_ref(id, v) else {
            return false;
        };
        check(v, a)
    })
}

/// 处在标注位置的普通检查：它不评任何 `unevaluated*` 看得见的东西。
pub fn ignore_annotations(check: Check) -> AnnotatingCheck {
    Arc::new(move |v: &Value, _: &mut Annotations| check(v))
}

/// 把标注型检查当普通检查跑，每次带一份新注解。
pub fn with_fresh_annotations(check: AnnotatingCheck) -> Check {
    Arc::new(move |v: &Value| {
        let mut a = new_annotations();
        check(v, &mut a)
    })
}

/// `check` 运行期间把 `uri` 压到动态作用域上（只有带 `$dynamicRef` 的 schema 会用到）。
pub fn scoped_annotating(uri: Arc<str>, check: AnnotatingCheck) -> AnnotatingCheck {
    Arc::new(move |v: &Value, a: &mut Annotations| {
        let _guard = enter_scope(uri.clone());
        check(v, a)
    })
}

/// `unevaluated*` 只看它自己 schema 的注解，从不要父级兄弟的。
pub fn isolate_annotations(check: AnnotatingCheck) -> AnnotatingCheck {
    Arc::new(move |v: &Value, a: &mut Annotations| {
        let mut b = new_annotations();
        if !check(v, &mut b) {
            return false;
        }
        merge(a, b);
        true
    })
}

/// 先跑不标注的关键字，再按顺序跑每个标注部分。
pub fn annotate_all(plain: Check, parts: Vec<AnnotatingCheck>) -> AnnotatingCheck {
    Arc::new(move |v: &Value, a: &mut Annotations| {
        if !plain(v) {
            return false;
        }
        for part in &parts {
            if !part(v, a) {
                return false;
            }
        }
        true
    })
}

/// `properties`、`patternProperties` 和 `additionalProperties`。
pub fn annotate_object(
    props: Vec<(String, Check)>,
    patterns: Vec<(Arc<Regex>, Check)>,
    additional: Option<Check>,
) -> AnnotatingCheck {
    let declared: HashSet<String> = props.iter().map(|(key, _)| key.clone()).collect();
    Arc::new(move |v: &Value, a: &mut Annotations| {
        if !is_object(v) {
            return true;
        }
        let Some(object) = v.as_object() else {
            return true;
        };
        let mut seen: Vec<String> = Vec::new();
        for (key, check) in &props {
            let Some(value) = object.get(key) else {
                continue;
            };
            if !check(value) {
                return false;
            }
            seen.push(key.clone());
        }
        for (key, value) in object {
            let mut matched = declared.contains(key);
            for (re, check) in &patterns {
                if re.find(key).is_some() {
                    if !check(value) {
                        return false;
                    }
                    matched = true;
                    seen.push(key.clone());
                }
            }
            if !matched {
                if let Some(additional) = additional.as_ref() {
                    if !additional(value) {
                        return false;
                    }
                    seen.push(key.clone());
                }
            }
        }
        for key in seen {
            add_prop(a, key);
        }
        true
    })
}

/// `prefixItems`/`items`/`additionalItems` 与带界的 `contains`。
pub fn annotate_array(
    prefix: Vec<Check>,
    rest: Option<Check>,
    contains: Option<Check>,
    min_contains: f64,
    max_contains: f64,
) -> AnnotatingCheck {
    Arc::new(move |v: &Value, a: &mut Annotations| {
        let Some(list) = v.as_array() else {
            return true;
        };
        let common = list.len().min(prefix.len());
        for i in 0..common {
            if !(prefix[i])(&list[i]) {
                return false;
            }
        }
        if let Some(rest) = rest.as_ref() {
            for item in &list[prefix.len().min(list.len())..] {
                if !rest(item) {
                    return false;
                }
            }
            a.all_items = true;
        }
        if common > a.items {
            a.items = common;
        }
        if let Some(contains) = contains.as_ref() {
            let mut n = 0f64;
            for (i, item) in list.iter().enumerate() {
                if contains(item) {
                    n += 1.0;
                    add_index(a, i);
                }
            }
            if n < min_contains || n > max_contains {
                return false;
            }
        }
        true
    })
}

/// `anyOf`（`exactly_one` 时是 `oneOf`）：每个通过的分支都贡献自己的注解。
pub fn annotate_any(branches: Vec<AnnotatingCheck>, exactly_one: bool) -> AnnotatingCheck {
    Arc::new(move |v: &Value, a: &mut Annotations| {
        let mut n = 0;
        for branch in &branches {
            let mut b = new_annotations();
            if branch(v, &mut b) {
                n += 1;
                merge(a, b);
            }
        }
        if exactly_one {
            n == 1
        } else {
            n > 0
        }
    })
}

/// `if`、`then` 和 `else`：通过的 `if` 即使没有 `then` 也贡献自己的注解。
pub fn annotate_if(
    cond: AnnotatingCheck,
    then: Option<AnnotatingCheck>,
    otherwise: Option<AnnotatingCheck>,
) -> AnnotatingCheck {
    Arc::new(move |v: &Value, a: &mut Annotations| {
        let mut b = new_annotations();
        if cond(v, &mut b) {
            merge(a, b);
            return then.as_ref().is_none_or(|then| then(v, a));
        }
        otherwise.as_ref().is_none_or(|otherwise| otherwise(v, a))
    })
}

/// 一个 `dependentSchemas` 条目。
pub fn annotate_dependent(key: String, check: AnnotatingCheck) -> AnnotatingCheck {
    Arc::new(move |v: &Value, a: &mut Annotations| {
        let present = v
            .as_object()
            .is_some_and(|object| object.contains_key(&key));
        !present || check(v, a)
    })
}

/// `unevaluatedProperties`：其它关键字都没评过的属性都要通过 `check`。
pub fn unevaluated_properties(check: Check) -> AnnotatingCheck {
    Arc::new(move |v: &Value, a: &mut Annotations| {
        let Some(object) = v.as_object() else {
            return true;
        };
        if a.all_props {
            return true;
        }
        for (key, value) in object {
            let evaluated = a.props.as_ref().is_some_and(|props| props.contains(key));
            if !evaluated && !check(value) {
                return false;
            }
        }
        a.all_props = true;
        true
    })
}

/// `unevaluatedItems`：其它关键字都没评过的项都要通过 `check`。
pub fn unevaluated_items(check: Check) -> AnnotatingCheck {
    Arc::new(move |v: &Value, a: &mut Annotations| {
        let Some(list) = v.as_array() else {
            return true;
        };
        if a.all_items {
            return true;
        }
        for (i, item) in list.iter().enumerate().skip(a.items) {
            let evaluated = a
                .indexes
                .as_ref()
                .is_some_and(|indexes| indexes.contains(&i));
            if !evaluated && !check(item) {
                return false;
            }
        }
        a.all_items = true;
        true
    })
}
