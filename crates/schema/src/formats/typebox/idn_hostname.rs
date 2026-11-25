//! 对应上游 `packages/schema/src/formats/typebox/idn_hostname.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use super::idna;

/// 值是不是一个合法的国际化（IDN）主机名。
///
/// https://tools.ietf.org/html/rfc3490 、rfc5891 、rfc5892 、rfc5893
pub fn is_idn_hostname(value: &str) -> bool {
    idna::is_idn_hostname(value)
}
