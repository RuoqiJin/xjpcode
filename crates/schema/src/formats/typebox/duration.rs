//! 对应上游 `packages/schema/src/formats/typebox/duration.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use std::sync::LazyLock;

use regress::Regex;

static DURATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::with_flags(
        r#"^P((\d+Y(\d+M(\d+D)?)?|\d+M(\d+D)?|\d+D)(T(\d+H(\d+M(\d+S)?)?|\d+M(\d+S)?|\d+S))?|T(\d+H(\d+M(\d+S)?)?|\d+M(\d+S)?|\d+S)|\d+W)$"#,
        "",
    )
    .expect("Duration format pattern")
});

/// Returns true if the value is a valid ISO-8601 duration.
pub fn is_duration(value: &str) -> bool {
    DURATION.find(value).is_some()
}
