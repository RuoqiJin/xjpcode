//! 对应上游 `packages/schema/src/explain.ts`。
//!
//! 一个值不合格时说出为什么：在 schema 上再走一遍，把每个不合格的关键字报在它不合格的那个值上。
//! 只在值没通过检查之后才跑，子 schema 的检查直接用编译器现成的。报出来的 `path`/`keyword`/
//! `message` 会原样回给模型看（工具参数不对时），所以文字与上游逐字相同。

use std::cmp::Ordering;
use std::collections::HashMap;

use serde_json::Value;

use crate::annotations::new_annotations;
use crate::checks::{
    code_points, const_check, contains_check, deep_equal, enum_check, is_multiple, is_object,
    never, type_check, unique_check,
};
use crate::compiler::{Compiler, Failure};
use crate::js;
use crate::resolver::is_schema_object;
use crate::shared::shared_regex;
use crate::types::{Check, CompileError, Draft, SchemaObject, ValidationError};

/// 解释一份已编译 schema 的失败。
pub(crate) struct Explainer<'a> {
    c: Compiler<'a>,
    schema: &'a Value,
    /// `$ref` 目标对某个对象的解释结果，一次 `errors()` 之内有效：递归的联合会逐分支解释，每个分支
    /// 又会把同样的子值解释一遍，深度上是指数的。
    memo: Option<HashMap<(usize, usize), Memo>>,
    /// 正在解释的 `$ref` 目标（目标地址、值地址）。同一个目标拿同一个值再进来一次就是环。
    in_flight: Vec<(usize, usize)>,
}

struct Memo {
    path: String,
    errors: Vec<ValidationError>,
}

/// 解释 `unevaluated*` 时看不见的关键字：`b` 已经应用过 `$id`，`unevaluated*` 自己手动应用。
const UNEVALUATED_REST: &[&str] = &["$id", "unevaluatedProperties", "unevaluatedItems"];

impl<'a> Explainer<'a> {
    /// `compiler` 编译的就是 `schema`，且已经编译过根检查器（上游构造时再调一次 `root()`，那一步在
    /// Rust 侧由 `validator::compiler()` 完成）。
    pub(crate) fn new(compiler: Compiler<'a>, schema: &'a Value) -> Self {
        Self {
            c: compiler,
            schema,
            memo: None,
            in_flight: Vec::new(),
        }
    }

    /// `value` 的错误，至少一条（`value` 已经没通过检查）。
    pub(crate) fn errors(&mut self, value: &Value) -> Result<Vec<ValidationError>, CompileError> {
        let mut out: Vec<ValidationError> = Vec::new();
        let walked = self.walk(self.schema, self.c.r.root_base, value, "", &mut out);
        self.memo = None;
        walked?;
        if out.is_empty() {
            out.push(ValidationError::new("", "schema", "invalid"));
        }
        Ok(out)
    }

    fn ok(&mut self, node: &'a Value, base: &str, v: &Value) -> Result<bool, CompileError> {
        Ok(self.check(node, base)?(v))
    }

    fn check(&mut self, node: &'a Value, base: &str) -> Result<Check, CompileError> {
        self.c.check(node, base).map_err(failure_error)
    }

