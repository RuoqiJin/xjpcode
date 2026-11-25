//! 对应上游 `packages/schema/src/compiler.ts`。
//!
//! 编译器：一份 schema 变成一个检查闭包，只建一次。关键字按它们约束的类型分组，值在每个 schema 上
//! 只分类一次；常见的形状（闭合对象、单项 schema 的数组、带标记的联合、叶子 schema）各有一个专门的
//! 闭包。

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use regress::Regex;
use serde_json::Value;

use crate::annotations::{
    annotate_all, annotate_any, annotate_array, annotate_dependent, annotate_if, annotate_object,
    annotating_false, annotating_true, ignore_annotations, indirect_annotating,
    isolate_annotations, scoped_annotating, unevaluated_items, unevaluated_properties,
    with_fresh_annotations, AnnotatingCheck, AnnotatingSlot, Annotations,
};
use crate::checks::{
    and, any_check, array_type_check, declared_object, deep_equal, enum_check, guard_array,
    guard_object, if_check, indirect, items_check, length_check, list_check, masked_any_check,
    not_check, object_type_check, one_check, open_object, property_count_check,
    property_names_check, scoped_check, string_check, tagged_check, type_check, type_check_of,
    type_mask_of_names, unique_check, CheckSlot, ObjectEntry,
};
use crate::checks::{
    bytes_check, const_check, contains_check, dependent_required_check, dependent_schemas_check,
    number_check, Guard, NumberMode,
};
use crate::js;
use crate::resolver::{document_of, resolve_uri, Resolver};
use crate::shared::shared_regex;
use crate::types::{
    Check, CompileError, CompileOptions, Draft, FormatCheck, JsonSchema, SchemaObject,
};

/// 不带动态作用域追踪的编译碰到“首尾都有 `$dynamicRef`”时要求重启：换成追踪模式再编一次。
pub(crate) enum Failure {
    Restart,
    Error(CompileError),
}

impl From<CompileError> for Failure {
    fn from(error: CompileError) -> Self {
        Failure::Error(error)
    }
}

/// 编译 `schema` 的根检查器。失败时不返回 Validator：上游 `compile()` 在这里 throw。
pub fn compile_check(schema: &JsonSchema, options: &CompileOptions) -> Result<Check, CompileError> {
    reject_ref_cycle(schema, options)?;
    let mut plain = Compiler::new(schema, options, false);
    match plain.root() {
        Ok(check) => Ok(check),
        Err(Failure::Error(error)) => Err(error),
        Err(Failure::Restart) => {
            let mut dynamic = Compiler::new(schema, options, true);
            dynamic.root().map_err(|failure| match failure {
                Failure::Error(error) => error,
                Failure::Restart => CompileError::new("dynamic scope restart failed"),
            })
        }
    }
}

/// 纯引用环在检查期永远不会返回：一圈都是只有 `$ref` 的 schema，进来就再进来一次。
///
/// 上游把这件事留给 JS 的调用栈，检查时抛 `RangeError`；Rust 里同样的无限递归会让整个进程 abort，
/// 不能留到运行期，所以编译期先顺着根 schema 的引用链走一遍（见 `PORTING.md` 差异表）。只走根这一
/// 条链：没人引用的环永远到不了，上游也不会因它报错。
fn reject_ref_cycle<'a>(
    schema: &'a JsonSchema,
    options: &'a CompileOptions,
) -> Result<(), CompileError> {
    let mut compiler = Compiler::new(schema, options, false);
    let root_base: &'static str = compiler.r.root_base;
    let mut node: &'a Value = schema;
    let mut base = compiler.r.base_for(node, root_base);
    let mut chain = vec![address_of(node)];
    while let Some(reference) = forward_reference(node, options.draft) {
        let target = compiler.r.resolve(reference, &base)?;
        if chain.contains(&address_of(target.node)) {
            return Err(CompileError::new(format!(
                "Unresolvable $ref cycle: {reference} in {}",
                js::stringify(node)
            )));
        }
        chain.push(address_of(target.node));
        node = target.node;
        base = target.base;
    }
    Ok(())
}

/// `node` 的检查是不是只有这一个引用：是的话返回它指向的引用串。
///
/// draft 7 里 `$ref` 让兄弟关键字全部失效，所以带 `$ref` 的 schema 一律算；2020-12 里要除注解之外
/// 什么都不带才算。`$dynamicRef` 可能按动态作用域解析，不追——那些环留给检查期的守卫。
fn forward_reference(node: &Value, draft: Draft) -> Option<&str> {
    let object = node.as_object()?;
    let reference = object.get("$ref").and_then(Value::as_str)?;
    if draft == Draft::Draft7 {
        return Some(reference);
    }
    let only_ref = object
        .iter()
        .all(|(key, _)| key == "$ref" || NEUTRAL_KEYWORDS.contains(&key.as_str()));
    only_ref.then_some(reference)
}

/// 一个 schema 的关键字视图：整份对象，或者去掉标注型关键字之后的投影。
///
/// 上游在标注路径上现造一个 `rest` 对象（`rest[k] = node[k]` 复制的是引用），Rust 侧改成“同一个对象、
/// 少看几个键”的视图，所以子 schema 的身份不变，缓存照常命中。
#[derive(Clone, Copy)]
pub(crate) struct SchemaNode<'a> {
    fields: &'a SchemaObject,
    exclude: Option<&'static [&'static str]>,
}

