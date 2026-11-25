//! 对应上游 `packages/schema/src/coerce.ts`。
//!
//! 把一份宽松类型的值（模型给的工具参数、表单输入、查询串）先纠正成 schema 最可能的意思，再交给检查。
//! 上游只有带 `coerce: true` 编译出来的检查器才有它；Rust 侧 `Validator` 上一直有这两个方法（见
//! `PORTING.md` 差异表 #67），纠正器按需现建，检查路径为它不付任何代价。
//!
//! 三趟，按这个顺序：
//! 1. 可选属性上的 null：schema 不收 null 的可选属性，值为 null 时整项丢掉，当作没给。
//! 2. 从 TypeBox 1.3.27 的 `Value.Convert`（MIT）搬来的转换规则，按 JSON Schema 的结构判断
//!    （`$ref`、`const`、`enum`、`anyOf`/`oneOf`、`allOf`、`type`、`items`/`prefixItems`、
//!    `properties`/`patternProperties`）：字符串变成数字、单个值变成一个元素的数组、联合取第一个转换后
//!    成立的支。
//! 3. 按 `type` 的原始类型纠正（含 type 数组），同 Ajv 的 `coerceTypes`：第 2 趟留下的那些。
//!
//! 纠正不了的值原样留着，由检查去报。每一趟都是“改了才复制”：输入从不被修改。三趟都按同样的关键字
//! 走对象的成员：`properties`，然后 `patternProperties`，最后是两者都没点名的键走
//! `additionalProperties`；元组项按 `prefixItems`（或 draft 7 的 `items` 数组），其余项按 `items`。
//! draft 7 的 `additionalItems` 不纠正。
//!
//! 值固定是 `serde_json::Value`（`PORTING.md` 差异表 #40），所以 `bigint`、`undefined`、字节串这些
//! 只存在于 JS 的值没有对应物：指向它们的 schema 不会把值变成它们，值原样留着，交给检查去报。

use std::collections::HashMap;

use serde_json::{Map, Number, Value};

use crate::checks::deep_equal;
use crate::compiler::{Compiler, Failure};
use crate::js;
use crate::resolver::is_schema_object;
use crate::shared::shared_regex;

/// 给一份编译好的 schema 纠正值。
pub struct Coercer<'a> {
    c: Compiler<'a>,
    schema: &'a Value,
    /// 第 2 趟正在经过重入点的（schema 节点，值）对，当栈用。转换只有通过 `$ref` 或把一个单值包成
    /// 数组时才会拿同一个值再进同一个节点：递归 schema 碰到哪一支都不收的值，会一直转换下去。已经在
    /// 栈上的对（原始值按值、对象按引用）让值原样返回。只有这两个点会压栈，没有引用和包裹的 schema
    /// 不花代价。
    entered: Vec<(usize, Identity)>,
    /// 这一次 `coerce()` 里重入被截断了几次；跨过一次截断算出来的结果不记忆。
    cuts: usize,
    /// 一次 `coerce()` 里 `$ref` 目标按（节点，值）记下的结果：递归的联合每一支都转换同一批子值
    /// （而且都失败）时，否则会按深度指数级地重复转换。
    ///
    /// 值的键是它的 JSON 文本（见 `memo_key`）：JS 按对象引用记，Rust 侧的值是复制出来的，地址会
    /// 被回收后再用，不能当身份。转换只取决于节点与值的内容（栈上的重入截断过的不记），所以按内容
    /// 记结论相同——内容相同的两个对象本来就转出同一个结果。
    memo: Option<HashMap<usize, HashMap<String, Value>>>,
}

/// 一个值在 JS 里的身份。
///
/// 上游按引用认对象（`Object.is`）。Rust 侧的值总是复制出来的，每一趟都可能交出一个新的一份，地址
/// 不能当身份：`{"allOf":[{"$ref":"#"}]}` 的 `allOf` 每经过一个成员就复制一次值，按地址认的话
/// 每次重入都像第一次进来，一路递归到栈溢出（`PORTING.md` 差异表 #69）。转换只取决于节点与值的内容，
/// 所以对象与数组按内容认：内容相同的两个值本来就转出同一个结果。原始值按值：`Object.is` 对数字区分
/// `-0` 与 `0`，字符串、布尔、null 按内容。`serde_json::Number` 会保留 `u64`/`f64` 的区别，所以数字
/// 一律先过 `as_f64()`——JS 里只有一个 `number`。
#[derive(Clone, PartialEq, Eq, Hash)]
enum Identity {
    Null,
    Bool(bool),
    String(String),
    Number(u64),
    /// 对象与数组：类型 + JSON 文本，同 `memo_key`。
    Content(String),
}

