//! 对应上游 `packages/schema/src/formats/typebox/` 目录。
//!
//! 格式检查改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

pub mod date;
pub mod date_time;
pub mod duration;
pub mod email;
pub mod hostname;
pub mod idn_email;
pub mod idn_hostname;
pub mod idna;
pub mod ipv4;
pub mod ipv6;
pub mod iri;
pub mod iri_reference;
pub mod json_pointer;
pub mod json_pointer_uri_fragment;
pub mod regex;
pub mod relative_json_pointer;
pub mod time;
pub mod uri;
pub mod uri_reference;
pub mod uri_template;
pub mod url;
pub mod uuid;
