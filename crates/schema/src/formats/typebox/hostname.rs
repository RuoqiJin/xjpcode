//! 对应上游 `packages/schema/src/formats/typebox/hostname.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use super::idna;

/// 值是不是一个合法主机名。
///
/// https://tools.ietf.org/html/rfc1123 、rfc5891 、rfc5892
pub fn is_hostname(value: &str) -> bool {
    idna::is_hostname(value)
}