fn identity(value: &Value) -> Identity {
    match value {
        Value::Null => Identity::Null,
        Value::Bool(flag) => Identity::Bool(*flag),
        Value::String(text) => Identity::String(text.clone()),
        Value::Number(number) => Identity::Number(number.as_f64().map_or(0, f64::to_bits)),
        Value::Array(_) | Value::Object(_) => Identity::Content(memo_key(value)),
    }
}

/// 记忆用的值键：类型 + JSON 文本。`-0` 与 `0` 同键（`Map` 用 SameValueZero），`"1"` 与 `1` 不同。
fn memo_key(value: &Value) -> String {
    let kind = match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    format!("{kind}:{}", js::stringify(value))
}

impl<'a> Coercer<'a> {
    /// 一台编译器与它编译的 schema。
    pub fn new(compiler: Compiler<'a>, schema: &'a Value) -> Self {
        Self {
            c: compiler,
            schema,
            entered: Vec::new(),
            cuts: 0,
            memo: None,
        }
    }

    /// `value` 纠正后的样子：只复制改过的路径；什么都没改时值不变。输入从不被修改。
    ///
    /// 交出去之前按 JS 的键序重建一遍：上游返回的是 JS 对象，枚举键的顺序是 JS 的（像数组下标的键
    /// 在前），`serde_json` 的 Map 只保住插入顺序。见 `PORTING.md` 差异表 #64。
    pub fn coerce(&mut self, value: &Value) -> Value {
        let schema = self.schema;
        let base = self.c.r.root_base;
        let without_nulls = self.drop_optional_nulls(value, schema, base);
        let converted = self.convert_only(&without_nulls);
        js::reorder(&normalize_numbers(&self.coerce_types(&converted, schema, base)))
    }

    /// 只跑第 2 趟，即 TypeBox 的 `Value.Convert`（TypeBox 的一致性测试跑的是它）。
    pub fn convert_only(&mut self, value: &Value) -> Value {
        let schema = self.schema;
        let base = self.c.r.root_base;
        let result = self.convert(schema, base, value);
        self.memo = None;
        self.cuts = 0;
        result
    }

    /// `node` 在重入点上转换 `value`：这一对已经在栈上就原样返回。
    fn reenter(
        &mut self,
        node: &'a Value,
        base: &str,
        value: &Value,
        memoize: bool,
    ) -> Value {
        let node_address = node as *const Value as usize;
        let key = identity(value);
        if self
            .entered
            .iter()
            .any(|(entered_node, entered_value)| {
                *entered_node == node_address && *entered_value == key
            })
        {
            self.cuts += 1;
            return value.clone();
        }
        // 对象与数组的身份已经是内容键；原始值的记忆键按类型加前缀（`"1"` 与 `1` 不同）。
        let key_for_memo = memoize.then(|| match &key {
            Identity::Content(content) => content.clone(),
            _ => memo_key(value),
        });
        if let Some(key_for_memo) = &key_for_memo {
            if let Some(known) = self
                .memo
                .as_ref()
                .and_then(|memo| memo.get(&node_address))
                .and_then(|values| values.get(key_for_memo))
            {
                return known.clone();
            }
        }
        let cuts = self.cuts;
        self.entered.push((node_address, key));
        let result = self.convert(node, base, value);
        self.entered.pop();
        if memoize && self.cuts == cuts {
            self.memo
                .get_or_insert_with(HashMap::new)
                .entry(node_address)
                .or_default()
                .insert(key_for_memo.expect("memoize"), result.clone());
        }
        result
    }

    /// `value` 合不合格；`node` 编译不出来时是 `None`。
    fn passes(&mut self, node: &'a Value, base: &str, value: &Value) -> Option<bool> {
        match self.c.check(node, base) {
            Ok(check) => Some(check(value)),
            Err(Failure::Error(_) | Failure::Restart) => None,
        }
    }

    fn base_of(&mut self, node: &'a Value, base: &str) -> String {
        if is_schema_object(node) {
            self.c.r.base_for(node, base)
        } else {
            base.to_string()
        }
    }

    // ---------------------------------------------------------------------------------------------------------------
    // 第 1 趟：可选属性上的 null
    // ---------------------------------------------------------------------------------------------------------------

