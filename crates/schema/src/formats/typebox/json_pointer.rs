//! 对应上游 `packages/schema/src/formats/typebox/json_pointer.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use std::sync::LazyLock;

use regress::Regex;

static JSON_POINTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::with_flags(r#"^(?:\/(?:[^~/]|~0|~1)*)*$"#, "").expect("JsonPointer format pattern")
});

/// Returns true if the value is a json pointer
pub fn is_json_pointer(value: &str) -> bool {
    JSON_POINTER.find(value).is_some()
}