/// 一个键都不少看。
const NO_KEYS: &[&str] = &[];

impl<'a> SchemaNode<'a> {
    fn all(fields: &'a SchemaObject) -> Self {
        Self {
            fields,
            exclude: None,
        }
    }

    fn without(fields: &'a SchemaObject, exclude: &'static [&'static str]) -> Self {
        Self {
            fields,
            exclude: Some(exclude),
        }
    }

    fn get(&self, key: &str) -> Option<&'a Value> {
        if self.exclude.is_some_and(|exclude| exclude.contains(&key)) {
            return None;
        }
        self.fields.get(key)
    }

    fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
}

/// 一次 schema 编译。根检查器建好之后，它的缓存和索引都可以回收。
pub struct Compiler<'a> {
    pub r: Resolver<'a>,
    pub draft: Draft,
    pub formats: Option<&'a std::collections::BTreeMap<String, FormatCheck>>,
    /// 检查是否追踪动态作用域（schema 里有首尾配对的 `$dynamicRef`）。
    pub dynamic: bool,
    schema: &'a JsonSchema,
    cache: HashMap<usize, Check>,
    annotating_cache: HashMap<usize, AnnotatingCheck>,
}

impl<'a> Compiler<'a> {
    pub fn new(schema: &'a JsonSchema, options: &'a CompileOptions, dynamic: bool) -> Self {
        Self {
            r: Resolver::new(schema, options),
            draft: options.draft,
            formats: options.formats.as_ref(),
            dynamic,
            schema,
            cache: HashMap::new(),
            annotating_cache: HashMap::new(),
        }
    }

    /// 根 schema 的检查器。
    pub(crate) fn root(&mut self) -> Result<Check, Failure> {
        self.check(self.schema, self.r.root_base)
    }

    /// 一个 schema 的检查器（按 schema 对象缓存；递归引用走一层间接）。
    pub(crate) fn check(&mut self, node: &'a Value, base: &str) -> Result<Check, Failure> {
        if let Some(boolean) = node.as_bool() {
            return Ok(if boolean {
                crate::checks::always()
            } else {
                crate::checks::never()
            });
        }
        let Some(object) = node.as_object() else {
            return Err(Failure::Error(CompileError::new(format!(
                "A schema must be an object or a boolean, got {}",
                js::stringify(node)
            ))));
        };
        let address = address_of(node);
        if let Some(cached) = self.cache.get(&address) {
            return Ok(cached.clone());
        }
        if let Some(bare) = self.bare_type(object) {
            return Ok(bare);
        }
        let slot = CheckSlot::new();
        self.cache.insert(address, indirect(slot.clone()));
        let base = self.r.base_for(node, base);
        let check = self.build(node, &base)?;
        slot.set(&check);
        self.cache.insert(address, check.clone());
        Ok(check)
    }

    fn build(&mut self, node: &'a Value, base: &str) -> Result<Check, Failure> {
        let object = node.as_object().expect("checked by check()");
        if self.draft == Draft::Draft7 && object.get("$ref").is_some_and(Value::is_string) {
            return self.ref_check(
                object
                    .get("$ref")
                    .and_then(Value::as_str)
                    .expect("$ref string"),
                base,
            );
        }
        if self.draft != Draft::Draft7
            && (object.contains_key("unevaluatedProperties")
                || object.contains_key("unevaluatedItems"))
        {
            return Ok(with_fresh_annotations(self.annotating(node, base)?));
        }
        let check = self.plain(SchemaNode::all(object), base)?;
        if self.dynamic && self.r.is_resource(node, base) {
            Ok(scoped_check(base.into(), check))
        } else {
            Ok(check)
        }
    }

    /// 只声明了 `type`（加注解）的 schema 用模块级的共享检查器，一个断言都没有的是 `ALWAYS`；其余
    /// schema 返回 `None`。这些是最常见的子 schema，共享起来不花代价。
    fn bare_type(&self, object: &'a SchemaObject) -> Option<Check> {
        let mut declared: Option<&'a Value> = None;
        for (key, value) in object {
            if key == "type" {
                declared = Some(value);
            } else if !NEUTRAL_KEYWORDS.contains(&key.as_str()) {
                return None;
            }
        }
        match declared {
            None => Some(crate::checks::always()),
            Some(value) => value.as_str().and_then(type_check),
        }
    }

