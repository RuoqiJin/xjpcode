//! 对应上游 `packages/schema/src/formats/typebox/json_pointer_uri_fragment.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use std::sync::LazyLock;

use regress::Regex;

static JSON_POINTER_URI_FRAGMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::with_flags(
        r##"^#(?:\/(?:[a-z0-9_\-.!$&'()*+,;:=@]|%[0-9a-f]{2}|~0|~1)*)*$"##,
        "i",
    )
    .expect("JsonPointerUriFragment format pattern")
});

/// Returns true if the value is a json pointer uri fragment
pub fn is_json_pointer_uri_fragment(value: &str) -> bool {
    JSON_POINTER_URI_FRAGMENT.find(value).is_some()
}