    /// 带 `$dynamicRef` 的 schema 走到这里时，检查已经不在原来的动态作用域里跑了，所以它们的错误是
    /// 近似的（上游注释如此；Rust 侧同理，见 `PORTING.md` 差异表）。
    fn walk(
        &mut self,
        node: &'a Value,
        base: &str,
        v: &Value,
        path: &str,
        out: &mut Vec<ValidationError>,
    ) -> Result<(), CompileError> {
        match node {
            Value::Bool(true) => return Ok(()),
            Value::Bool(false) => {
                out.push(ValidationError::new(
                    path,
                    "false",
                    "no value is allowed here",
                ));
                return Ok(());
            }
            Value::Object(_) => {}
            _ => return Ok(()),
        }
        let object: &'a SchemaObject = node.as_object().expect("object");
        let b = self.c.r.base_for(node, base);
        if self.ok(node, &b, v)? {
            return Ok(());
        }
        let start = out.len();
        // draft 7 里 `$ref` 让兄弟关键字全部失效。
        if self.c.draft == Draft::Draft7 {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                self.walk_ref(reference, &b, v, path, out)?;
                return Ok(());
            }
        }
        if !self.walk_value_keywords(object, v, path, out) {
            return Ok(());
        }
        if is_object(v) {
            self.walk_object(node, &b, v, path, out)?;
        }
        if v.is_array() {
            self.walk_array(node, &b, v, path, out)?;
        }
        self.walk_combinators(node, object, &b, v, path, start, out)
    }

    /// 作用于值本身的关键字：`type`、`enum`、`const` 与字符串、数字上的关键字。
    ///
    /// 与上游一样，`type` 不合格就到此为止（返回 `false`）。拆出来只为让每层递归的栈帧小一点。
    fn walk_value_keywords(
        &mut self,
        object: &'a SchemaObject,
        v: &Value,
        path: &str,
        out: &mut Vec<ValidationError>,
    ) -> bool {
        if let Some(declared) = object.get("type") {
            let types: Vec<&Value> = match declared.as_array() {
                Some(list) => list.iter().collect(),
                None => vec![declared],
            };
            let passes = types.iter().any(|declared| {
                let check = declared.as_str().and_then(type_check).unwrap_or_else(never);
                check(v)
            });
            if !passes {
                let names: Vec<String> = types.iter().map(|declared| join_item(declared)).collect();
                out.push(ValidationError::new(
                    path,
                    "type",
                    format!("expected {}, got {}", names.join(" or "), type_name(v)),
                ));
                return false;
            }
        }
        if let Some(values) = object.get("enum").and_then(Value::as_array) {
            if !enum_check(values.clone())(v) {
                out.push(ValidationError::new(
                    path,
                    "enum",
                    format!("expected one of {}", list_values(values)),
                ));
            }
        }
        if let Some(expected) = object.get("const") {
            if !const_check(expected.clone())(v) {
                out.push(ValidationError::new(
                    path,
                    "const",
                    format!("expected {}", js::stringify(expected)),
                ));
            }
        }
        if let Some(text) = v.as_str() {
            self.walk_string(object, text, path, out);
        }
        // 上游 `v instanceof Uint8Array && node.type === "bytes"` 一分支在 JSON 里没有对应值。
        if let Some(x) = v.as_f64() {
            self.walk_number(object, x, path, out);
        }
        true
    }

    /// 组合子与引用：`allOf`、`anyOf`、`oneOf`、`not`、`if`、`$ref`、`unevaluated*`、`$dynamicRef`。
    fn walk_combinators(
        &mut self,
        node: &'a Value,
        object: &'a SchemaObject,
        b: &str,
        v: &Value,
        path: &str,
        start: usize,
        out: &mut Vec<ValidationError>,
    ) -> Result<(), CompileError> {
        if let Some(list) = object.get("allOf").and_then(Value::as_array) {
            for schema in list {
                self.walk(schema, b, v, path, out)?;
            }
        }
        if let Some(list) = object.get("anyOf").and_then(Value::as_array) {
            self.walk_union(list, b, v, path, out, false)?;
        }
        if let Some(list) = object.get("oneOf").and_then(Value::as_array) {
            self.walk_union(list, b, v, path, out, true)?;
        }
        if let Some(not) = object.get("not") {
            if self.ok(not, b, v)? {
                out.push(ValidationError::new(
                    path,
                    "not",
                    "must not match the schema in not",
                ));
            }
        }
        if let Some(cond) = object.get("if") {
            let branch = if self.ok(cond, b, v)? {
                object.get("then")
            } else {
                object.get("else")
            };
            if let Some(branch) = branch {
                self.walk(branch, b, v, path, out)?;
            }
        }
        if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
            self.walk_ref(reference, b, v, path, out)?;
        }
        if out.len() == start
            && (object.contains_key("unevaluatedProperties")
                || object.contains_key("unevaluatedItems"))
        {
            self.walk_unevaluated(node, b, v, path, out)?;
        }
        if out.len() == start && object.get("$dynamicRef").and_then(Value::as_str).is_some() {
            out.push(ValidationError::new(
                path,
                "$dynamicRef",
                "does not match the dynamically referenced schema",
            ));
        }
        Ok(())
    }

    fn walk_ref(
        &mut self,
        reference: &str,
        base: &str,
        v: &Value,
        path: &str,
        out: &mut Vec<ValidationError>,
    ) -> Result<(), CompileError> {
        let target = self.c.r.resolve(reference, base)?;
        if target.node == &Value::Bool(false) && self.c.r.is_unresolved(reference, base) {
            out.push(ValidationError::new(
                path,
                "$ref",
                format!("unresolved reference {reference}"),
            ));
            return Ok(());
        }
        // 上游在这里由 JS 的调用栈接住环（`RangeError`）；Rust 的栈用尽就是 abort，所以同一个目标
        // 拿同一个值再进来一次就停（见 `PORTING.md` 差异表）。
        let key = (
            target.node as *const Value as usize,
            v as *const Value as usize,
        );
        if self.in_flight.contains(&key) {
            return Ok(());
        }
        if !is_schema_object(target.node) || !(v.is_object() || v.is_array()) {
            return self.walk_guarded(key, target.node, &target.base, v, path, out);
        }
        let memo = self.memo.get_or_insert_with(HashMap::new);
        if let Some(known) = memo.get(&key) {
            if known.path == path {
                let errors = known.errors.clone();
                out.extend(errors);
                return Ok(());
            }
        }
        let mut errors: Vec<ValidationError> = Vec::new();
        let walked = self.walk_guarded(key, target.node, &target.base, v, path, &mut errors);
        walked?;
        if let Some(memo) = self.memo.as_mut() {
            memo.insert(
                key,
                Memo {
                    path: path.to_string(),
                    errors: errors.clone(),
                },
            );
        }
        out.extend(errors);
        Ok(())
    }

    /// 记下“正在解释这个引用目标上的这个值”，解释完再摘掉。
    fn walk_guarded(
        &mut self,
        key: (usize, usize),
        node: &'a Value,
        base: &str,
        v: &Value,
        path: &str,
        out: &mut Vec<ValidationError>,
    ) -> Result<(), CompileError> {
        self.in_flight.push(key);
        let walked = self.walk(node, base, v, path, out);
        self.in_flight.pop();
        walked
    }

    fn walk_string(
        &mut self,
        node: &SchemaObject,
        v: &str,
        path: &str,
        out: &mut Vec<ValidationError>,
    ) {
        let n = code_points(v) as f64;
        if let Some(min) = number_of(node.get("minLength")) {
            if n < min {
                out.push(ValidationError::new(
                    path,
                    "minLength",
                    format!("expected at least {} characters", js::number(min)),
                ));
            }
        }
        if let Some(max) = number_of(node.get("maxLength")) {
            if n > max {
                out.push(ValidationError::new(
                    path,
                    "maxLength",
                    format!("expected at most {} characters", js::number(max)),
                ));
            }
        }
        if let Some(source) = node.get("pattern").and_then(Value::as_str) {
            let matched = shared_regex(source).map(|re| re.find(v).is_some());
            if !matched.unwrap_or(false) {
                out.push(ValidationError::new(
                    path,
                    "pattern",
                    format!("expected to match {source}"),
                ));
            }
        }
        let formats = self.c.formats;
        let name = node.get("format").and_then(Value::as_str);
        let format = match (formats, name) {
            (Some(formats), Some(name)) => formats.get(name),
            _ => None,
        };
        if let Some(format) = format {
            if !format(v) {
                out.push(ValidationError::new(
                    path,
                    "format",
                    format!("expected format {}", name.unwrap_or("")),
                ));
            }
        }
    }

    /// 数字与 bigint 一视同仁：任一种的界都与任一种的值比。JSON 里只有 number。
    fn walk_number(
        &mut self,
        node: &SchemaObject,
        v: f64,
        path: &str,
        out: &mut Vec<ValidationError>,
    ) {
        let minimum = number_of(node.get("minimum"));
        let maximum = number_of(node.get("maximum"));
        let exclusive_minimum = number_of(node.get("exclusiveMinimum"));
        let exclusive_maximum = number_of(node.get("exclusiveMaximum"));
        let multiple_of = number_of(node.get("multipleOf"));
        if let Some(minimum) = minimum {
            if v < minimum {
                out.push(ValidationError::new(
                    path,
                    "minimum",
                    format!("expected >= {}", js::number(minimum)),
                ));
            }
        }
        if let Some(maximum) = maximum {
            if v > maximum {
                out.push(ValidationError::new(
                    path,
                    "maximum",
                    format!("expected <= {}", js::number(maximum)),
                ));
            }
        }
        if let Some(exclusive_minimum) = exclusive_minimum {
            if v <= exclusive_minimum {
                out.push(ValidationError::new(
                    path,
                    "exclusiveMinimum",
                    format!("expected > {}", js::number(exclusive_minimum)),
                ));
            }
        }
        if let Some(exclusive_maximum) = exclusive_maximum {
            if v >= exclusive_maximum {
                out.push(ValidationError::new(
                    path,
                    "exclusiveMaximum",
                    format!("expected < {}", js::number(exclusive_maximum)),
                ));
            }
        }
        if let Some(multiple_of) = multiple_of {
            if !is_multiple(v, multiple_of) {
                out.push(ValidationError::new(
                    path,
                    "multipleOf",
                    format!("expected a multiple of {}", js::number(multiple_of)),
                ));
            }
        }
    }

    fn walk_object(
        &mut self,
        node: &'a Value,
        b: &str,
        v: &Value,
        path: &str,
        out: &mut Vec<ValidationError>,
    ) -> Result<(), CompileError> {
        let object: &'a SchemaObject = node.as_object().expect("object");
        let value = v.as_object().expect("object value");
        let properties = object.get("properties").and_then(Value::as_object);
        if let Some(required) = object.get("required").and_then(Value::as_array) {
            for key in required {
                let Some(key) = key.as_str() else {
                    continue;
                };
                // 上游另有“有这一项、值是 undefined”的一条：JSON 里没有 undefined，缺席就是缺席。
                if !value.contains_key(key) {
                    out.push(ValidationError::new(
                        pointer(path, key),
                        "required",
                        "missing",
                    ));
                }
            }
        }
        if let Some(properties) = properties {
            // 上游 `for (const k of Object.keys(properties))`：JS 的键序，像数字的键在前。
            for (key, schema) in js::entries(properties) {
                if let Some(child) = value.get(key.as_str()) {
                    self.walk(schema, b, child, &pointer(path, key), out)?;
                }
            }
        }
        let patterns: Vec<(&String, &Value)> = object
            .get("patternProperties")
            .and_then(Value::as_object)
            .map(js::entries)
            .unwrap_or_default();
        let additional = object.get("additionalProperties");
        let property_names = object.get("propertyNames");
        for (key, child) in js::entries(value) {
            let mut matched = properties.is_some_and(|properties| properties.contains_key(key));
            for (pattern, schema) in &patterns {
                let matched_pattern = shared_regex(pattern)
                    .map(|re| re.find(key).is_some())
                    .unwrap_or(false);
                if matched_pattern {
                    matched = true;
                    self.walk(schema, b, child, &pointer(path, key), out)?;
                }
            }
            if !matched {
                if let Some(additional) = additional {
                    if additional == &Value::Bool(false) {
                        out.push(ValidationError::new(
                            pointer(path, key),
                            "additionalProperties",
                            "unknown field",
                        ));
                    } else {
                        self.walk(additional, b, child, &pointer(path, key), out)?;
                    }
                }
            }
            if let Some(names) = property_names {
                if !self.ok(names, b, &Value::String(key.clone()))? {
                    out.push(ValidationError::new(
                        pointer(path, key),
                        "propertyNames",
                        "invalid property name",
                    ));
                }
            }
        }
        if let Some(min) = number_of(object.get("minProperties")) {
            if (value.len() as f64) < min {
                out.push(ValidationError::new(
                    path,
                    "minProperties",
                    format!("expected at least {} properties", js::number(min)),
                ));
            }
        }
        if let Some(max) = number_of(object.get("maxProperties")) {
            if (value.len() as f64) > max {
                out.push(ValidationError::new(
                    path,
                    "maxProperties",
                    format!("expected at most {} properties", js::number(max)),
                ));
            }
        }
        // 上游把两张表摊成一张：`dependencies` 的键盖住 `dependentRequired` 的同名键，位置留在原处。
        let mut dependencies: Vec<(&'a String, &'a Value)> = Vec::new();
        if self.c.draft != Draft::Draft7 {
            if let Some(map) = object.get("dependentRequired").and_then(Value::as_object) {
                extend_entries(&mut dependencies, map);
            }
        }
        if let Some(map) = object.get("dependencies").and_then(Value::as_object) {
            extend_entries(&mut dependencies, map);
        }
        sort_by_js_key(&mut dependencies);
        for (key, dependency) in &dependencies {
            if !value.contains_key(key.as_str()) {
                continue;
            }
            match dependency.as_array() {
                None => self.walk(dependency, b, v, path, out)?,
                Some(list) => {
                    for required in list {
                        let required = js::to_string_value(required);
                        if !value.contains_key(&required) {
                            out.push(ValidationError::new(
                                pointer(path, &required),
                                "dependentRequired",
                                format!("required when {key} is present"),
                            ));
                        }
                    }
                }
            }
        }
        if self.c.draft != Draft::Draft7 {
            if let Some(map) = object.get("dependentSchemas").and_then(Value::as_object) {
                for (key, schema) in js::entries(map) {
                    if value.contains_key(key.as_str()) {
                        self.walk(schema, b, v, path, out)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn walk_array(
        &mut self,
        node: &'a Value,
        b: &str,
        v: &Value,
        path: &str,
        out: &mut Vec<ValidationError>,
    ) -> Result<(), CompileError> {
        let object: &'a SchemaObject = node.as_object().expect("object");
        let list = v.as_array().expect("array value");
        let mut prefix: &[Value] = &[];
        let rest: Option<&Value>;
        match object.get("items") {
            Some(items) if items.is_array() => {
                prefix = items.as_array().expect("items array");
                rest = object.get("additionalItems");
            }
            _ => {
                if self.c.draft != Draft::Draft7 {
                    if let Some(list) = object.get("prefixItems").and_then(Value::as_array) {
                        prefix = list;
                    }
                }
                rest = object.get("items");
            }
        }
        for (index, item) in list.iter().enumerate() {
            let schema = if index < prefix.len() {
                prefix.get(index)
            } else {
                rest
            };
            if let Some(schema) = schema {
                self.walk(schema, b, item, &format!("{path}/{index}"), out)?;
            }
        }
        if let Some(min) = number_of(object.get("minItems")) {
            if (list.len() as f64) < min {
                out.push(ValidationError::new(
                    path,
                    "minItems",
                    format!("expected at least {} items", js::number(min)),
                ));
            }
        }
        if let Some(max) = number_of(object.get("maxItems")) {
            if (list.len() as f64) > max {
                out.push(ValidationError::new(
                    path,
                    "maxItems",
                    format!("expected at most {} items", js::number(max)),
                ));
            }
        }
        if object.get("uniqueItems") == Some(&Value::Bool(true)) && !unique_check(v) {
            out.push(ValidationError::new(
                path,
                "uniqueItems",
                "expected unique items",
            ));
        }
        if let Some(contains) = object.get("contains") {
            let draft7 = self.c.draft == Draft::Draft7;
            let min = if draft7 {
                1.0
            } else {
                number_of(object.get("minContains")).unwrap_or(1.0)
            };
            let max = if draft7 {
                f64::INFINITY
            } else {
                number_of(object.get("maxContains")).unwrap_or(f64::INFINITY)
            };
            let check = contains_check(self.check(contains, b)?, min, max);
            if !check(v) {
                out.push(ValidationError::new(
                    path,
                    "contains",
                    contains_message(min, max),
                ));
            }
        }
        Ok(())
    }

    fn walk_union(
        &mut self,
        branches: &'a [Value],
        b: &str,
        v: &Value,
        path: &str,
        out: &mut Vec<ValidationError>,
        exactly_one: bool,
    ) -> Result<(), CompileError> {
        let mut matches = 0usize;
        for branch in branches {
            if self.ok(branch, b, v)? {
                matches += 1;
            }
        }
        if matches == 1 || (!exactly_one && matches > 0) {
            return Ok(());
        }
        let keyword = if exactly_one { "oneOf" } else { "anyOf" };
        if matches > 1 {
            out.push(ValidationError::new(
                path,
                keyword,
                "matches more than one alternative",
            ));
            return Ok(());
        }
        // 带标记的联合报标记挑中的那个分支；否则报第一个错误最深的那一支。
        if let Some(tag) = self.tag_of(branches, b)? {
            if let Some(object) = v.as_object() {
                let selected = object.get(&tag.key).and_then(|value| {
                    tag.values
                        .iter()
                        .position(|candidate| deep_equal(candidate, value))
                });
                match selected {
                    None => out.push(ValidationError::new(
                        &pointer(path, &tag.key),
                        keyword,
                        format!("expected one of {}", list_values(&tag.values)),
                    )),
                    Some(index) => self.walk(&branches[index], b, v, path, out)?,
                }
                return Ok(());
            }
        }
        // 对象归给声明了它最多属性的那一支（最可能是本来要写的那一支），再归给第一个错误最深的那一支。
        let mut best: Option<(Vec<ValidationError>, usize)> = None;
        for branch in branches {
            let mut errors: Vec<ValidationError> = Vec::new();
            self.walk(branch, b, v, path, &mut errors)?;
            if errors.is_empty() {
                continue;
            }
            let declared = if is_object(v) {
                self.declared_keys(branch, b, v)?
            } else {
                0
            };
            let better = match &best {
                None => true,
                Some((best_errors, best_declared)) => {
                    declared > *best_declared
                        || (declared == *best_declared
                            && utf16_len(&errors[0].path) > utf16_len(&best_errors[0].path))
                }
            };
            if better {
                best = Some((errors, declared));
            }
        }
        match best {
            Some((errors, _)) => out.extend(errors),
            None => out.push(ValidationError::new(
                path,
                keyword,
                "matches no alternative",
            )),
        }
        Ok(())
    }

    /// 分支 `s`（可能经过一层 `$ref`）在 `properties` 里声明了 `v` 的多少个键。
    fn declared_keys(&mut self, s: &'a Value, b: &str, v: &Value) -> Result<usize, CompileError> {
        let reference = s.get("$ref").and_then(Value::as_str);
        let node: &'a Value = match reference {
            Some(reference) => {
                let base = self.c.r.base_for(s, b);
                self.c.r.resolve(reference, &base)?.node
            }
            None => s,
        };
        let properties = node.get("properties").and_then(Value::as_object);
        let Some(value) = v.as_object() else {
            return Ok(0);
        };
        Ok(value
            .keys()
            .filter(|key| properties.is_some_and(|properties| properties.contains_key(*key)))
            .count())
    }

    /// 联合分支共同的标记键：每一支都在同一个键上带一个互不相同的原始 `const`。
    fn tag_of(&mut self, branches: &'a [Value], b: &str) -> Result<Option<Tag>, CompileError> {
        let mut resolved: Vec<&'a Value> = Vec::with_capacity(branches.len());
        for branch in branches {
            let node: &'a Value = match branch.get("$ref").and_then(Value::as_str) {
                Some(reference) => {
                    let base = self.c.r.base_for(branch, b);
                    self.c.r.resolve(reference, &base)?.node
                }
                None => branch,
            };
            resolved.push(node);
        }
        // 上游 `resolved[0]` 在空列表上是 `undefined`，`isSchemaObject` 就此返回 `undefined`：
        // `anyOf: []` 的“没有一支成立”就走这条路。
        let Some(first) = resolved.first().and_then(|first| first.as_object()) else {
            return Ok(None);
        };
        let Some(first_properties) = first.get("properties").and_then(Value::as_object) else {
            return Ok(None);
        };
        for key in js::keys(first_properties) {
            let mut values: Vec<Value> = Vec::with_capacity(resolved.len());
            let mut complete = true;
            for branch in &resolved {
                let tag = branch
                    .get("properties")
                    .and_then(Value::as_object)
                    .and_then(|properties| properties.get(key))
                    .and_then(Value::as_object)
                    .and_then(|tag| tag.get("const"));
                match tag {
                    Some(tag) if !tag.is_object() && !tag.is_array() => values.push(tag.clone()),
                    _ => {
                        complete = false;
                        break;
                    }
                }
            }
            if !complete || has_duplicates(&values) {
                continue;
            }
            return Ok(Some(Tag {
                key: key.clone(),
                values,
            }));
        }
        Ok(None)
    }

    fn walk_unevaluated(
        &mut self,
        node: &'a Value,
        b: &str,
        v: &Value,
        path: &str,
        out: &mut Vec<ValidationError>,
    ) -> Result<(), CompileError> {
        let object: &'a SchemaObject = node.as_object().expect("object");
        // `b` 已经应用过 `$id`，这份视图不能再应用一次。
        let check = self
            .c
            .annotating_view(node, b, UNEVALUATED_REST)
            .map_err(failure_error)?;
        let mut annotations = new_annotations();
        check(v, &mut annotations);
        if let Some(value) = v.as_object() {
            if let Some(unevaluated) = object.get("unevaluatedProperties") {
                if !annotations.all_props {
                    for (key, child) in js::entries(value) {
                        if annotations
                            .props
                            .as_ref()
                            .is_some_and(|props| props.contains(key))
                        {
                            continue;
                        }
                        if unevaluated == &Value::Bool(false) {
                            out.push(ValidationError::new(
                                pointer(path, key),
                                "unevaluatedProperties",
                                "unknown field",
                            ));
                        } else {
                            self.walk(unevaluated, b, child, &pointer(path, key), out)?;
                        }
                    }
                }
            }
        }
        if let Some(list) = v.as_array() {
            if let Some(unevaluated) = object.get("unevaluatedItems") {
                if !annotations.all_items {
                    for index in annotations.items..list.len() {
                        if annotations
                            .indexes
                            .as_ref()
                            .is_some_and(|indexes| indexes.contains(&index))
                        {
                            continue;
                        }
                        if unevaluated == &Value::Bool(false) {
                            out.push(ValidationError::new(
                                format!("{path}/{index}"),
                                "unevaluatedItems",
                                "unexpected item",
                            ));
                        } else {
                            self.walk(
                                unevaluated,
                                b,
                                &list[index],
                                &format!("{path}/{index}"),
                                out,
                            )?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// 带标记联合的标记键：键名，以及每一支在该键上的取值。
struct Tag {
    key: String,
    values: Vec<Value>,
}

/// JS 的对象展开：同名键的值换成后一份的，位置留在先出现的地方。每一份自己按 JS 的键序走。
fn extend_entries<'a>(
    entries: &mut Vec<(&'a String, &'a Value)>,
    map: &'a serde_json::Map<String, Value>,
) {
    for (key, value) in js::entries(map) {
        match entries.iter_mut().find(|(existing, _)| *existing == key) {
            Some(slot) => slot.1 = value,
            None => entries.push((key, value)),
        }
    }
}

/// 把合并后的条目按 JS 的键序摆好：像数组下标的键在前、按数值升序，其余键保持插入顺序。
///
/// 上游把两张表展开成一个对象，`Object.entries` 的键序就是这个顺序；只把每一份各自按 JS 键序
/// 拼起来不够——`dependencies` 里的 `"1"` 要排到 `dependentRequired` 里的 `"b"` 前面。
fn sort_by_js_key(entries: &mut [(&String, &Value)]) {
    entries.sort_by(|(a, _), (b, _)| match (js::array_index(a), js::array_index(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    });
}

fn failure_error(failure: Failure) -> CompileError {
    match failure {
        Failure::Error(error) => error,
        // 根 schema 需不需要追踪动态作用域，编译期已经定过；走到这里说明这一步用到的子 schema 需要它。
        Failure::Restart => CompileError::new("dynamic scope restart failed"),
    }
}

/// 一条错误里数字的文字：枚举的错误名允许取值的清单，多到几千个时截断，让消息读得下去。
const LISTED_VALUES: usize = 20;

/// `values` 的 JSON 文本，至多 `LISTED_VALUES` 个，然后还有多少个。
fn list_values(values: &[Value]) -> String {
    if values.len() <= LISTED_VALUES {
        return js::stringify(&Value::Array(values.to_vec()));
    }
    let listed = js::stringify(&Value::Array(values[..LISTED_VALUES].to_vec()));
    format!(
        "{}, ... and {} more]",
        &listed[..listed.len() - 1],
        values.len() - LISTED_VALUES
    )
}

fn contains_message(min: f64, max: f64) -> String {
    if max == f64::INFINITY {
        return format!(
            "expected at least {} matching {}",
            js::number(min),
            if min == 1.0 { "item" } else { "items" }
        );
    }
    if min == 0.0 {
        return format!(
            "expected at most {} matching {}",
            js::number(max),
            if max == 1.0 { "item" } else { "items" }
        );
    }
    format!(
        "expected between {} and {} matching items",
        js::number(min),
        js::number(max)
    )
}

fn pointer(path: &str, key: &str) -> String {
    let mut escaped = String::with_capacity(key.len());
    for character in key.chars() {
        match character {
            '~' => escaped.push_str("~0"),
            '/' => escaped.push_str("~1"),
            other => escaped.push(other),
        }
    }
    format!("{path}/{escaped}")
}

/// 上游 `typeName`。
fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) => {
            let integer = number
                .as_f64()
                .is_some_and(|x| x.is_finite() && x.fract() == 0.0);
            if integer {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// 数字关键字：一个 JS 的 `number`。
fn number_of(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64)
}

fn has_duplicates(values: &[Value]) -> bool {
    for i in 0..values.len() {
        for j in 0..i {
            if deep_equal(&values[i], &values[j]) {
                return true;
            }
        }
    }
    false
}

/// JS 字符串的长度（UTF-16 单元数）：上游用 `path.length` 比深浅。
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `Array.prototype.join` 里的一项：`null` 和 `undefined` 变成空串。
fn join_item(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        other => js::to_string_value(other),
    }
}
