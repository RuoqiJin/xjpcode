//! 上游没有对应文件：JS 的对象键顺序与 `JSON.stringify` 是语言自带的，Rust 侧要显式写出来。
//!
//! 错误消息里有两样东西由 JS 决定，而不是由 schema 决定：
//!
//! - **走对象键的顺序。** `JSON.parse('{"b":1,"2":1,"1":1}')` 的 `Object.keys` 是
//!   `["1","2","b"]`：像数组下标的键排在最前、按数值升序，其余键按插入顺序。解释器按这个顺序
//!   报错，`JSON.stringify` 也按这个顺序写键。`serde_json` 的 `Map` 开 `preserve_order` 之后
//!   只保住插入顺序，所以要比对 `Object.keys` 的地方都得先过这里。
//! - **数字的写法。** `String(1)` 是 `"1"`、`String(1e-7)` 是 `"1e-7"`：`-7 < 指数 < 21` 用
//!   十进制，此外用指数写法，而且给的是最短往返位数。
//!
//! 见 `PORTING.md` 差异表。

use serde_json::{Map, Value};

/// `map` 的键，按 JS 的键序。
pub fn keys(map: &Map<String, Value>) -> Vec<&String> {
    let mut indexed: Vec<(u32, &String)> = Vec::new();
    let mut rest: Vec<&String> = Vec::new();
    for key in map.keys() {
        match array_index(key) {
            Some(index) => indexed.push((index, key)),
            None => rest.push(key),
        }
    }
    indexed.sort_by_key(|(index, _)| *index);
    indexed
        .into_iter()
        .map(|(_, key)| key)
        .chain(rest)
        .collect()
}

/// `map` 的条目，按 JS 的键序。
pub fn entries(map: &Map<String, Value>) -> Vec<(&String, &Value)> {
    keys(map)
        .into_iter()
        .map(|key| (key, map.get(key).expect("a key that is in the map")))
        .collect()
}

/// 一个键是不是 JS 的数组下标：规范十进制整数（无前导零、无正负号、无小数点）、取值
/// `0..=2^32-2`。`"4294967295"`（2^32-1）、`"01"`、`"1.0"`、`"1e2"`、`"-0"` 都不是。
pub(crate) fn array_index(key: &str) -> Option<u32> {
    let bytes = key.as_bytes();
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if bytes.len() > 1 && bytes[0] == b'0' {
        return None;
    }
    let value: u32 = key.parse().ok()?;
    (value < u32::MAX).then_some(value)
}

/// 一个 `number` 按 JS `String(x)` 的写法。
///
/// `serde_json` 的 `Value::to_string` 会把 `1` 写成 `1.0`、把 `1e-7` 写成 `0.0000001`；错误消息
/// 要逐字与上游相同，所以按 JS 的规则重写。Rust 的 `{:e}` 与 JS 一样给最短往返位数，所以只差写法
/// 的选择。
pub fn number(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    if x.is_infinite() {
        return if x > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        };
    }
    if x == 0.0 {
        return "0".to_string();
    }
    let negative = x < 0.0;
    let scientific = format!("{:e}", x.abs());
    let (mantissa, exponent) = scientific.split_once('e').expect("exponent");
    let exponent: i32 = exponent.parse().expect("exponent number");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let length = digits.len() as i32;
    let text = if exponent >= -6 && exponent < 21 {
        if exponent >= 0 {
            let integers = exponent + 1;
            if length <= integers {
                format!("{}{}", digits, "0".repeat((integers - length) as usize))
            } else {
                format!(
                    "{}.{}",
                    &digits[..integers as usize],
                    &digits[integers as usize..]
                )
            }
        } else {
            format!("0.{}{}", "0".repeat((-exponent - 1) as usize), digits)
        }
    } else if length == 1 {
        format!(
            "{}e{}{}",
            digits,
            if exponent > 0 { '+' } else { '-' },
            exponent.abs()
        )
    } else {
        format!(
            "{}.{}e{}{}",
            &digits[..1],
            &digits[1..],
            if exponent > 0 { '+' } else { '-' },
            exponent.abs()
        )
    };
    if negative {
        format!("-{text}")
    } else {
        text
    }
}

