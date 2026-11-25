//! 对应上游 `packages/schema/src/resolver.ts`。
//!
//! 一次编译的引用解析：schema 资源、锚点，以及根 schema 与已登记文档上的 JSON Pointer。`Resolver`
//! 只在 schema 编译期间活着（将来解释错误时也一样）。

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use serde_json::Value;
use url::Url;

use crate::js;
use crate::types::{CompileError, CompileOptions, Draft, JsonSchema};

/// 一份 schema 和它的引用所对照的基地址。
#[derive(Debug, Clone)]
pub struct Resolved<'a> {
    pub node: &'a Value,
    pub base: String,
}

/// 没有 `$id` 的根 schema 的基地址；相对引用对照它解析。
pub const DEFAULT_BASE: &str = "https://json-schema.invalid/root.json";

/// 上游 `unresolvedRefs: "reject"` 编译出来的 `false` schema。
static FALSE_SCHEMA: LazyLock<Value> = LazyLock::new(|| Value::Bool(false));

/// 解析根 schema 与已登记文档里的引用。
///
/// 上游的做法：第一次遇到引用或第一个 `$id` 时才开始索引，两者都没有的 schema 因此一次都不用走。
pub struct Resolver<'a> {
    pub draft: Draft,
    /// 根 schema 开始走的前缀地址；走的时候由 `base_for` 应用根本身的 `$id`。
    pub root_base: &'static str,
    /// schema 对象（按地址）到它被索引时的基地址。
    pub base_of: HashMap<usize, String>,
    /// 每个 `$dynamicAnchor` 的 URI（带 `#name`）到它的 schema。
    pub dynamic_anchors: HashMap<String, &'a Value>,
    root: &'a JsonSchema,
    options: &'a CompileOptions,
    registry: HashMap<String, &'a JsonSchema>,
    reject_unresolved: bool,
    /// 什么都没指到、按 `false` 解析的引用（`unresolvedRefs: "reject"`）。
    unresolved: HashSet<String>,
    resources: HashMap<String, &'a Value>,
    anchors: HashMap<String, &'a Value>,
    indexed: bool,
}

impl<'a> Resolver<'a> {
    pub fn new(root: &'a JsonSchema, options: &'a CompileOptions) -> Self {
        Self {
            draft: options.draft,
            root_base: DEFAULT_BASE,
            base_of: HashMap::new(),
            dynamic_anchors: HashMap::new(),
            root,
            options,
            registry: HashMap::new(),
            reject_unresolved: options.unresolved_refs == crate::types::UnresolvedRefs::Reject,
            unresolved: HashSet::new(),
            resources: HashMap::new(),
            anchors: HashMap::new(),
            indexed: false,
        }
    }

    /// `node` 的基地址，父级用 `base`：它的 `$id` 会改它——除非 draft 7 让这个 `$id` 失效。
    /// schema 已经索引过之后，用索引时的基地址（解析得到的引用目标已经带自己的了）。
    pub fn base_for(&mut self, node: &'a Value, base: &str) -> String {
        if let Some(indexed) = self.base_of.get(&address_of(node)) {
            return indexed.clone();
        }
        let Some(object) = node.as_object() else {
            return base.to_string();
        };
        let Some(id) = object.get("$id").and_then(Value::as_str) else {
            return base.to_string();
        };
        // 应用 `$id` 之前先索引：调用方给的 `base` 可能已经含它，而索引只应用一次。
        if !self.indexed {
            self.ensure_indexed();
            if let Some(indexed) = self.base_of.get(&address_of(node)) {
                return indexed.clone();
            }
        }
        if self.draft == Draft::Draft7
            && (object.get("$ref").is_some_and(Value::is_string) || id.starts_with('#'))
        {
            return base.to_string();
        }
        document_of(&resolve_uri(id, base)).to_string()
    }

    /// 索引根 schema 与已登记文档的 URI（只做一次）。
    pub fn ensure_indexed(&mut self) {
        if self.indexed {
            return;
        }
        self.indexed = true;
        for (uri, schema) in &self.options.schemas {
            let doc = document_of(&resolve_uri(uri, DEFAULT_BASE)).to_string();
            self.registry.insert(doc, schema);
        }
        self.resources.insert(DEFAULT_BASE.to_string(), self.root);
        self.index(self.root, DEFAULT_BASE);
    }

