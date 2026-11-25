//! 对应上游 `packages/schema/src/formats/typebox/idna/` 目录。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

pub mod format;
pub mod hostname;
pub mod idn_hostname;
pub mod label;
pub mod pattern;

pub use hostname::is_hostname;
pub use idn_hostname::is_idn_hostname;
