//! 上游没有这个测试文件：`src/js.rs` 是为了对上 JS 的对象键序与数字写法而新增的模块，见
//! `PORTING.md` 的差异表。这里的期望值都是 node 22 上 `Object.keys`、`String(x)`、
//! `JSON.stringify` 的实际输出。
//!
//! 这些规则会被错误消息原样回给模型看（`expected one of [...]`、`expected {...}`），所以单独钉住。

use serde_json::{Map, Value};

use xjpcode_schema::js;

fn object(text: &str) -> Map<String, Value> {
    serde_json::from_str(text).expect("an object")
}

fn keys_of(text: &str) -> Vec<String> {
    js::keys(&object(text)).into_iter().cloned().collect()
}

fn value_of(text: &str) -> Value {
    serde_json::from_str(text).expect("a value")
}

#[test]
fn sorts_array_index_keys_ahead_in_numeric_order() {
    assert_eq!(keys_of(r#"{"b":1,"2":1,"1":1}"#), ["1", "2", "b"]);
    assert_eq!(keys_of(r#"{"10":1,"2":1,"b":1,"1":1}"#), ["1", "2", "10", "b"]);
    assert!(keys_of("{}").is_empty());
    // 条目的值与键一一对上。
    let entries: Vec<String> = js::entries(&object(r#"{"b":1,"2":2,"1":3}"#))
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    assert_eq!(entries, ["1=3", "2=2", "b=1"]);
}

#[test]
fn only_canonical_indices_below_2_pow_32_minus_1_sort_ahead() {
    // `Object.keys(JSON.parse('{"4294967295":1,"0":2,"00":3,"4294967294":4}'))`
    // 2^32-1 不是数组下标，`"00"` 也不是：两者都留在插入顺序里。
    assert_eq!(
        keys_of(r#"{"4294967295":1,"0":2,"00":3,"4294967294":4}"#),
        ["0", "4294967294", "4294967295", "00"]
    );
    // `"-0"` 的规范写法是 `"0"`，因此它不是下标。
    assert_eq!(keys_of(r#"{"-0":1,"0":2}"#), ["0", "-0"]);
    // 指数写法与小数写法都不是规范十进制整数。
    assert_eq!(keys_of(r#"{"1e2":1,"100":2}"#), ["100", "1e2"]);
    assert_eq!(keys_of(r#"{"1.0":1,"1":2}"#), ["1", "1.0"]);
    // 前导零同样不行。
    assert_eq!(keys_of(r#"{"01":1,"1":2}"#), ["1", "01"]);
}

#[test]
fn writes_numbers_as_js_does() {
    assert_eq!(js::number(1.0), "1");
    assert_eq!(js::number(1.5), "1.5");
    assert_eq!(js::number(-0.0), "0");
    assert_eq!(js::number(-1.5), "-1.5");
    assert_eq!(js::number(1e-6), "0.000001");
    assert_eq!(js::number(1e-7), "1e-7");
    assert_eq!(js::number(1e20), "100000000000000000000");
    assert_eq!(js::number(1e21), "1e+21");
    assert_eq!(js::number(1e-21), "1e-21");
    assert_eq!(js::number(0.30000000000000004), "0.30000000000000004");
    // 最短往返位数：最后一两位必须保住。
    assert_eq!(js::number(0.013009999999999999), "0.013009999999999999");
    assert_eq!(js::number(1.2345678901234568e20), "123456789012345680000");
}

#[test]
fn stringifies_like_js() {
    let value =
        value_of(r#"{"b":1,"2":[1,{"z":null,"1":true}],"text":"a\"b\n"}"#);
    assert_eq!(
        js::stringify(&value),
        r#"{"2":[1,{"1":true,"z":null}],"b":1,"text":"a\"b\n"}"#
    );
    assert_eq!(js::stringify(&value_of("null")), "null");
    assert_eq!(js::stringify(&value_of("[1,0.013009999999999999]")), "[1,0.013009999999999999]");
    assert_eq!(js::stringify(&value_of(r#""a/b""#)), r#""a/b""#);
}

#[test]
fn reorders_a_value_into_js_key_order() {
    let value = value_of(r#"{"b":{"2":1,"1":2},"a":[{"z":1,"1":2}]}"#);
    assert_eq!(
        serde_json::to_string(&js::reorder(&value)).expect("a value"),
        r#"{"b":{"1":2,"2":1},"a":[{"1":2,"z":1}]}"#
    );
}

#[test]
fn pretty_prints_like_json_stringify_with_two_spaces() {
    // node 22：`JSON.stringify(value, null, 2)`。工具参数不对时，回给模型看的文字里有这一份。
    assert_eq!(js::stringify_pretty(&value_of("{}")), "{}");
    assert_eq!(js::stringify_pretty(&value_of("[]")), "[]");
    assert_eq!(
        js::stringify_pretty(&value_of(r#"{"value":"many"}"#)),
        "{\n  \"value\": \"many\"\n}"
    );
    assert_eq!(
        js::stringify_pretty(&value_of(r#"{"b":1,"2":[true,null,{"x":"y"}]}"#)),
        "{\n  \"2\": [\n    true,\n    null,\n    {\n      \"x\": \"y\"\n    }\n  ],\n  \"b\": 1\n}"
    );
    // 数字按 JS 的写法，不按 `serde_json` 的写法。
    assert_eq!(js::stringify_pretty(&value_of("10.0")), "10");
    assert_eq!(js::stringify_pretty(&value_of("[1e21,1e-7]")), "[\n  1e+21,\n  1e-7\n]");
}
