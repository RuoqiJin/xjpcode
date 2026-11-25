//! 对应上游 `packages/schema/src/formats/typebox/relative_json_pointer.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use std::sync::LazyLock;

use regress::Regex;

static RELATIVE_JSON_POINTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::with_flags(r##"^(?:0|[1-9][0-9]*)(?:#|(?:\/(?:[^~/]|~0|~1)*)*)$"##, "")
        .expect("RelativeJsonPointer format pattern")
});

/// Returns true if the value is a relative json pointer
pub fn is_relative_json_pointer(value: &str) -> bool {
    RELATIVE_JSON_POINTER.find(value).is_some()
}
