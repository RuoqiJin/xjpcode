//! 对应上游 `packages/schema/src/types.ts`。
//!
//! 上游这里只放公开类型：`compile()` 接受的格式、它的选项，以及检查器的形状。值类型 `unknown`
//! 在本仓固定为 `serde_json::Value`（模型给的参数本来就是 JSON），见 `PORTING.md` 差异表。

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value};

/// 一份 JSON Schema：`true`、`false` 或一个 schema 对象。
///
/// 上游注释说明 schema 来自工具定义、MCP server 等不可信来源，所以编译时每个关键字都按“可能
/// 是任何值”读取，而不是先假定一个静态形状。Rust 侧同理：只保留 JSON 的三种形态。
pub type JsonSchema = Value;

/// 一个格式检查：`value`（字符串）是否满足该格式。
pub type FormatCheck = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// 一个值的一处不合格：在哪里（JSON Pointer，值是它自己时是 `""`）、哪个关键字、说了什么。
///
/// 它会被原样回给模型看（工具参数不对时），所以三个字段的文字与上游逐字相同。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub path: String,
    pub keyword: String,
    pub message: String,
}

impl ValidationError {
    pub fn new(
        path: impl Into<String>,
        keyword: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            keyword: keyword.into(),
            message: message.into(),
        }
    }
}

/// 一个 schema 对象：任何关键字、任何取值，用到它的地方再逐个检查。
pub type SchemaObject = Map<String, Value>;

/// 编译出来的布尔检查器。
///
/// 上游是 `(value: unknown) => boolean`。Rust 侧要求 `Send + Sync`：循环会同时跑几个工具，
/// 它们共用同一个检查器（见 `PORTING.md` 的既定约束）。
pub type Check = Arc<dyn Fn(&Value) -> bool + Send + Sync>;

/// 适用哪一版的语义。上游是字面量联合 `"2020-12" | "7"`，默认 `"2020-12"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Draft {
    /// `"2020-12"`
    #[default]
    Draft202012,
    /// `"7"`
    Draft7,
}

/// 一个什么都没指到的 `$ref`（缺定义、`schemas` 里没有这个文档）怎么办。
///
/// 上游：`"throw"`（默认）让 compile 抛错；`"reject"` 把它编译成 `false`，只有那个位置上的值
/// 不合格，schema 其余部分照常工作。用于别人系统发来的 schema，比如 MCP server 的引用可能悬空。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnresolvedRefs {
    #[default]
    Throw,
    Reject,
}

/// `compile()` 的选项，对应上游 `CompileOptions`。
#[derive(Clone, Default)]
pub struct CompileOptions {
    /// 适用哪一版语义。上游：`draft?: "2020-12" | "7"`。
    pub draft: Draft,
    /// 格式名到检查器。不给时 `format` 只是注解，与 2020-12 的规定一致。
    pub formats: Option<BTreeMap<String, FormatCheck>>,
    /// 按 URI 登记的其它 schema 文档，供 `$ref` 跨文档引用。
    pub schemas: BTreeMap<String, JsonSchema>,
    /// 指空的 `$ref` 怎么办。
    pub unresolved_refs: UnresolvedRefs,
    /// 上游 `coerce?: boolean`：加上 `coerce()` 与 `parse()`。
    ///
    /// Rust 没有“按选项决定有没有这个方法”的类型：`Validator` 上一直有这两个方法（见
    /// `PORTING.md` 差异表）。纠正器按需现建，不给这个选项的调用方不调它们就不花代价。
    pub coerce: bool,
}

impl CompileOptions {
    /// 只有 `draft`，其余按上游默认值：没有 formats、没有额外文档、`$ref` 指空即抛错。
    pub fn new(draft: Draft) -> Self {
        Self {
            draft,
            ..Self::default()
        }
    }
}

// 格式检查是闭包，`Debug` 没法导出它；只把名字列出来。
impl std::fmt::Debug for CompileOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompileOptions")
            .field("draft", &self.draft)
            .field(
                "formats",
                &self
                    .formats
                    .as_ref()
                    .map(|formats| formats.keys().collect::<Vec<_>>()),
            )
            .field("schemas", &self.schemas.keys().collect::<Vec<_>>())
            .field("unresolved_refs", &self.unresolved_refs)
            .field("coerce", &self.coerce)
            .finish()
    }
}

/// `parse()` 的结果：合格时是纠正后的值，不合格时是纠正后值的错误。
///
/// 上游是 `{ ok: true; value } | { ok: false; errors }`（`ParseResult<T>`）；Rust 用枚举，两个
/// 变体各对应上游的一支。
#[derive(Debug, Clone, PartialEq)]
pub enum ParseResult {
    /// 上游 `{ ok: true, value }`。
    Ok { value: Value },
    /// 上游 `{ ok: false, errors }`。
    Errors { errors: Vec<ValidationError> },
}

/// `compile()` 失败：schema 本身不成形，或 `$ref` 指了不存在的东西。
///
/// 上游用 `throw new Error(...)` 表达；Rust 侧落成 `Result`，调用方不必靠 catch/panic 区分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    message: String,
}

impl CompileError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// 上游 `Error.message`。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CompileError {}
