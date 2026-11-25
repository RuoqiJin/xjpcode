//! 对应上游 `packages/schema/src/formats/typebox/uuid.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use std::sync::LazyLock;

use regress::Regex;

static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::with_flags(r#"^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$"#, "i")
        .expect("Uuid format pattern")
});

/// Returns true if the value is a uuid
pub fn is_uuid(value: &str) -> bool {
    UUID.find(value).is_some()
}