    /// `node` 的每个关键字，不追注解。
    fn plain(&mut self, node: SchemaNode<'a>, base: &str) -> Result<Check, Failure> {
        let mut parts: Vec<Check> = Vec::new();
        let types: Option<Vec<&'a Value>> = match node.get("type") {
            None => None,
            Some(value) => match value.as_array() {
                Some(list) => Some(list.iter().collect()),
                None => Some(vec![value]),
            },
        };
        let single: Option<&'a Value> = match types.as_ref() {
            Some(list) if list.len() == 1 => Some(list[0]),
            _ => None,
        };
        let single_type = single.and_then(Value::as_str);
        let type_in_group = if single_type == Some("bytes") {
            parts.push(bytes_check(
                number(node.get("minLength")).unwrap_or(0.0),
                number(node.get("maxLength")).unwrap_or(f64::INFINITY),
            ));
            true
        } else {
            let string = self.string_group(&node, single_type == Some("string"))?;
            let has_string = string.is_some();
            if let Some(string) = string {
                parts.push(string);
            }
            let numeric = matches!(
                single_type,
                Some("number") | Some("integer") | Some("bigint")
            );
            let mode = match single_type {
                Some("number") => NumberMode::Number,
                Some("integer") => NumberMode::Integer,
                Some("bigint") => NumberMode::Bigint,
                _ => NumberMode::Any,
            };
            let numeric_check = self.number_group(&node, mode);
            let has_numeric = numeric_check.is_some();
            if let Some(numeric_check) = numeric_check {
                parts.push(numeric_check);
            }
            let object = self.object_group(&node, base, single_type == Some("object"))?;
            let has_object = object.is_some();
            if let Some(object) = object {
                parts.push(object);
            }
            let array = self.array_group(&node, base, single_type == Some("array"))?;
            let has_array = array.is_some();
            if let Some(array) = array {
                parts.push(array);
            }
            (single_type == Some("string") && has_string)
                || (numeric && has_numeric)
                || (single_type == Some("object") && has_object)
                || (single_type == Some("array") && has_array)
        };
        if let Some(types) = types.as_ref() {
            if !type_in_group {
                parts.insert(0, type_check_of(types));
            }
        }
        self.in_place(&node, base, &mut parts)?;
        Ok(and(parts))
    }

    /// 作用于值本身、与类型无关的关键字。
    fn in_place(
        &mut self,
        node: &SchemaNode<'a>,
        base: &str,
        parts: &mut Vec<Check>,
    ) -> Result<(), Failure> {
        if let Some(values) = node.get("enum").and_then(Value::as_array) {
            parts.push(enum_check(values.clone()));
        }
        if let Some(value) = node.get("const") {
            parts.push(const_check(value.clone()));
        }
        if let Some(list) = node.get("allOf").and_then(Value::as_array) {
            let mut checks = Vec::with_capacity(list.len());
            for schema in list {
                checks.push(self.check(schema, base)?);
            }
            parts.push(and(checks));
        }
        if let Some(list) = node.get("anyOf").and_then(Value::as_array) {
            parts.push(self.union(list, base, false)?);
        }
        if let Some(list) = node.get("oneOf").and_then(Value::as_array) {
            parts.push(self.union(list, base, true)?);
        }
        if let Some(not) = node.get("not") {
            parts.push(not_check(self.check(not, base)?));
        }
        if let Some(cond) = node.get("if") {
            if node.get("then").is_some() || node.get("else").is_some() {
                let then = match node.get("then") {
                    None => crate::checks::always(),
                    Some(then) => self.check(then, base)?,
                };
                let otherwise = match node.get("else") {
                    None => crate::checks::always(),
                    Some(otherwise) => self.check(otherwise, base)?,
                };
                parts.push(if_check(self.check(cond, base)?, then, otherwise));
            }
        }
        if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
            parts.push(self.ref_check(reference, base)?);
        }
        if self.draft != Draft::Draft7 {
            if let Some(reference) = node.get("$dynamicRef").and_then(Value::as_str) {
                parts.push(self.dynamic_ref(reference, base)?);
            }
        }
        Ok(())
    }

    fn union(
        &mut self,
        branches: &'a [Value],
        base: &str,
        exactly_one: bool,
    ) -> Result<Check, Failure> {
        if let Some(check) = const_union(branches, exactly_one) {
            return Ok(check);
        }
        if let Some(check) = self.tagged(branches, base)? {
            return Ok(check);
        }
        let mut checks = Vec::with_capacity(branches.len());
        for schema in branches {
            checks.push(self.check(schema, base)?);
        }
        Ok(if exactly_one {
            one_check(checks)
        } else {
            any_of(checks)
        })
    }

    fn ref_check(&mut self, reference: &str, base: &str) -> Result<Check, Failure> {
        let target = self.r.resolve(reference, base)?;
        let check = self.check(target.node, &target.base)?;
        if self.dynamic && target.base != base {
            Ok(scoped_check(target.base.as_str().into(), check))
        } else {
            Ok(check)
        }
    }

    fn dynamic_ref(&mut self, reference: &str, base: &str) -> Result<Check, Failure> {
        let Some((initial, name)) = self.bookended(reference, base)? else {
            return self.ref_check(reference, base);
        };
        let fallback = self.check(initial.node, &initial.base)?;
        let lookup = self.dynamic_lookup(&name)?;
        Ok(Arc::new(move |v: &Value| {
            for uri in crate::checks::scope_snapshot() {
                if let Some(check) = lookup.get(format!("{uri}#{name}").as_str()) {
                    return check(v);
                }
            }
            fallback(v)
        }))
    }

    /// 一个动态解析的 `$dynamicRef` 的初始目标与锚点名；普通引用返回 `None`。
    fn bookended(
        &mut self,
        reference: &str,
        base: &str,
    ) -> Result<Option<(crate::resolver::Resolved<'a>, String)>, Failure> {
        let initial = self.r.resolve(reference, base)?;
        let uri = resolve_uri(reference, base);
        let name = uri.get(document_of(&uri).len() + 1..).unwrap_or("");
        let anchored = initial
            .node
            .as_object()
            .and_then(|object| object.get("$dynamicAnchor"))
            .and_then(Value::as_str)
            == Some(name);
        if name.is_empty() || name.starts_with('/') || !anchored {
            return Ok(None);
        }
        if !self.dynamic {
            return Err(Failure::Restart);
        }
        Ok(Some((initial, name.to_string())))
    }

    /// 编译每个同名 `$dynamicAnchor`，运行期按作用域里的 URI 取。
    ///
    /// 上游在第一次用到时按 URI 懒编译并记住；Rust 侧在编译期一次编全（锚点很少），检查期就不需要
    /// 再碰编译器。
    fn dynamic_lookup(&mut self, name: &str) -> Result<HashMap<String, Check>, Failure> {
        let anchors: Vec<(String, &'a Value)> = self
            .r
            .dynamic_anchors
            .iter()
            .map(|(key, target)| (key.clone(), *target))
            .collect();
        let mut found = HashMap::new();
        for (key, target) in anchors {
            if !key.ends_with(&format!("#{name}")) {
                continue;
            }
            let base = self
                .r
                .base_of
                .get(&address_of(target))
                .cloned()
                .unwrap_or_else(|| document_of(&key).to_string());
            let check = self.check(target, &base)?;
            found.insert(key, check);
        }
        Ok(found)
    }

    /// `anyOf`/`oneOf` 的分支全是在同一个键上带互不相同的必需 `const` 的对象：标记挑出那一个分支。
    fn tagged(&mut self, branches: &'a [Value], base: &str) -> Result<Option<Check>, Failure> {
        if branches.len() < 2 {
            return Ok(None);
        }
        let mut resolved: Vec<&'a SchemaObject> = Vec::with_capacity(branches.len());
        for branch in branches {
            let Some(object) = branch.as_object() else {
                return Ok(None);
            };
            let node: &'a SchemaObject = match branch.get("$ref").and_then(Value::as_str) {
                Some(reference) if object.len() == 1 => {
                    let resolved_base = self.r.base_for(branch, base);
                    let resolved = self.r.resolve(reference, &resolved_base)?;
                    match resolved.node.as_object() {
                        Some(node) => node,
                        None => return Ok(None),
                    }
                }
                _ => object,
            };
            resolved.push(node);
        }
        let Some(first) = resolved[0].get("properties").and_then(Value::as_object) else {
            return Ok(None);
        };
        // 上游 `Object.keys(first)`：像数字的键按 JS 的键序排在前面。
        for key in js::keys(first) {
            let mut tags: Vec<Value> = Vec::with_capacity(resolved.len());
            let mut complete = true;
            for branch in &resolved {
                match tag_of(branch, key) {
                    Some(tag) => tags.push(tag),
                    None => {
                        complete = false;
                        break;
                    }
                }
            }
            if !complete || has_duplicates(&tags) {
                continue;
            }
            let mut checks = Vec::with_capacity(branches.len());
            for schema in branches {
                checks.push(self.check(schema, base)?);
            }
            return Ok(Some(tagged_check(key.clone(), tags, checks)));
        }
        Ok(None)
    }

    fn string_group(
        &mut self,
        node: &SchemaNode<'a>,
        strict: bool,
    ) -> Result<Option<Check>, Failure> {
        let min = number(node.get("minLength")).unwrap_or(0.0);
        let max = number(node.get("maxLength")).unwrap_or(f64::INFINITY);
        let re: Option<Arc<Regex>> = match node.get("pattern").and_then(Value::as_str) {
            Some(source) => Some(shared_regex(source)?),
            None => None,
        };
        let format: Option<FormatCheck> =
            match (self.formats, node.get("format").and_then(Value::as_str)) {
                (Some(formats), Some(name)) => formats.get(name).cloned(),
                _ => None,
            };
        if min == 0.0 && max == f64::INFINITY && re.is_none() && format.is_none() {
            return Ok(None);
        }
        Ok(Some(string_check(strict, min, max, re, format)))
    }

    fn number_group(&mut self, node: &SchemaNode<'a>, mode: NumberMode) -> Option<Check> {
        let min = bound(node.get("minimum"));
        let max = bound(node.get("maximum"));
        let xmin = bound(node.get("exclusiveMinimum"));
        let xmax = bound(node.get("exclusiveMaximum"));
        let multiple_of = bound(node.get("multipleOf"));
        if [min, max, xmin, xmax, multiple_of]
            .iter()
            .all(Option::is_none)
        {
            return None;
        }
        let inf = f64::INFINITY;
        Some(number_check(
            mode,
            min.unwrap_or(-inf),
            max.unwrap_or(inf),
            xmin.unwrap_or(-inf),
            xmax.unwrap_or(inf),
            multiple_of,
        ))
    }

    fn object_group(
        &mut self,
        node: &SchemaNode<'a>,
        base: &str,
        strict: bool,
    ) -> Result<Option<Check>, Failure> {
        let only_core = OBJECT_EXTRAS.iter().all(|key| node.get(key).is_none());
        // 只有对象核心关键字的 schema 用一个闭包，顺带做类型守卫。
        let guard = if only_core {
            if strict {
                Guard::Strict
            } else {
                Guard::Loose
            }
        } else {
            Guard::None
        };
        let core = self.object_core(node, base, guard)?;
        if core.is_some() && only_core {
            return Ok(core);
        }
        let mut parts: Vec<Check> = Vec::new();
        if let Some(core) = core {
            parts.push(core);
        }
        if let Some(names) = node.get("propertyNames") {
            parts.push(property_names_check(self.check(names, base)?));
        }
        let min_properties = number(node.get("minProperties"));
        let max_properties = number(node.get("maxProperties"));
        if min_properties.is_some() || max_properties.is_some() {
            parts.push(property_count_check(
                min_properties.unwrap_or(0.0),
                max_properties.unwrap_or(f64::INFINITY),
            ));
        }
        let mut dependent_required: Vec<(String, Vec<String>)> = Vec::new();
        let mut dependent_schemas: Vec<(String, Check)> = Vec::new();
        if self.draft != Draft::Draft7 {
            for (key, list) in js::entries(object_or_empty(node.get("dependentRequired"))) {
                if let Some(list) = list.as_array() {
                    dependent_required.push((key.clone(), property_keys(list)));
                }
            }
            for (key, schema) in js::entries(object_or_empty(node.get("dependentSchemas"))) {
                dependent_schemas.push((key.clone(), self.check(schema, base)?));
            }
        }
        for (key, dependency) in js::entries(object_or_empty(node.get("dependencies"))) {
            if let Some(list) = dependency.as_array() {
                dependent_required.push((key.clone(), property_keys(list)));
            } else {
                dependent_schemas.push((key.clone(), self.check(dependency, base)?));
            }
        }
        if !dependent_required.is_empty() {
            parts.push(dependent_required_check(dependent_required));
        }
        if !dependent_schemas.is_empty() {
            parts.push(dependent_schemas_check(dependent_schemas));
        }
        if parts.is_empty() {
            return Ok(if strict {
                Some(object_type_check())
            } else {
                None
            });
        }
        Ok(Some(guard_object(and(parts), strict)))
    }

    fn object_core(
        &mut self,
        node: &SchemaNode<'a>,
        base: &str,
        guard: Guard,
    ) -> Result<Option<Check>, Failure> {
        let properties = object_or_empty(node.get("properties"));
        let required: Vec<&str> = node
            .get("required")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let patterns = object_or_empty(node.get("patternProperties"));
        let additional = node.get("additionalProperties");
        let keys: Vec<String> = js::keys(properties).into_iter().cloned().collect();
        let pattern_keys: Vec<String> = js::keys(patterns).into_iter().cloned().collect();
        if keys.len() + required.len() + pattern_keys.len() == 0
            && (additional.is_none() || additional == Some(&Value::Bool(true)))
        {
            return Ok(None);
        }
        // 每个对象一份数组：键、检查、是否必需。
        let mut entries: Vec<ObjectEntry> = Vec::with_capacity(keys.len());
        for key in &keys {
            let schema = properties.get(key).expect("declared property");
            let check = self.check(schema, base)?;
            entries.push(ObjectEntry {
                key: key.clone(),
                check,
                required: required.contains(&key.as_str()),
            });
        }
        let extra_required: Vec<String> = required
            .iter()
            .filter(|key| !properties.contains_key(**key))
            .map(|key| (*key).to_string())
            .collect();
        let mut pats: Vec<(Arc<Regex>, Check)> = Vec::with_capacity(pattern_keys.len());
        for pattern in &pattern_keys {
            let re = shared_regex(pattern)?;
            let schema = patterns.get(pattern).expect("declared pattern");
            pats.push((re, self.check(schema, base)?));
        }
        let extra = match additional {
            None => None,
            Some(value) if value == &Value::Bool(true) => None,
            Some(value) => Some(self.check(value, base)?),
        };
        let closed = additional == Some(&Value::Bool(false));
        if pats.is_empty() && extra_required.is_empty() && (extra.is_none() || closed) {
            let required_count = keys
                .iter()
                .filter(|key| required.contains(&key.as_str()))
                .count();
            return Ok(Some(declared_object(
                entries,
                required_count,
                closed,
                guard,
            )));
        }
        Ok(Some(open_object(
            entries,
            keys,
            extra_required,
            pats,
            extra,
            guard,
        )))
    }

    fn array_group(
        &mut self,
        node: &SchemaNode<'a>,
        base: &str,
        strict: bool,
    ) -> Result<Option<Check>, Failure> {
        let mut prefix: &'a [Value] = &[];
        let rest: Option<&'a Value>;
        match node.get("items") {
            Some(items) if items.is_array() => {
                prefix = items.as_array().expect("items array");
                rest = node.get("additionalItems");
            }
            _ => {
                if self.draft != Draft::Draft7 {
                    if let Some(list) = node.get("prefixItems").and_then(Value::as_array) {
                        prefix = list;
                    }
                }
                rest = node.get("items");
            }
        }
        let mut prefix_checks: Vec<Check> = Vec::with_capacity(prefix.len());
        for schema in prefix {
            prefix_checks.push(self.check(schema, base)?);
        }
        let rest_check = match rest {
            None => None,
            Some(value) if value == &Value::Bool(true) => None,
            Some(value) => Some(self.check(value, base)?),
        };
        let min_items = number(node.get("minItems"));
        let max_items = number(node.get("maxItems"));
        if node.get("contains").is_none()
            && node.get("uniqueItems") != Some(&Value::Bool(true))
            && (!prefix_checks.is_empty() || rest_check.is_some())
        {
            // 最常见的数组：守卫、长度界和项，一个闭包搞定。
            let guard = if strict { Guard::Strict } else { Guard::Loose };
            return Ok(Some(list_check(
                guard,
                min_items.unwrap_or(0.0),
                max_items.unwrap_or(f64::INFINITY),
                prefix_checks,
                rest_check,
            )));
        }
        let mut parts: Vec<Check> = Vec::new();
        if !prefix_checks.is_empty() || rest_check.is_some() {
            parts.push(items_check(prefix_checks, rest_check));
        }
        if min_items.is_some() || max_items.is_some() {
            parts.push(length_check(
                min_items.unwrap_or(0.0),
                max_items.unwrap_or(f64::INFINITY),
            ));
        }
        if let Some(contains) = node.get("contains") {
            let min = if self.draft == Draft::Draft7 {
                1.0
            } else {
                number(node.get("minContains")).unwrap_or(1.0)
            };
            let max = if self.draft == Draft::Draft7 {
                f64::INFINITY
            } else {
                number(node.get("maxContains")).unwrap_or(f64::INFINITY)
            };
            parts.push(contains_check(self.check(contains, base)?, min, max));
        }
        if node.get("uniqueItems") == Some(&Value::Bool(true)) {
            parts.push(Arc::new(unique_check));
        }
        if parts.is_empty() {
            return Ok(if strict {
                Some(array_type_check())
            } else {
                None
            });
        }
        Ok(Some(guard_array(and(parts), strict)))
    }

    /// 就地求值的 schema 的标注型检查（只在 `unevaluatedProperties`/`unevaluatedItems` 之下）。
    pub(crate) fn annotating(
        &mut self,
        node: &'a Value,
        base: &str,
    ) -> Result<AnnotatingCheck, Failure> {
        if let Some(boolean) = node.as_bool() {
            return Ok(if boolean {
                annotating_true()
            } else {
                annotating_false()
            });
        }
        if !node.is_object() {
            return Err(Failure::Error(CompileError::new(format!(
                "A schema must be an object or a boolean, got {}",
                js::stringify(node)
            ))));
        }
        let address = address_of(node);
        if let Some(cached) = self.annotating_cache.get(&address) {
            return Ok(cached.clone());
        }
        let slot = AnnotatingSlot::new();
        self.annotating_cache
            .insert(address, indirect_annotating(slot.clone()));
        let base = self.r.base_for(node, base);
        let check = self.build_annotating(node, &base, NO_KEYS)?;
        slot.set(&check);
        self.annotating_cache.insert(address, check.clone());
        Ok(check)
    }

    /// `node` 少看 `exclude` 里那几个关键字的标注型检查。
    ///
    /// 上游解释 `unevaluated*` 时现复制一份去掉 `$id` 与 `unevaluated*` 的对象再编译；Rust 侧把同一
    /// 件事做成一份关键字视图，schema 的身份不变（见 `PORTING.md` 差异表）。
    pub(crate) fn annotating_view(
        &mut self,
        node: &'a Value,
        base: &str,
        exclude: &'static [&'static str],
    ) -> Result<AnnotatingCheck, Failure> {
        if !node.is_object() {
            return Err(Failure::Error(CompileError::new(format!(
                "A schema must be an object or a boolean, got {}",
                js::stringify(node)
            ))));
        }
        let base = self.r.base_for(node, base);
        self.build_annotating(node, &base, exclude)
    }

    fn build_annotating(
        &mut self,
        node: &'a Value,
        base: &str,
        exclude: &'static [&'static str],
    ) -> Result<AnnotatingCheck, Failure> {
        let object = node.as_object().expect("checked by annotating()");
        let view = SchemaNode::without(object, exclude);
        let annotates = object
            .keys()
            .any(|key| !exclude.contains(&key.as_str()) && ANNOTATING.contains(&key.as_str()));
        let plain = self.plain(SchemaNode::without(object, ANNOTATING), base)?;
        let mut parts: Vec<AnnotatingCheck> = Vec::new();
        if annotates {
            self.annotating_parts(&view, base, &mut parts)?;
        }
        let all = if parts.is_empty() {
            ignore_annotations(plain)
        } else {
            annotate_all(plain, parts)
        };
        let own = if view.contains_key("unevaluatedProperties")
            || view.contains_key("unevaluatedItems")
        {
            isolate_annotations(all)
        } else {
            all
        };
        // 上游那份副本不是资源（`resources` 里没有它），所以 `$dynamicAnchor` 的包裹只在原样编译时加。
        if self.dynamic && exclude.is_empty() && self.r.is_resource(node, base) {
            Ok(scoped_annotating(base.into(), own))
        } else {
            Ok(own)
        }
    }

    fn annotating_parts(
        &mut self,
        node: &SchemaNode<'a>,
        base: &str,
        parts: &mut Vec<AnnotatingCheck>,
    ) -> Result<(), Failure> {
        if node.contains_key("properties")
            || node.contains_key("patternProperties")
            || node.contains_key("additionalProperties")
        {
            let mut props: Vec<(String, Check)> = Vec::new();
            for (key, schema) in js::entries(object_or_empty(node.get("properties"))) {
                props.push((key.clone(), self.check(schema, base)?));
            }
            let mut pats: Vec<(Arc<Regex>, Check)> = Vec::new();
            for (pattern, schema) in js::entries(object_or_empty(node.get("patternProperties"))) {
                pats.push((shared_regex(pattern)?, self.check(schema, base)?));
            }
            let extra = match node.get("additionalProperties") {
                None => None,
                Some(value) => Some(self.check(value, base)?),
            };
            parts.push(annotate_object(props, pats, extra));
        }
        if node.contains_key("prefixItems")
            || node.contains_key("items")
            || node.contains_key("contains")
        {
            let prefix: &'a [Value] = match node.get("prefixItems").and_then(Value::as_array) {
                Some(list) => list,
                None => node
                    .get("items")
                    .and_then(Value::as_array)
                    .map_or(&[], |list| list),
            };
            let rest = if node.get("items").is_some_and(Value::is_array) {
                node.get("additionalItems")
            } else {
                node.get("items")
            };
            let mut prefix_checks: Vec<Check> = Vec::with_capacity(prefix.len());
            for schema in prefix {
                prefix_checks.push(self.check(schema, base)?);
            }
            let rest_check = match rest {
                None => None,
                Some(value) => Some(self.check(value, base)?),
            };
            let contains = match node.get("contains") {
                None => None,
                Some(value) => Some(self.check(value, base)?),
            };
            parts.push(annotate_array(
                prefix_checks,
                rest_check,
                contains,
                number(node.get("minContains")).unwrap_or(1.0),
                number(node.get("maxContains")).unwrap_or(f64::INFINITY),
            ));
        }
        if let Some(list) = node.get("allOf").and_then(Value::as_array) {
            for schema in list {
                parts.push(self.annotating(schema, base)?);
            }
        }
        if let Some(list) = node.get("anyOf").and_then(Value::as_array) {
            let mut branches = Vec::with_capacity(list.len());
            for schema in list {
                branches.push(self.annotating(schema, base)?);
            }
            parts.push(annotate_any(branches, false));
        }
        if let Some(list) = node.get("oneOf").and_then(Value::as_array) {
            let mut branches = Vec::with_capacity(list.len());
            for schema in list {
                branches.push(self.annotating(schema, base)?);
            }
            parts.push(annotate_any(branches, true));
        }
        if let Some(cond) = node.get("if") {
            let then = match node.get("then") {
                None => None,
                Some(then) => Some(self.annotating(then, base)?),
            };
            let otherwise = match node.get("else") {
                None => None,
                Some(otherwise) => Some(self.annotating(otherwise, base)?),
            };
            parts.push(annotate_if(self.annotating(cond, base)?, then, otherwise));
        }
        if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
            parts.push(self.annotating_ref(reference, base)?);
        }
        if let Some(reference) = node.get("$dynamicRef").and_then(Value::as_str) {
            parts.push(self.annotating_dynamic_ref(reference, base)?);
        }
        for (key, schema) in js::entries(object_or_empty(node.get("dependentSchemas"))) {
            parts.push(annotate_dependent(
                key.clone(),
                self.annotating(schema, base)?,
            ));
        }
        if let Some(unevaluated) = node.get("unevaluatedProperties") {
            parts.push(unevaluated_properties(self.check(unevaluated, base)?));
        }
        if let Some(unevaluated) = node.get("unevaluatedItems") {
            parts.push(unevaluated_items(self.check(unevaluated, base)?));
        }
        Ok(())
    }

    fn annotating_ref(&mut self, reference: &str, base: &str) -> Result<AnnotatingCheck, Failure> {
        let target = self.r.resolve(reference, base)?;
        let check = self.annotating(target.node, &target.base)?;
        if self.dynamic && target.base != base {
            Ok(scoped_annotating(target.base.as_str().into(), check))
        } else {
            Ok(check)
        }
    }

    fn annotating_dynamic_ref(
        &mut self,
        reference: &str,
        base: &str,
    ) -> Result<AnnotatingCheck, Failure> {
        let Some((initial, name)) = self.bookended(reference, base)? else {
            return self.annotating_ref(reference, base);
        };
        let fallback = self.annotating(initial.node, &initial.base)?;
        let lookup = self.dynamic_annotating_lookup(&name)?;
        Ok(Arc::new(move |v: &Value, a: &mut Annotations| {
            for uri in crate::checks::scope_snapshot() {
                if let Some(check) = lookup.get(format!("{uri}#{name}").as_str()) {
                    return check(v, a);
                }
            }
            fallback(v, a)
        }))
    }

    fn dynamic_annotating_lookup(
        &mut self,
        name: &str,
    ) -> Result<HashMap<String, AnnotatingCheck>, Failure> {
        let anchors: Vec<(String, &'a Value)> = self
            .r
            .dynamic_anchors
            .iter()
            .map(|(key, target)| (key.clone(), *target))
            .collect();
        let mut found = HashMap::new();
        for (key, target) in anchors {
            if !key.ends_with(&format!("#{name}")) {
                continue;
            }
            let base = self
                .r
                .base_of
                .get(&address_of(target))
                .cloned()
                .unwrap_or_else(|| document_of(&key).to_string());
            let check = self.annotating(target, &base)?;
            found.insert(key, check);
        }
        Ok(found)
    }
}