    /// 引用指到的 schema，连同它编译时的基地址。
    ///
    /// 指空时返回错误；`unresolvedRefs: "reject"` 下改解析成 `false`，也就是没有任何值能通过的 schema。
    pub fn resolve(&mut self, reference: &str, base: &str) -> Result<Resolved<'a>, CompileError> {
        self.ensure_indexed();
        if !self.reject_unresolved {
            return self.find(reference, base);
        }
        match self.find(reference, base) {
            Ok(resolved) => Ok(resolved),
            Err(_) => {
                self.unresolved.insert(resolve_uri(reference, base));
                Ok(Resolved {
                    node: &FALSE_SCHEMA,
                    base: base.to_string(),
                })
            }
        }
    }

    /// `reference`（从 `base` 出发）是不是什么都没指到、按 `false` 解析过。
    pub fn is_unresolved(&self, reference: &str, base: &str) -> bool {
        self.unresolved.contains(&resolve_uri(reference, base))
    }

    fn find(&mut self, reference: &str, base: &str) -> Result<Resolved<'a>, CompileError> {
        let uri = resolve_uri(reference, base);
        let doc = document_of(&uri);
        let fragment = if uri.len() > doc.len() {
            decode_uri_component(&uri[doc.len() + 1..])?
        } else {
            String::new()
        };
        let root = match self.resources.get(doc) {
            Some(root) => *root,
            None => self.load(doc).ok_or_else(|| {
                CompileError::new(format!(
                    "Unresolved $ref {reference} (no schema registered for {doc})"
                ))
            })?,
        };
        if fragment.is_empty() {
            return Ok(Resolved {
                node: root,
                base: doc.to_string(),
            });
        }
        if fragment.starts_with('/') {
            return self.pointer(root, doc, &fragment);
        }
        let key = format!("{doc}#{fragment}");
        let Some(target) = self.anchors.get(&key).copied() else {
            return Err(CompileError::new(format!(
                "Unresolved $ref {reference} (no anchor {fragment} in {doc})"
            )));
        };
        Ok(Resolved {
            node: target,
            base: self
                .base_of
                .get(&address_of(target))
                .cloned()
                .unwrap_or_else(|| doc.to_string()),
        })
    }

    /// `node` 是不是 `base` 上那份 schema 资源的根。
    pub fn is_resource(&mut self, node: &'a Value, base: &str) -> bool {
        self.ensure_indexed();
        self.resources
            .get(base)
            .is_some_and(|root| std::ptr::eq(*root, node))
    }

    // 只走装子 schema 的关键字：`enum`、`const` 或未知关键字里的 `$id` 什么都不命名。
    fn index(&mut self, node: &'a Value, base: &str) {
        let Some(object) = node.as_object() else {
            return;
        };
        if self.base_of.contains_key(&address_of(node)) {
            return;
        }
        let mut current = base.to_string();
        // draft 7 里有了 `$ref`，兄弟关键字（含 `$id`）全部失效。
        let ref_only =
            self.draft == Draft::Draft7 && object.get("$ref").is_some_and(Value::is_string);
        if !ref_only {
            if let Some(id) = object.get("$id").and_then(Value::as_str) {
                if self.draft == Draft::Draft7 && id.starts_with('#') {
                    self.anchors
                        .insert(format!("{}{id}", document_of(&current)), node);
                } else {
                    let uri = resolve_uri(id, &current);
                    current = document_of(&uri).to_string();
                    self.resources.insert(current.clone(), node);
                    if uri.len() > current.len() + 1 {
                        self.anchors.insert(uri, node);
                    }
                }
            }
        }
        self.base_of.insert(address_of(node), current.clone());
        if ref_only {
            return;
        }
        if self.draft != Draft::Draft7 {
            if let Some(anchor) = object.get("$anchor").and_then(Value::as_str) {
                self.anchors.insert(format!("{current}#{anchor}"), node);
            }
            if let Some(anchor) = object.get("$dynamicAnchor").and_then(Value::as_str) {
                self.anchors.insert(format!("{current}#{anchor}"), node);
                self.dynamic_anchors
                    .insert(format!("{current}#{anchor}"), node);
            }
        }
        for key in SCHEMA_KEYWORDS {
            if let Some(child) = object.get(*key) {
                self.index(child, &current);
            }
        }
        for key in MAP_KEYWORDS {
            let Some(map) = object.get(*key).and_then(Value::as_object) else {
                continue;
            };
            // 上游 `for (const k of Object.keys(map))`：同名 `$id` 的胜者跟着 JS 的键序走。
            for (_, child) in js::entries(map) {
                self.index(child, &current);
            }
        }
        for key in ARRAY_KEYWORDS {
            let Some(list) = object.get(*key).and_then(Value::as_array) else {
                continue;
            };
            for child in list {
                self.index(child, &current);
            }
        }
    }

    fn load(&mut self, doc: &str) -> Option<&'a JsonSchema> {
        let schema = *self.registry.get(doc)?;
        self.resources.insert(doc.to_string(), schema);
        self.index(schema, doc);
        Some(schema)
    }

    fn pointer(
        &mut self,
        root: &'a Value,
        base: &str,
        fragment: &str,
    ) -> Result<Resolved<'a>, CompileError> {
        let mut node = root;
        let mut current = base.to_string();
        for raw in fragment.split('/').skip(1) {
            let key = raw.replace("~1", "/").replace("~0", "~");
            node = pointer_member(node, &key).ok_or_else(|| {
                CompileError::new(format!("Unresolved $ref #{fragment} in {base}"))
            })?;
            if node.is_object() {
                if let Some(indexed) = self.base_of.get(&address_of(node)) {
                    current = indexed.clone();
                }
            }
        }
        Ok(Resolved {
            node,
            base: current,
        })
    }
}