/// 一个值按 JS `String(v)` 的样子变成字符串。
///
/// 上游有几处依赖字符串化：`type` 的取值列表、`dependencies` 里不是字符串的键名（`Object.hasOwn`
/// 会把属性键这么转）。数字按 [`number`]，数组按 `Array.prototype.join`，对象给
/// `"[object Object]"`。
pub fn to_string_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(value) => value
            .as_f64()
            .map(number)
            .unwrap_or_else(|| value.to_string()),
        Value::String(text) => text.clone(),
        Value::Array(list) => list
            .iter()
            .map(|item| match item {
                // `Array.prototype.join` 把 `null` 当成空串。
                Value::Null => String::new(),
                other => to_string_value(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

/// 一份值按 JS 的键序重建：对象的键按 [`keys`] 排，其余原样。
///
/// 用在“`JSON.stringify` 之后再 `JSON.parse`”这一步（上游 `toToolDeclaration`）：
/// 往返之后对象的可观察键序就是 JS 的键序。
pub fn reorder(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            entries(map)
                .into_iter()
                .map(|(key, value)| (key.clone(), reorder(value)))
                .collect(),
        ),
        Value::Array(list) => Value::Array(list.iter().map(reorder).collect()),
        other => other.clone(),
    }
}

/// 一个值按 JS `JSON.stringify(value, null, 2)` 的写法：与 [`stringify`] 同样的键序与数字、字符串
/// 写法，只是对象与数组分行缩进。
///
/// 工具参数不对时，那段回给模型看的文字里有这一份（上游 `JSON.stringify(toolCall.arguments, null, 2)`）。
pub fn stringify_pretty(value: &Value) -> String {
    fn write(value: &Value, depth: usize, out: &mut String) {
        match value {
            Value::Array(list) if !list.is_empty() => {
                out.push_str("[\n");
                for (index, item) in list.iter().enumerate() {
                    if index > 0 {
                        out.push_str(",\n");
                    }
                    push_indent(depth + 1, out);
                    write(item, depth + 1, out);
                }
                out.push('\n');
                push_indent(depth, out);
                out.push(']');
            }
            Value::Object(map) if !map.is_empty() => {
                out.push_str("{\n");
                for (index, (key, item)) in entries(map).into_iter().enumerate() {
                    if index > 0 {
                        out.push_str(",\n");
                    }
                    push_indent(depth + 1, out);
                    out.push_str(&serde_json::to_string(key).expect("a key is a string"));
                    out.push_str(": ");
                    write(item, depth + 1, out);
                }
                out.push('\n');
                push_indent(depth, out);
                out.push('}');
            }
            other => out.push_str(&stringify(other)),
        }
    }

    fn push_indent(depth: usize, out: &mut String) {
        for _ in 0..depth {
            out.push_str("  ");
        }
    }

    let mut out = String::new();
    write(value, 0, &mut out);
    out
}

/// 一个值按 JS `JSON.stringify` 的写法：键序同 [`keys`]，数字同 [`number`]。
pub fn stringify(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number_value) => number_value
            .as_f64()
            .map(number)
            .unwrap_or_else(|| number_value.to_string()),
        Value::String(text) => serde_json::to_string(text).expect("a string is JSON"),
        Value::Array(list) => {
            let items: Vec<String> = list.iter().map(stringify).collect();
            format!("[{}]", items.join(","))
        }
        Value::Object(map) => {
            let items: Vec<String> = entries(map)
                .into_iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).expect("a key is a string"),
                        stringify(value)
                    )
                })
                .collect();
            format!("{{{}}}", items.join(","))
        }
    }
}