    fn drop_optional_nulls(&mut self, value: &Value, schema: &'a Value, base: &str) -> Value {
        let Some(object) = schema.as_object() else {
            return value.clone();
        };
        let b = self.base_of(schema, base);
        if let Some(items) = value.as_array() {
            return Value::Array(
                items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| match item_schema_at(object, index) {
                        None => item.clone(),
                        Some(item_schema) => self.drop_optional_nulls(item, item_schema, &b),
                    })
                    .collect(),
            );
        }
        if !value.is_object() {
            return value.clone();
        }
        let properties = object.get("properties").and_then(Value::as_object);
        let patterns: Vec<(std::sync::Arc<regress::Regex>, &'a Value)> = object
            .get("patternProperties")
            .and_then(Value::as_object)
            .map(|patterns| {
                js::entries(patterns)
                    .into_iter()
                    .filter_map(|(pattern, item)| shared_regex(pattern).ok().map(|re| (re, item)))
                    .collect()
            })
            .unwrap_or_default();
        let additional = object
            .get("additionalProperties")
            .filter(|value| value.is_object());
        if patterns.is_empty() && additional.is_none() && properties.is_none() {
            return value.clone();
        }
        let required: Vec<&str> = object
            .get("required")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let input = value.as_object().expect("object value");
        let mut out: Option<Map<String, Value>> = None;
        // 上游 `set`：值没变就不复制、不写。
        macro_rules! set {
            ($key:expr, $next:expr) => {{
                let next = $next;
                let current = match out.as_ref() {
                    Some(out) => out.get($key),
                    None => input.get($key),
                };
                if !current.is_some_and(|current| same_value(current, &next)) {
                    let copied = out.get_or_insert_with(|| input.clone());
                    copied.insert($key.to_string(), next);
                }
            }};
        }
        if let Some(properties) = properties {
            for (key, property) in js::entries(properties) {
                let Some(current) = input.get(key) else {
                    continue;
                };
                let is_ref = property
                    .as_object()
                    .and_then(|object| object.get("$ref"))
                    .is_some_and(Value::is_string);
                let dropped = current.is_null()
                    && !required.contains(&key.as_str())
                    && !is_ref
                    && self.passes(property, &b, &Value::Null) == Some(false);
                if dropped {
                    // `shift_remove`：JS 的 `delete` 不挪动其余的键，`Map::remove` 会把最后一项
                    // 换到空出来的位置上（`preserve_order` 下是 `IndexMap::swap_remove`）。
                    out.get_or_insert_with(|| input.clone()).shift_remove(key);
                    continue;
                }
                let next = self.drop_optional_nulls(current, property, &b);
                set!(key, next);
            }
        }
        // pattern 与 additional 属性值里的可选 null 照样走；键本身不是可选属性，不会被丢掉。
        if !patterns.is_empty() || additional.is_some() {
            for (key, original) in js::entries(input) {
                let mut matched = properties.is_some_and(|properties| properties.contains_key(key));
                for (re, item) in &patterns {
                    if re.find(key).is_none() {
                        continue;
                    }
                    matched = true;
                    let current = match out.as_ref() {
                        Some(out) => out.get(key),
                        None => input.get(key),
                    };
                    let next = self.drop_optional_nulls(current.expect("key of the value"), item, &b);
                    set!(key, next);
                }
                if !matched {
                    if let Some(additional) = additional {
                        let next = self.drop_optional_nulls(original, additional, &b);
                        set!(key, next);
                    }
                }
            }
        }
        match out {
            Some(out) => Value::Object(out),
            None => value.clone(),
        }
    }

    // ---------------------------------------------------------------------------------------------------------------
    // 第 2 趟：TypeBox 的转换规则
    // ---------------------------------------------------------------------------------------------------------------

