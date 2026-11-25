//! 对应上游 `packages/schema/src/formats/index.ts`。
//!
//! JSON Schema 2020-12 定义的格式检查，供 `compile()` 的 `formats` 选项使用。这是一个可选的入口：
//! 不传它的 schema 把 `format` 当注解，也不会带上这些检查。

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use super::typebox::date::is_date;
use super::typebox::date_time::is_date_time;
use super::typebox::duration::is_duration;
use super::typebox::email::is_email;
use super::typebox::hostname::is_hostname;
use super::typebox::idn_email::is_idn_email;
use super::typebox::idn_hostname::is_idn_hostname;
use super::typebox::ipv4::is_ipv4;
use super::typebox::ipv6::is_ipv6;
use super::typebox::iri::is_iri;
use super::typebox::iri_reference::is_iri_reference;
use super::typebox::json_pointer::is_json_pointer;
use super::typebox::json_pointer_uri_fragment::is_json_pointer_uri_fragment;
use super::typebox::regex::is_regex;
use super::typebox::relative_json_pointer::is_relative_json_pointer;
use super::typebox::time::is_time;
use super::typebox::uri::is_uri;
use super::typebox::uri_reference::is_uri_reference;
use super::typebox::uri_template::is_uri_template;
use super::typebox::url::is_url;
use super::typebox::uuid::is_uuid;
use crate::types::FormatCheck;

/// JSON Schema 2020-12 定义的每个格式，按名字；另加 TypeBox 与 ajv-formats 也会检查的 `url` 和
/// `json-pointer-uri-fragment`。检查本身改编自 TypeBox 1.3.27（MIT）。
pub fn formats() -> &'static BTreeMap<String, FormatCheck> {
    &FORMATS
}

static FORMATS: LazyLock<BTreeMap<String, FormatCheck>> = LazyLock::new(|| {
    let mut formats: BTreeMap<String, FormatCheck> = BTreeMap::new();
    let mut add = |name: &str, check: fn(&str) -> bool| {
        formats.insert(name.to_string(), Arc::new(check) as FormatCheck);
    };
    add("date", is_date);
    add("date-time", is_date_time);
    add("duration", is_duration);
    add("email", is_email);
    add("hostname", is_hostname);
    add("idn-email", is_idn_email);
    add("idn-hostname", is_idn_hostname);
    add("ipv4", is_ipv4);
    add("ipv6", is_ipv6);
    add("iri", is_iri);
    add("iri-reference", is_iri_reference);
    add("json-pointer", is_json_pointer);
    add("json-pointer-uri-fragment", is_json_pointer_uri_fragment);
    add("regex", is_regex);
    add("relative-json-pointer", is_relative_json_pointer);
    add("time", is_time);
    add("uri", is_uri);
    add("uri-reference", is_uri_reference);
    add("uri-template", is_uri_template);
    add("url", is_url);
    add("uuid", is_uuid);
    formats
});