/// 从不影响检查的关键字。
pub const NEUTRAL_KEYWORDS: &[&str] = &[
    "$schema",
    "$comment",
    "title",
    "description",
    "default",
    "examples",
    "deprecated",
    "readOnly",
    "writeOnly",
    "$defs",
    "definitions",
    "contentEncoding",
    "contentMediaType",
    "minContains",
    "maxContains",
    "$vocabulary",
];

/// 对象关键字里除核心（properties、required、patternProperties、additionalProperties）之外的那些。
const OBJECT_EXTRAS: &[&str] = &[
    "propertyNames",
    "minProperties",
    "maxProperties",
    "dependentRequired",
    "dependentSchemas",
    "dependencies",
];

/// 标注型检查自己处理的关键字；其它关键字都编译进它的普通部分。
const ANNOTATING: &[&str] = &[
    "properties",
    "patternProperties",
    "additionalProperties",
    "prefixItems",
    "items",
    "additionalItems",
    "contains",
    "minContains",
    "maxContains",
    "allOf",
    "anyOf",
    "oneOf",
    "if",
    "then",
    "else",
    "$ref",
    "$dynamicRef",
    "dependentSchemas",
    "unevaluatedProperties",
    "unevaluatedItems",
];

/// `anyOf`/`oneOf` 的分支全是 `{ const: 原始值 }`：等价于一个 enum（取值互不相同时 oneOf 也一样）。
fn const_union(branches: &[Value], exactly_one: bool) -> Option<Check> {
    let mut values: Vec<Value> = Vec::with_capacity(branches.len());
    for branch in branches {
        let object = branch.as_object()?;
        if !object.contains_key("const") {
            return None;
        }
        for key in object.keys() {
            if key != "const" && !NEUTRAL_KEYWORDS.contains(&key.as_str()) {
                return None;
            }
        }
        let value = object.get("const").expect("const");
        if value.is_object() || value.is_array() {
            return None;
        }
        if exactly_one && values.iter().any(|other| deep_equal(other, value)) {
            return None;
        }
        values.push(value.clone());
    }
    Some(enum_check(values))
}