    fn convert(&mut self, node: &'a Value, base: &str, value: &Value) -> Value {
        let Some(object) = node.as_object() else {
            return value.clone();
        };
        let b = self.base_of(node, base);
        if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
            let Ok(target) = self.c.r.resolve(reference, &b) else {
                return value.clone();
            };
            return if is_schema_object(target.node) {
                self.reenter(target.node, &target.base, value, true)
            } else {
                value.clone()
            };
        }
        self.convert_node(node, &b, value)
    }

    /// 一个 schema 节点的转换：`const`、`enum`、联合、`allOf`、`type` 里第一个适用的。
    fn convert_node(&mut self, node: &'a Value, b: &str, value: &Value) -> Value {
        let object = node.as_object().expect("checked by convert()");
        if let Some(expected) = object.get("const") {
            return convert_literal(expected, value);
        }
        if let Some(values) = object.get("enum").and_then(Value::as_array) {
            return self.convert_enum(node, values, b, value);
        }
        // 联合或交叠的节点也可能自己声明成员（普通 JSON Schema；TypeBox 从不这样）：成员在分支之后转换。
        if let Some(branches) = object.get("anyOf").and_then(Value::as_array) {
            return self.convert_union(node, branches, b, value);
        }
        if let Some(branches) = object.get("oneOf").and_then(Value::as_array) {
            return self.convert_union(node, branches, b, value);
        }
        if let Some(members) = object.get("allOf").and_then(Value::as_array) {
            let mut next = value.clone();
            for member in members {
                next = self.convert(member, b, &next);
            }
            return self.convert_members(node, b, &next);
        }
        let Some(declared) = object.get("type") else {
            if object.contains_key("properties") || object.contains_key("patternProperties") {
                return self.convert_object(object, b, value);
            }
            if object.contains_key("items") || object.contains_key("prefixItems") {
                return self.convert_array(object, b, value);
            }
            return value.clone();
        };
        // 上游 `switch (node.type)`：type 是数组（或别的非字符串）时落到 default，值原样返回。
        let Some(name) = declared.as_str() else {
            return value.clone();
        };
        match name {
            "string" => or_value(try_string(value), value),
            "number" => or_value(try_number(value).map(number_value), value),
            "integer" => match try_number(value) {
                Some(result) => number_value(result.trunc()),
                None => value.clone(),
            },
            "boolean" => or_value(try_boolean(value), value),
            "null" => or_value(try_null(value), value),
            // bigint 与 undefined 在 JSON 里没有对应值：值原样留着，交给检查去报。
            "bigint" | "undefined" => value.clone(),
            "array" => self.convert_array(object, b, value),
            "object" => self.convert_object(object, b, value),
            _ => value.clone(),
        }
    }

    /// 枚举是取值的联合：值本身是成员就留着，否则取输入能转换成的第一个成员。
    fn convert_enum(
        &mut self,
        node: &'a Value,
        values: &'a [Value],
        base: &str,
        value: &Value,
    ) -> Value {
        if values.iter().any(|member| deep_equal(member, value)) {
            return value.clone();
        }
        for member in values {
            let candidate = convert_literal(member, value);
            if !same_value(&candidate, value) && self.passes(node, base, &candidate) == Some(true) {
                return candidate;
            }
        }
        value.clone()
    }

    /// 一支就收下的值留着；否则取第一支转换后整个联合收下的。分支按顺序转换，且只到有一支被收下为止，
    /// 所以后面的递归分支不会被无谓地进去。自己声明了成员的节点（普通 JSON Schema；TypeBox 从不这样）
    /// 会把成员先转换进每个候选，再交给检查，免得一支因为一个随后才转换的兄弟属性被丢掉。
    fn convert_union(
        &mut self,
        node: &'a Value,
        branches: &'a [Value],
        base: &str,
        value: &Value,
    ) -> Value {
        if branches
            .iter()
            .any(|branch| self.passes(branch, base, value) == Some(true))
        {
            return self.convert_members(node, base, value);
        }
        for branch in branches {
            let converted = self.convert(branch, base, value);
            let candidate = self.convert_members(node, base, &converted);
            if self.passes(node, base, &candidate) == Some(true) {
                return candidate;
            }
        }
        self.convert_members(node, base, value)
    }

    /// 联合或交叠节点自己声明的对象成员（如果声明了）。
    fn convert_members(&mut self, node: &'a Value, base: &str, value: &Value) -> Value {
        let Some(object) = node.as_object() else {
            return value.clone();
        };
        let declares = object.contains_key("properties")
            || object.contains_key("patternProperties")
            || object.contains_key("additionalProperties");
        if declares {
            self.convert_object(object, base, value)
        } else {
            value.clone()
        }
    }

    fn convert_array(&mut self, node: &'a Map<String, Value>, base: &str, value: &Value) -> Value {
        let prefix = match node.get("items") {
            Some(items) if items.is_array() => items.as_array(),
            _ => node.get("prefixItems").and_then(Value::as_array),
        };
        if let Some(prefix) = prefix {
            // 元组转换它的项，从不把单个值包起来。
            let Some(items) = value.as_array() else {
                return value.clone();
            };
            let rest = if node.get("items").is_some_and(Value::is_array) {
                None
            } else {
                node.get("items")
            };
            return Value::Array(
                items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        let schema = if index < prefix.len() {
                            prefix.get(index)
                        } else {
                            rest
                        };
                        match schema {
                            Some(schema) => self.convert(schema, base, item),
                            None => item.clone(),
                        }
                    })
                    .collect(),
            );
        }
        if let Some(items) = value.as_array() {
            let Some(schema) = node.get("items") else {
                return value.clone();
            };
            return Value::Array(
                items
                    .iter()
                    .map(|item| self.convert(schema, base, item))
                    .collect(),
            );
        }
        // 被包起来的单值由项 schema 拿同一个值转换：一个重入点。
        let wrapped = match node.get("items") {
            Some(item) if is_schema_object(item) => self.reenter(item, base, value, false),
            _ => value.clone(),
        };
        Value::Array(vec![wrapped])
    }

    fn convert_object(&mut self, node: &'a Map<String, Value>, base: &str, value: &Value) -> Value {
        let Some(input) = value.as_object() else {
            return value.clone();
        };
        let properties = node.get("properties").and_then(Value::as_object);
        let patterns: Vec<(std::sync::Arc<regress::Regex>, &'a Value)> = node
            .get("patternProperties")
            .and_then(Value::as_object)
            .map(|patterns| {
                js::entries(patterns)
                    .into_iter()
                    .filter_map(|(pattern, schema)| shared_regex(pattern).ok().map(|re| (re, schema)))
                    .collect()
            })
            .unwrap_or_default();
        let additional = node.get("additionalProperties").filter(|value| value.is_object());
        let mut out: Option<Map<String, Value>> = None;
        for (key, current) in js::entries(input) {
            let mut next = current.clone();
            let mut matched = properties.is_some_and(|properties| properties.contains_key(key));
            if matched {
                next = self.convert(
                    properties
                        .and_then(|properties| properties.get(key))
                        .expect("declared property"),
                    base,
                    &next,
                );
            }
            for (re, schema) in &patterns {
                if re.find(key).is_none() {
                    continue;
                }
                matched = true;
                next = self.convert(schema, base, &next);
            }
            if !matched {
                if let Some(additional) = additional {
                    next = self.convert(additional, base, &next);
                }
            }
            if !same_value(&next, current) {
                out.get_or_insert_with(|| input.clone())
                    .insert(key.clone(), next);
            }
        }
        match out {
            Some(out) => Value::Object(out),
            None => value.clone(),
        }
    }

    // ---------------------------------------------------------------------------------------------------------------
    // 第 3 趟：按 type 的原始类型纠正
    // ---------------------------------------------------------------------------------------------------------------

    fn coerce_types(&mut self, value: &Value, schema: &'a Value, base: &str) -> Value {
        let Some(object) = schema.as_object() else {
            return value.clone();
        };
        let b = self.base_of(schema, base);
        let mut next = value.clone();
        if let Some(members) = object.get("allOf").and_then(Value::as_array) {
            for member in members {
                next = self.coerce_types(&next, member, &b);
            }
        }
        if let Some(branches) = object.get("anyOf").and_then(Value::as_array) {
            next = self.coerce_union(&next, branches, &b);
        }
        if let Some(branches) = object.get("oneOf").and_then(Value::as_array) {
            next = self.coerce_union(&next, branches, &b);
        }
        let types = types_of(object);
        let matches_union_member =
            types.len() > 1 && types.iter().any(|name| matches_type(&next, name));
        if !types.is_empty() && !matches_union_member {
            for name in &types {
                let candidate = coerce_primitive(&next, name);
                if !same_value(&candidate, &next) {
                    next = candidate;
                    break;
                }
            }
        }
        if types.contains(&"object") && next.is_object() {
            next = self.coerce_properties(&next, object, &b);
        }
        if types.contains(&"array") && next.is_array() {
            next = self.coerce_items(&next, object, &b);
        }
        next
    }

    fn coerce_union(&mut self, value: &Value, branches: &'a [Value], base: &str) -> Value {
        for branch in branches {
            if self.passes(branch, base, value) == Some(true) {
                return value.clone();
            }
        }
        for branch in branches {
            let coerced = self.coerce_types(value, branch, base);
            if self.passes(branch, base, &coerced) == Some(true) {
                return coerced;
            }
        }
        value.clone()
    }

    fn coerce_properties(
        &mut self,
        value: &Value,
        schema: &'a Map<String, Value>,
        base: &str,
    ) -> Value {
        let input = value.as_object().expect("object value");
        let properties = schema.get("properties").and_then(Value::as_object);
        let patterns: Vec<(std::sync::Arc<regress::Regex>, &'a Value)> = schema
            .get("patternProperties")
            .and_then(Value::as_object)
            .map(|patterns| {
                js::entries(patterns)
                    .into_iter()
                    .filter_map(|(pattern, item)| shared_regex(pattern).ok().map(|re| (re, item)))
                    .collect()
            })
            .unwrap_or_default();
        let additional = schema
            .get("additionalProperties")
            .filter(|value| value.is_object());
        let mut out: Option<Map<String, Value>> = None;
        macro_rules! set {
            ($key:expr, $next:expr) => {{
                let next = $next;
                let current = match out.as_ref() {
                    Some(out) => out.get($key),
                    None => input.get($key),
                };
                if !current.is_some_and(|current| same_value(current, &next)) {
                    let copied = out.get_or_insert_with(|| input.clone());
                    copied.insert($key.to_string(), next);
                }
            }};
        }
        if let Some(properties) = properties {
            for (key, property) in js::entries(properties) {
                if let Some(current) = input.get(key) {
                    let next = self.coerce_types(current, property, base);
                    set!(key, next);
                }
            }
        }
        for (key, original) in js::entries(input) {
            let mut matched = properties.is_some_and(|properties| properties.contains_key(key));
            for (re, item) in &patterns {
                if re.find(key).is_none() {
                    continue;
                }
                matched = true;
                let current = match out.as_ref() {
                    Some(out) => out.get(key),
                    None => input.get(key),
                };
                let next = self.coerce_types(current.expect("key of the value"), item, base);
                set!(key, next);
            }
            if !matched {
                if let Some(additional) = additional {
                    let next = self.coerce_types(original, additional, base);
                    set!(key, next);
                }
            }
        }
        match out {
            Some(out) => Value::Object(out),
            None => value.clone(),
        }
    }

    fn coerce_items(&mut self, value: &Value, schema: &'a Map<String, Value>, base: &str) -> Value {
        let items = value.as_array().expect("array value");
        Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| match item_schema_at(schema, index) {
                    Some(item_schema) if is_schema_object(item_schema) => {
                        self.coerce_types(item, item_schema, base)
                    }
                    _ => item.clone(),
                })
                .collect(),
        )
    }
}