/// `node[key]`，按 JS 里对象与数组的己有成员查找。
fn pointer_member<'a>(node: &'a Value, key: &str) -> Option<&'a Value> {
    match node {
        Value::Object(map) => map.get(key),
        // JS 的数组也是对象：`"0"` 是己有下标，`"length"` 是己有属性。
        Value::Array(list) => {
            if key == "length" {
                return None; // 上游会拿到一个 number，这里没有可返回的 &Value；见 PORTING.md。
            }
            let index = key.parse::<usize>().ok()?;
            if index.to_string() != key {
                return None;
            }
            list.get(index)
        }
        _ => None,
    }
}

const SCHEMA_KEYWORDS: &[&str] = &[
    "additionalProperties",
    "propertyNames",
    "contains",
    "not",
    "if",
    "then",
    "else",
    "unevaluatedProperties",
    "unevaluatedItems",
    "additionalItems",
    "contentSchema",
    "items",
];

const MAP_KEYWORDS: &[&str] = &[
    "properties",
    "patternProperties",
    "$defs",
    "definitions",
    "dependentSchemas",
    "dependencies",
];

const ARRAY_KEYWORDS: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems", "items"];

/// `value` 是不是一个 schema 对象（不是 `true`/`false`、数组或原始值）。
pub fn is_schema_object(value: &Value) -> bool {
    value.is_object()
}

/// 一个 schema 节点的地址，用来把它当身份用（上游按对象引用记缓存和索引）。
fn address_of(node: &Value) -> usize {
    node as *const Value as usize
}

/// `reference` 对照 `base` 解析后的绝对 URI。
///
/// 上游是 `new URL(ref, base).href`；URL 解析器不认的基地址（URN 之类）只接受 fragment 引用。
pub fn resolve_uri(reference: &str, base: &str) -> String {
    let joined = Url::parse(base)
        .ok()
        .and_then(|base| base.join(reference).ok());
    match joined {
        Some(url) => url.to_string(),
        None => {
            if reference.starts_with('#') {
                format!("{}{}", document_of(base), reference)
            } else {
                reference.to_string()
            }
        }
    }
}

/// `uri` 去掉 fragment。
pub fn document_of(uri: &str) -> &str {
    match uri.find('#') {
        Some(hash) => &uri[..hash],
        None => uri,
    }
}

/// 上游 `decodeURIComponent`：解 `%XX` 成字节再按 UTF-8 解码，坏掉的转义或字节序列报错。
fn decode_uri_component(fragment: &str) -> Result<String, CompileError> {
    let bytes = fragment.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let valid = i + 2 < bytes.len()
                && bytes[i + 1].is_ascii_hexdigit()
                && bytes[i + 2].is_ascii_hexdigit();
            if !valid {
                return Err(CompileError::new(format!(
                    "URI malformed in $ref fragment {fragment}"
                )));
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    percent_encoding::percent_decode_str(fragment)
        .decode_utf8()
        .map(|decoded| decoded.into_owned())
        .map_err(|_| CompileError::new(format!("URI malformed in $ref fragment {fragment}")))
}
