//! `@earendil-works/pi-schema` 的移植目标 crate 根，对应上游 `src/index.ts`。
//!
//! 目录层级与 `PORTING.md` 的文件对照表逐一对应。上游 `src/index.ts` 的整包 barrel 里的 builders 不在
//! 这一步的范围（它们不属于“检查对不对”的三块之一），本文件只做模块声明与本步已搬内容的再导出。

pub mod annotations;
pub mod checks;
pub mod coerce;
pub mod compiler;
pub mod explain;
pub mod formats;
pub mod js;
pub mod resolver;
pub mod shared;
pub mod types;
pub mod validator;

pub use checks::deep_equal as equal;
pub use coerce::Coercer;
pub use types::{
    Check, CompileError, CompileOptions, Draft, FormatCheck, JsonSchema, ParseResult,
    SchemaObject, UnresolvedRefs, ValidationError,
};
pub use validator::{compile, Validator};
