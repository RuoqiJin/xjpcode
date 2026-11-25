//! 对应上游 `packages/schema/src/formats/typebox/idna/label/ascii.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use super::super::pattern::pattern::{
    RE_ASCII_LDH, RE_RULE_HYPHEN_PLACEMENT, RE_RULE_NOT_RESERVED_ACE,
};

/// 按 RFC 5891 §4.2.3.1 校验一个 ASCII LDH 标签。
pub fn is_ascii_label(value: &str) -> bool {
    RE_RULE_HYPHEN_PLACEMENT.find(value).is_some()
        && RE_RULE_NOT_RESERVED_ACE.find(value).is_some()
        && RE_ASCII_LDH.find(value).is_some()
}