/// 数组第 `index` 项的 schema：元组的（`prefixItems`，或 draft 7 的 `items` 数组）取前缀里的，
/// `prefixItems` 之后取 `items`，否则每项都是 `items`。什么都不适用时是 `None`。
fn item_schema_at<'a>(schema: &'a Map<String, Value>, index: usize) -> Option<&'a Value> {
    if let Some(items) = schema.get("items").and_then(Value::as_array) {
        return items.get(index);
    }
    if let Some(prefix) = schema.get("prefixItems").and_then(Value::as_array) {
        if index < prefix.len() {
            return prefix.get(index);
        }
    }
    schema.get("items")
}

// -------------------------------------------------------------------------------------------------------------------
// TypeBox 对一个值的转换（TypeBox 1.3.27 的 value/convert/try）：给一个结果，没有就是 `None`
// -------------------------------------------------------------------------------------------------------------------

/// 一个 JS 的 `number` 写成 JSON：整数值用整数表示，和 `JSON.parse` 读同一份文本的结果一致。
///
/// 上游所有数字都是 double，`JSON.stringify(42)` 写 `42`；`serde_json` 的 `42.0` 会写成 `42.0`，
/// 核对接口、缓存前缀时就会和 pi 的文本不一样。`-0` 按 JS 的 `JSON.stringify` 写 `0`。
fn number_value(x: f64) -> Value {
    // `i64::MAX` 与 `u64::MAX` 都不是 double 能表示的数：上界取 2^63、2^64（不含），边界上的值
    // 走浮点，免得整数表示悄悄换成相邻的数。
    const I64_MIN: f64 = -9_223_372_036_854_775_808.0;
    const I64_LIMIT: f64 = 9_223_372_036_854_775_808.0;
    const U64_LIMIT: f64 = 18_446_744_073_709_551_616.0;
    if x.is_finite() && x.fract() == 0.0 {
        if (I64_MIN..I64_LIMIT).contains(&x) {
            return Value::Number(Number::from(x as i64));
        }
        if (0.0..U64_LIMIT).contains(&x) {
            return Value::Number(Number::from(x as u64));
        }
    }
    Value::Number(Number::from_f64(x).expect("a finite number"))
}