/// `anyOf`：其中的裸类型分支（`nullable` 会给出一个）合成一次类型测试。
fn any_of(checks: Vec<Check>) -> Check {
    let mut types: Vec<&'static str> = Vec::new();
    let mut rest: Vec<Check> = Vec::new();
    for check in checks {
        match crate::checks::bare_type_of(&check) {
            Some(name) => types.push(name),
            None => rest.push(check),
        }
    }
    if types.is_empty() {
        any_check(rest)
    } else {
        masked_any_check(type_mask_of_names(&types), rest)
    }
}

/// 带标记联合里一个分支的标记：对象 schema 的必需属性上的原始 `const`。
fn tag_of(branch: &SchemaObject, key: &str) -> Option<Value> {
    if branch.get("type").and_then(Value::as_str) != Some("object") {
        return None;
    }
    let required = branch.get("required")?.as_array()?;
    if !required.iter().any(|name| name.as_str() == Some(key)) {
        return None;
    }
    let tag = branch
        .get("properties")?
        .as_object()?
        .get(key)?
        .as_object()?;
    let value = tag.get("const")?;
    if value.is_object() || value.is_array() {
        return None;
    }
    Some(value.clone())
}

fn has_duplicates(tags: &[Value]) -> bool {
    for i in 0..tags.len() {
        for j in 0..i {
            if deep_equal(&tags[i], &tags[j]) {
                return true;
            }
        }
    }
    false
}

/// 数字关键字的值：一个数字（TypeBox 的 `Type.BigInt` 边界在 JSON 里不出现）。
fn bound(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64)
}

fn number(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64)
}

static EMPTY_OBJECT: LazyLock<SchemaObject> = LazyLock::new(SchemaObject::new);

fn object_or_empty(value: Option<&Value>) -> &SchemaObject {
    value.and_then(Value::as_object).unwrap_or(&EMPTY_OBJECT)
}

/// `dependentRequired` 的一项按 JS 的属性键写出来：`Object.hasOwn(o, r)` 会把 `r` 转成属性键，
/// 数字 `1` 就是 `"1"`、布尔 `true` 就是 `"true"`。
fn property_keys(list: &[Value]) -> Vec<String> {
    list.iter().map(js::to_string_value).collect()
}

fn address_of(node: &Value) -> usize {
    node as *const Value as usize
}
