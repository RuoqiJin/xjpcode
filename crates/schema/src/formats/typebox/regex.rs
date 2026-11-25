//! 对应上游 `packages/schema/src/formats/typebox/regex.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use regress::Regex;

/// 值是不是一个合法的正则串。
///
/// https://ecma-international.org/ecma-262 ；上游用 `new RegExp(value, "u")` 试编译。
pub fn is_regex(value: &str) -> bool {
    Regex::with_flags(value, "u").is_ok()
}