/// 两个值在 JS 的 `===` 下相同。对象按 JSON 相等处理：Rust 侧的值总是复制出来的，没有引用可保，
/// 值相同的对象不再复制，结论与上游一致。
fn same_value(a: &Value, b: &Value) -> bool {
    deep_equal(a, b)
}

/// 纠正结果里的数字按 JS 的写法归一：整数值用整数表示，`-0` 写成 `0`。
///
/// pi 的数字只有 double 一种：`10` 与 `10.0` 是同一个值，交给工具时 `JSON.stringify` 写 `10`。Rust
/// 侧 `serde_json` 会保住浮点写法，工具按整数读参数（`as_i64`/`as_u64`）就读不到。只动浮点表示：
/// 已经是整数的值原样留着，2^53 以上的整数因此不会被舍成 double（见 `PORTING.md` 差异表 #65、#70）。
fn normalize_numbers(value: &Value) -> Value {
    match value {
        Value::Number(number) if number.is_f64() => {
            let x = number.as_f64().expect("an f64 number");
            if x.is_finite() && x.fract() == 0.0 {
                number_value(x)
            } else {
                value.clone()
            }
        }
        Value::Array(items) => Value::Array(items.iter().map(normalize_numbers).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), normalize_numbers(value)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn or_value(result: Option<Value>, value: &Value) -> Value {
    match result {
        Some(result) => result,
        None => value.clone(),
    }
}

/// 从另一个类型的值里取一个字面量（`const`、枚举的一个取值），转换后正好等于它才取。
fn convert_literal(literal: &Value, value: &Value) -> Value {
    if deep_equal(literal, value) {
        return value.clone();
    }
    let result = match literal {
        Value::String(_) => try_string(value),
        Value::Number(_) => try_number(value).map(number_value),
        Value::Bool(_) => try_boolean(value),
        // bigint 字面量在 JSON 里没有对应值。
        _ => None,
    };
    match result {
        Some(result) if deep_equal(&result, literal) => result,
        _ => value.clone(),
    }
}

fn try_string(value: &Value) -> Option<Value> {
    match value {
        Value::Bool(_) => Some(Value::String(js::to_string_value(value))),
        Value::Number(number) => number
            .as_f64()
            .filter(|x| x.is_finite())
            .map(|x| Value::String(js::number(x))),
        Value::Null => Some(Value::String("null".to_string())),
        Value::String(_) => Some(value.clone()),
        Value::Array(_) | Value::Object(_) => None,
    }
}

/// JavaScript 的 `Number.MAX_SAFE_INTEGER` / `MIN_SAFE_INTEGER`。
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

fn try_number(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(flag) => Some(if *flag { 1.0 } else { 0.0 }),
        Value::Number(number) => number.as_f64().filter(|x| x.is_finite()),
        Value::Null => Some(0.0),
        Value::String(text) => {
            if let Some(coerced) = number_from_string(text) {
                if coerced.is_finite() {
                    return Some(coerced);
                }
            }
            let lowercase = text.to_lowercase();
            if lowercase == "false" {
                return Some(0.0);
            }
            if lowercase == "true" {
                return Some(1.0);
            }
            let big = bigint_of_string(text)?;
            (big.abs() <= MAX_SAFE_INTEGER).then_some(big)
        }
        Value::Array(_) | Value::Object(_) => None,
    }
}

/// TypeBox 的 `TryBigInt` 对一个字符串：`12n`、`12.5`（截断）或 `12`；true/false 不会从
/// `tryNumber` 走到这里。
fn bigint_of_string(text: &str) -> Option<f64> {
    let digits = text.strip_suffix('n');
    if let Some(digits) = digits {
        return integer_text(digits);
    }
    if let Some((integer, fraction)) = text.split_once('.') {
        if !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit()) {
            return integer_text(integer);
        }
        return None;
    }
    integer_text(text)
}

