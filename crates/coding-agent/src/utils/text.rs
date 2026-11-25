//! 对应上游 `src/utils/text.ts`。

/// pi: `splitBom`——把开头的 UTF-8 BOM 分出来。
pub fn split_bom(content: &str) -> (&str, &str) {
    match content.strip_prefix('\u{FEFF}') {
        Some(text) => ("\u{FEFF}", text),
        None => ("", content),
    }
}

/// pi: `stripBom`
pub fn strip_bom(content: &str) -> &str {
    split_bom(content).1
}