/// `^-?(0|[1-9][0-9]*)$` 的十进制整数，转成 `f64`。
fn integer_text(text: &str) -> Option<f64> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(digits) => (true, digits),
        None => (false, text),
    };
    if digits.is_empty()
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
        || (digits.len() > 1 && digits.starts_with('0'))
    {
        return None;
    }
    let mut value = 0f64;
    for digit in digits.bytes() {
        value = value * 10.0 + f64::from(digit - b'0');
    }
    Some(if negative { -value } else { value })
}

/// TypeBox 的 `TryBoolean`：`"true"`/`"false"`（不分大小写）、`"1"`/`"0"`、1/0，null 是 false。
fn try_boolean(value: &Value) -> Option<Value> {
    match value {
        Value::Bool(_) => Some(value.clone()),
        Value::Number(number) => {
            let x = number.as_f64().filter(|x| x.is_finite())?;
            if x == 0.0 {
                Some(Value::Bool(false))
            } else if x == 1.0 {
                Some(Value::Bool(true))
            } else {
                None
            }
        }
        Value::Null => Some(Value::Bool(false)),
        Value::String(text) => {
            let lowercase = text.to_lowercase();
            if lowercase == "false" || text == "0" {
                Some(Value::Bool(false))
            } else if lowercase == "true" || text == "1" {
                Some(Value::Bool(true))
            } else {
                None
            }
        }
        Value::Array(_) | Value::Object(_) => None,
    }
}

/// TypeBox 的 `TryNull`：`"null"`、`"undefined"`、`""`、`"0"`、0 与 false。
fn try_null(value: &Value) -> Option<Value> {
    match value {
        Value::Bool(flag) => (!*flag).then_some(Value::Null),
        Value::Number(number) => (number.as_f64() == Some(0.0)).then_some(Value::Null),
        Value::Null => Some(Value::Null),
        Value::String(text) => {
            let lowercase = text.to_lowercase();
            (lowercase == "undefined"
                || lowercase == "null"
                || text.is_empty()
                || text == "0")
                .then_some(Value::Null)
        }
        Value::Array(_) | Value::Object(_) => None,
    }
}

// -------------------------------------------------------------------------------------------------------------------
// 按 type 的原始类型纠正（第 3 趟）
// -------------------------------------------------------------------------------------------------------------------

fn types_of(schema: &Map<String, Value>) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(name)) => vec![name.as_str()],
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn matches_type(value: &Value, name: &str) -> bool {
    match name {
        "number" => is_finite(value),
        "integer" => is_integer(value),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "null" => value.is_null(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

fn is_finite(value: &Value) -> bool {
    value.as_f64().is_some_and(f64::is_finite)
}

fn is_integer(value: &Value) -> bool {
    value
        .as_f64()
        .is_some_and(|x| x.is_finite() && x.fract() == 0.0)
}

fn coerce_primitive(value: &Value, name: &str) -> Value {
    match name {
        "number" | "integer" => {
            if value.is_null() {
                return number_value(0.0);
            }
            if let Some(text) = value.as_str() {
                if !text.trim().is_empty() {
                    if let Some(parsed) = number_from_string(text) {
                        if name == "number" {
                            if parsed.is_finite() {
                                return number_value(parsed);
                            }
                        } else if parsed.is_finite() && parsed.fract() == 0.0 {
                            return number_value(parsed);
                        }
                    }
                }
            }
            if let Some(flag) = value.as_bool() {
                return number_value(if flag { 1.0 } else { 0.0 });
            }
            value.clone()
        }
        "boolean" => {
            if value.is_null() {
                return Value::Bool(false);
            }
            if let Some(text) = value.as_str() {
                if text == "true" {
                    return Value::Bool(true);
                }
                if text == "false" {
                    return Value::Bool(false);
                }
            }
            if let Some(number) = value.as_f64() {
                if number == 1.0 {
                    return Value::Bool(true);
                }
                if number == 0.0 {
                    return Value::Bool(false);
                }
            }
            value.clone()
        }
        "string" => {
            if value.is_null() {
                return Value::String(String::new());
            }
            if value.is_number() || value.is_boolean() {
                return Value::String(js::to_string_value(value));
            }
            value.clone()
        }
        "null" => {
            if value.as_str() == Some("")
                || value.as_f64() == Some(0.0)
                || value.as_bool() == Some(false)
            {
                Value::Null
            } else {
                value.clone()
            }
        }
        _ => value.clone(),
    }
}

/// JavaScript 的 `Number(text)`，即字符串到数的转换：先按 `StringNumericLiteral` 读，读不出来是 NaN。
///
/// 与 Rust 的 `f64::from_str` 不同：JS 认 `0x`/`0o`/`0b` 前缀、认 `Infinity`，不认 `inf`/`nan`，
/// 两边对空白、正负号与小数点的写法也有出入。
fn number_from_string(text: &str) -> Option<f64> {
    let trimmed = trim_js_whitespace(text);
    if trimmed.is_empty() {
        return Some(0.0);
    }
    match trimmed {
        "Infinity" | "+Infinity" => return Some(f64::INFINITY),
        "-Infinity" => return Some(f64::NEG_INFINITY),
        _ => {}
    }
    // 带前缀的整数不能有正负号（`Number("+0x10")` 是 NaN）。
    if trimmed.len() > 2 && !trimmed.starts_with(['+', '-']) {
        let bytes = trimmed.as_bytes();
        let radix = match &bytes[..2] {
            b"0x" | b"0X" => Some(16u32),
            b"0o" | b"0O" => Some(8u32),
            b"0b" | b"0B" => Some(2u32),
            _ => None,
        };
        if let Some(radix) = radix {
            let digits = &trimmed[2..];
            if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
                return None;
            }
            return Some(radix_number(digits, radix));
        }
    }
    if !is_decimal_literal(trimmed) {
        return None;
    }
    trimmed.parse::<f64>().ok()
}

/// 一个带前缀的整数（`0x…`/`0o…`/`0b…`）按浮点累加：JS 会先算精确整数再舍入，短数字两者相同；
/// 超长的数字没有真实用例，这里不做大整数。
fn radix_number(digits: &str, radix: u32) -> f64 {
    let mut value = 0f64;
    for character in digits.chars() {
        let digit = character.to_digit(radix).expect("checked digit");
        value = value * f64::from(radix) + f64::from(digit);
    }
    value
}

/// JS 十进制字面量：`[+-]? (digits [. digits?]? | . digits) ([eE] [+-]? digits)?`。
fn is_decimal_literal(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    let mut digits = i - start;
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        let fraction = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        digits += i - fraction;
    }
    if digits == 0 {
        return false;
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let exponent = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == exponent {
            return false;
        }
    }
    i == bytes.len()
}

/// JS `String.prototype.trim` 认的空白与行终止符。
///
/// 与 Rust 的 `char::is_whitespace` 几乎一样，只差 U+0085（JS 不认）和 U+FEFF（JS 认）。
fn trim_js_whitespace(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

fn is_js_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}
