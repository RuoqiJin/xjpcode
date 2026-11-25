//! 移植自上游 `packages/ai/test/streaming-json.test.ts`。
//!
//! 逐条对应上游的用例；只有这几处按 `PORTING.md` 差异表改过，每条在用例里注明：
//!
//! - `matches today's parseStreamingJson at every prefix of compact documents` 没有搬：它的对照是
//!   `parseStreamingJson()`，那个函数与 `partial-json` 本次都不搬（差异表 #103）。它要守的规矩由第一条
//!   与上一条（与独立写的一份参考实现、与 `JSON.parse` 逐条比）覆盖。
//! - `memory-helpers.ts` 的 `allocatedBytes` 换成测试里自己数的分配量（`counting allocator` +
//!   线程本地计数），`collectGarbage` 那条没有对应物：Rust 没有 GC，流与快照随所有权释放（差异表 #105）。
//! - 孤立代理项（`\ud83d` 后面没有低半）在 Rust 的 `String` 里没有对应值（差异表 #104）：低半可能还在后面
//!   时先压着，字符串结束时显示成 U+FFFD；被拆到两段文字里的原始代理对不可能出现。
//! - `__proto__` 的原型污染在 Rust 里没有对应物（`serde_json` 的对象没有原型链，差异表 #104）；
//!   用例留下“它就是一个普通成员”。
//! - 数字的表示：整数值的浮点写法（`2.5e3`、`1.0`）读进来是 `2500.0` / `1.0`，数值与 pi 相同，文本与
//!   `JSON.stringify` 不同（差异表 #22、#107）。

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use serde_json::{json, Map, Value};
use xjpcode_ai::utils::json_parse::{parse_json_with_repair, parse_tool_call_arguments};
use xjpcode_ai::utils::streaming_json::{
    JsonChange, JsonPathSegment, StreamingJsonError, StreamingJsonParser, ToolCallArgumentsStream,
};

// ---------------------------------------------------------------------------
// 数与文档的生成：照上游的随机数（同一条公式，double 乘法超了 2^53 也照它舍入），
// 所以每条种子生成的文档与上游相同。
// ---------------------------------------------------------------------------

struct Random {
    state: f64,
}

impl Random {
    fn new(seed: u32) -> Self {
        Self {
            state: f64::from(seed),
        }
    }

    fn next(&mut self) -> f64 {
        // `state = (state * 1103515245 + 12345) & 0x7fffffff`：乘法在 double 里做（超过 2^53 会舍入），
        // `&` 先 ToInt32 再取低 31 位。
        let product = self.state * 1_103_515_245.0 + 12_345.0;
        let low = (product as i64) as u32 & 0x7fff_ffff;
        self.state = f64::from(low);
        self.state / 2_147_483_647.0
    }

    fn range(&mut self, length: usize) -> usize {
        (self.next() * length as f64).floor() as usize
    }
}

fn pick<'a, T>(rand: &mut Random, items: &'a [T]) -> &'a T {
    &items[rand.range(items.len())]
}

const STRING_PIECES: &[&str] = &[
    "a", "b", "x y", " ", "\n", "\t", "\\", "\"", "/", "é", "😀", "\u{1}", "\u{1f}", "\u{2028}",
    "{", "]", ",",
];

fn random_string(rand: &mut Random) -> String {
    let length = rand.range(6);
    let mut text = String::new();
    for _ in 0..length {
        text.push_str(pick(rand, STRING_PIECES));
    }
    text
}

fn random_scalar(rand: &mut Random) -> Value {
    match rand.range(10) {
        0 => Value::String(random_string(rand)),
        1 => Value::from((rand.next() * 2000.0).floor() as i64 - 1000),
        2 => Value::from(rand.next() * 100.0),
        3 => Value::from(1e21),
        4 => Value::from(-5e-8),
        5 => Value::from(-0.0),
        6 => Value::from(0),
        7 => Value::Bool(true),
        8 => Value::Bool(false),
        _ => Value::Null,
    }
}

fn random_value(rand: &mut Random, depth: usize) -> Value {
    let roll = rand.next();
    if depth > 3 || roll < 0.3 {
        return random_scalar(rand);
    }
    if roll < 0.65 {
        let mut members = Map::new();
        let count = rand.range(4);
        for _ in 0..count {
            members.insert(random_string(rand), random_value(rand, depth + 1));
        }
        return Value::Object(members);
    }
    let count = rand.range(4);
    Value::Array((0..count).map(|_| random_value(rand, depth + 1)).collect())
}

/// `value` 的 JSON 文本；`spaced` 时按上游的取法插入随机的无关空白。
///
/// 求值顺序与上游逐字对应：容器的空白在元素渲染之后才取，这样同一颗种子生成同一份文字。
fn render(value: &Value, rand: &mut Random, spaced: bool) -> String {
    let space = |rand: &mut Random| -> String {
        if spaced {
            pick(rand, &["", "", " ", "\n  ", "\t", "\r\n"]).to_string()
        } else {
            String::new()
        }
    };
    match value {
        Value::Array(items) => {
            let mut rendered = String::from("[");
            rendered.push_str(&space(rand));
            let items: Vec<String> = items
                .iter()
                .map(|item| render(item, rand, spaced))
                .collect();
            let separator = format!("{},{}", space(rand), space(rand));
            rendered.push_str(&items.join(&separator));
            rendered.push_str(&space(rand));
            rendered.push(']');
            rendered
        }
        Value::Object(members) => {
            let mut rendered = String::from("{");
            rendered.push_str(&space(rand));
            let members: Vec<String> = members
                .iter()
                .map(|(key, item)| {
                    format!(
                        "{}:{}{}",
                        serde_json::to_string(key).expect("a JSON string"),
                        space(rand),
                        render(item, rand, spaced)
                    )
                })
                .collect();
            let separator = format!("{},{}", space(rand), space(rand));
            rendered.push_str(&members.join(&separator));
            rendered.push_str(&space(rand));
            rendered.push('}');
            rendered
        }
        // 上游的 `JSON.stringify`；`-0` 的文本在 Rust 里是 `-0.0`，读回来是同一个数。
        scalar => serde_json::to_string(scalar).expect("a JSON value"),
    }
}

fn random_documents(seed: u32, count: usize, spaced: bool) -> Vec<String> {
    let mut rand = Random::new(seed);
    (0..count)
        .map(|_| {
            let value = if rand.next() < 0.8 {
                let mut members = Map::new();
                members.insert("path".to_string(), Value::String(random_string(&mut rand)));
                members.insert("args".to_string(), random_value(&mut rand, 0));
                Value::Object(members)
            } else {
                random_value(&mut rand, 0)
            };
            render(&value, &mut rand, spaced)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 参考实现：把整段前缀按文档里的规矩递归读一遍，与增量解析器互相检查。
// ---------------------------------------------------------------------------

struct Read {
    value: Value,
    complete: bool,
    end: usize,
}

fn reference_snapshot(text: &str, pending_scalars: bool) -> Option<Value> {
    let start = skip_space(text, 0);
    read_value(text, start, pending_scalars).map(|read| read.value)
}

fn skip_space(text: &str, start: usize) -> usize {
    let mut index = start;
    while index < text.len() && matches!(text.as_bytes()[index], b' ' | b'\t' | b'\n' | b'\r') {
        index += 1;
    }
    index
}

fn read_value(text: &str, start: usize, pending_scalars: bool) -> Option<Read> {
    if start >= text.len() {
        return None;
    }
    match text.as_bytes()[start] {
        b'"' => Some(read_string(text, start)),
        b'{' => Some(read_object(text, start, pending_scalars)),
        b'[' => Some(read_array(text, start, pending_scalars)),
        _ => read_scalar(text, start, pending_scalars),
    }
}

fn simple_escape(char: char) -> Option<&'static str> {
    Some(match char {
        '"' => "\"",
        '\\' => "\\",
        '/' => "/",
        'b' => "\u{8}",
        'f' => "\u{c}",
        'n' => "\n",
        'r' => "\r",
        't' => "\t",
        _ => return None,
    })
}

fn read_string(text: &str, start: usize) -> Read {
    let mut value = String::new();
    let mut pending_high: Option<u16> = None;
    let mut index = start + 1;
    while index < text.len() {
        let char = text[index..].chars().next().expect("a character boundary");
        if char == '"' {
            if pending_high.take().is_some() {
                value.push(char::REPLACEMENT_CHARACTER);
            }
            return Read {
                value: Value::String(value),
                complete: true,
                end: index + 1,
            };
        }
        if char != '\\' {
            if pending_high.take().is_some() {
                value.push(char::REPLACEMENT_CHARACTER);
            }
            value.push(char);
            index += char.len_utf8();
            continue;
        }
        let next = text[index + 1..].chars().next();
        let Some(next) = next else {
            return Read {
                value: Value::String(value),
                complete: false,
                end: text.len(),
            };
        };
        if let Some(decoded) = simple_escape(next) {
            if pending_high.take().is_some() {
                value.push(char::REPLACEMENT_CHARACTER);
            }
            value.push_str(decoded);
            index += 1 + next.len_utf8();
            continue;
        }
        if next == 'u' {
            let mut end = index + 2;
            let mut digits = String::new();
            while end < text.len() && digits.len() < 4 {
                let digit = text[end..].chars().next().expect("a character boundary");
                if !digit.is_ascii_hexdigit() {
                    break;
                }
                digits.push(digit);
                end += 1;
            }
            if digits.len() == 4 {
                let code = u16::from_str_radix(&digits, 16).expect("four hex digits");
                if let Some(high) = pending_high.take() {
                    if (0xDC00..=0xDFFF).contains(&code) {
                        let combined = 0x10000
                            + ((u32::from(high) - 0xD800) << 10)
                            + (u32::from(code) - 0xDC00);
                        value.push(char::from_u32(combined).expect("a surrogate pair"));
                        index = end;
                        continue;
                    }
                    value.push(char::REPLACEMENT_CHARACTER);
                }
                if (0xD800..=0xDBFF).contains(&code) {
                    pending_high = Some(code);
                } else if (0xDC00..=0xDFFF).contains(&code) {
                    value.push(char::REPLACEMENT_CHARACTER);
                } else {
                    value.push(char::from_u32(u32::from(code)).expect("a BMP code unit"));
                }
                index = end;
                continue;
            }
            if end == text.len() {
                return Read {
                    value: Value::String(value),
                    complete: false,
                    end,
                };
            }
            // 不是四个十六进制数字：转义留成字面文字，下一个字符按普通字符读。
            if pending_high.take().is_some() {
                value.push(char::REPLACEMENT_CHARACTER);
            }
            value.push_str(&text[index..end]);
            index = end;
            continue;
        }
        // 非法转义：反斜杠留成字面文字，下一个字符按普通字符读。
        if pending_high.take().is_some() {
            value.push(char::REPLACEMENT_CHARACTER);
        }
        value.push('\\');
        index += 1;
    }
    Read {
        value: Value::String(value),
        complete: false,
        end: text.len(),
    }
}

fn set_own(members: &mut Map<String, Value>, key: String, value: Value) {
    // 与 `Object.defineProperty` 一样：同名的键保住第一次的位置，值换成新的。
    members.insert(key, value);
}

fn read_object(text: &str, start: usize, pending_scalars: bool) -> Read {
    let mut members = Map::new();
    fn partial(members: &Map<String, Value>, end: usize) -> Read {
        Read {
            value: Value::Object(members.clone()),
            complete: false,
            end,
        }
    }
    let mut index = skip_space(text, start + 1);
    if index >= text.len() {
        return partial(&members, text.len());
    }
    if text.as_bytes()[index] == b'}' {
        return Read {
            value: Value::Object(members),
            complete: true,
            end: index + 1,
        };
    }
    loop {
        assert_eq!(
            text.as_bytes()[index],
            b'"',
            "reference reads valid JSON only: {text:?}"
        );
        let key = read_string(text, index);
        if !key.complete {
            return partial(&members, text.len());
        }
        index = skip_space(text, key.end);
        if index >= text.len() {
            return partial(&members, text.len());
        }
        index = skip_space(text, index + 1);
        let Some(member) = read_value(text, index, pending_scalars) else {
            return partial(&members, text.len());
        };
        set_own(
            &mut members,
            key.value.as_str().expect("a key").to_string(),
            member.value.clone(),
        );
        if !member.complete {
            return partial(&members, text.len());
        }
        index = skip_space(text, member.end);
        if index >= text.len() {
            return partial(&members, text.len());
        }
        if text.as_bytes()[index] == b'}' {
            return Read {
                value: Value::Object(members),
                complete: true,
                end: index + 1,
            };
        }
        index = skip_space(text, index + 1);
        if index >= text.len() {
            return partial(&members, text.len());
        }
    }
}

fn read_array(text: &str, start: usize, pending_scalars: bool) -> Read {
    let mut elements: Vec<Value> = Vec::new();
    fn partial(elements: &[Value], end: usize) -> Read {
        Read {
            value: Value::Array(elements.to_vec()),
            complete: false,
            end,
        }
    }
    let mut index = skip_space(text, start + 1);
    if index >= text.len() {
        return partial(&elements, text.len());
    }
    if text.as_bytes()[index] == b']' {
        return Read {
            value: Value::Array(elements),
            complete: true,
            end: index + 1,
        };
    }
    loop {
        let Some(element) = read_value(text, index, pending_scalars) else {
            return partial(&elements, text.len());
        };
        elements.push(element.value.clone());
        if !element.complete {
            return partial(&elements, text.len());
        }
        index = skip_space(text, element.end);
        if index >= text.len() {
            return partial(&elements, text.len());
        }
        if text.as_bytes()[index] == b']' {
            return Read {
                value: Value::Array(elements),
                complete: true,
                end: index + 1,
            };
        }
        index = skip_space(text, index + 1);
        if index >= text.len() {
            return partial(&elements, text.len());
        }
    }
}

fn read_scalar(text: &str, start: usize, pending_scalars: bool) -> Option<Read> {
    let bytes = text.as_bytes();
    let mut end = start;
    while end < bytes.len()
        && !matches!(
            bytes[end],
            b' ' | b'\t' | b'\n' | b'\r' | b',' | b':' | b'{' | b'}' | b'[' | b']' | b'"'
        )
    {
        end += 1;
    }
    let token = &text[start..end];
    if end < bytes.len() {
        return Some(Read {
            value: serde_json::from_str(token).expect("the reference reads valid JSON only"),
            complete: true,
            end,
        });
    }
    if !pending_scalars {
        return None;
    }
    // 文字末尾：字面量的前缀、一个数，或者指数还没数字的数。
    for (literal, value) in [
        ("true", Value::Bool(true)),
        ("false", Value::Bool(false)),
        ("null", Value::Null),
    ] {
        if literal.starts_with(token) {
            return Some(Read {
                value,
                complete: false,
                end,
            });
        }
    }
    if is_number(token) {
        return Some(Read {
            value: serde_json::from_str(token).ok()?,
            complete: false,
            end,
        });
    }
    let mantissa = mantissa_without_exponent_digits(token)?;
    Some(Read {
        value: serde_json::from_str(mantissa).ok()?,
        complete: false,
        end,
    })
}

/// `^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?$`
fn is_number(token: &str) -> bool {
    let bytes = token.as_bytes();
    let byte = |index: usize| bytes.get(index).copied();
    let mut index = 0;
    if byte(index) == Some(b'-') {
        index += 1;
    }
    match byte(index) {
        Some(b'0') => index += 1,
        Some(b'1'..=b'9') => {
            index += 1;
            while matches!(byte(index), Some(b'0'..=b'9')) {
                index += 1;
            }
        }
        _ => return false,
    }
    if byte(index) == Some(b'.') {
        index += 1;
        if !matches!(byte(index), Some(b'0'..=b'9')) {
            return false;
        }
        while matches!(byte(index), Some(b'0'..=b'9')) {
            index += 1;
        }
    }
    if matches!(byte(index), Some(b'e') | Some(b'E')) {
        index += 1;
        if matches!(byte(index), Some(b'+') | Some(b'-')) {
            index += 1;
        }
        if !matches!(byte(index), Some(b'0'..=b'9')) {
            return false;
        }
        while matches!(byte(index), Some(b'0'..=b'9')) {
            index += 1;
        }
    }
    index == bytes.len()
}

fn mantissa_without_exponent_digits(token: &str) -> Option<&str> {
    let mantissa = ["e+", "e-", "E+", "E-"]
        .iter()
        .find_map(|suffix| token.strip_suffix(suffix))
        .or_else(|| token.strip_suffix('e'))
        .or_else(|| token.strip_suffix('E'))?;
    is_number(mantissa).then_some(mantissa)
}

// ---------------------------------------------------------------------------
// 分段喂给解析器
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum ChunkSize {
    Fixed(usize),
    Random,
}

const CHUNK_SIZES: &[ChunkSize] = &[
    ChunkSize::Fixed(1),
    ChunkSize::Fixed(2),
    ChunkSize::Fixed(3),
    ChunkSize::Fixed(7),
    ChunkSize::Random,
];

struct Step {
    read: String,
    snapshot: Option<Value>,
    copy: Option<Value>,
    error: Option<StreamingJsonError>,
}

/// 把 `text` 分段喂进去；返回每一段之后的快照、当场复制的一份，以及到那时读到的文字。
fn stream(text: &str, chunk: ChunkSize) -> Vec<Step> {
    let mut parser = StreamingJsonParser::new();
    let mut rand = matches!(chunk, ChunkSize::Random).then(|| Random::new(read32(text)));
    let mut steps = Vec::new();
    let mut index = 0;
    while index < text.len() {
        let size = match &mut rand {
            Some(rand) => 1 + rand.range(12),
            None => match chunk {
                ChunkSize::Fixed(size) => size,
                ChunkSize::Random => unreachable!("the random seed is built above"),
            },
        };
        let end = next_boundary(text, (index + size).min(text.len()));
        let error = parser.append(&text[index..end]).err();
        index = end;
        let snapshot = parser.snapshot();
        steps.push(Step {
            read: text[..index].to_string(),
            copy: snapshot.as_deref().cloned(),
            snapshot: snapshot.as_deref().cloned(),
            error,
        });
    }
    steps
}

/// 同上，但每段都不许报错。
fn stream_ok(text: &str, chunk: ChunkSize) -> Vec<Step> {
    let steps = stream(text, chunk);
    for step in &steps {
        assert_eq!(step.error, None, "{:?}", step.read);
    }
    steps
}

/// 大于等于 `index` 的第一个字符边界。
fn next_boundary(text: &str, index: usize) -> usize {
    let mut end = index;
    while end < text.len() && !text.is_char_boundary(end) {
        end += 1;
    }
    end
}

fn final_snapshot(text: &str, chunk: ChunkSize) -> Option<Value> {
    stream_ok(text, chunk)
        .last()
        .and_then(|step| step.snapshot.clone())
}

/// 一份种子，让每份文字的随机分段各不相同。
fn read32(text: &str) -> u32 {
    let mut hash: i32 = 7;
    for char in text.chars() {
        hash = hash.wrapping_mul(31).wrapping_add(char as u32 as i32);
    }
    (i64::from(hash).unsigned_abs() as u32) + 1
}

/// 断言 `value` 是普通的、有限的 JSON 数据，也就是使用方（例如会话文件的严格 JSON 复制）收得下的形状。
fn assert_plain_json(value: Option<&Value>, label: &str) {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(_)) | Some(Value::String(_)) => {}
        Some(Value::Number(number)) => {
            let float = number.as_f64().expect("a number");
            assert!(float.is_finite(), "{label}: a finite number");
        }
        Some(Value::Array(items)) => {
            for item in items {
                assert_plain_json(Some(item), label);
            }
        }
        Some(Value::Object(members)) => {
            for item in members.values() {
                assert_plain_json(Some(item), label);
            }
        }
    }
}

fn expected(text: &str) -> Value {
    serde_json::from_str(text).expect("a JSON expectation")
}

// ---------------------------------------------------------------------------
// differential fuzzing
// ---------------------------------------------------------------------------

#[test]
fn matches_the_reference_after_every_piece_at_every_chunk_size_with_whitespace() {
    for text in random_documents(1, 250, true) {
        for chunk in CHUNK_SIZES {
            for step in stream_ok(&text, *chunk) {
                assert_eq!(
                    step.snapshot,
                    reference_snapshot(&step.read, true),
                    "{:?} (chunk size {chunk:?})",
                    step.read
                );
            }
        }
    }
}

#[test]
fn reads_complete_documents_exactly_like_json_parse() {
    let documents: Vec<String> = random_documents(3, 300, true)
        .into_iter()
        .chain(random_documents(4, 300, false))
        .collect();
    for text in documents {
        let expected: Value = serde_json::from_str(&text).expect("valid JSON");
        for chunk in CHUNK_SIZES {
            assert_eq!(
                final_snapshot(&text, *chunk),
                Some(expected.clone()),
                "{text}"
            );
        }
    }
}

#[test]
fn never_changes_a_snapshot_once_it_is_returned() {
    for text in random_documents(5, 150, true) {
        for step in stream_ok(&text, ChunkSize::Random) {
            assert_eq!(step.snapshot, step.copy, "{:?}", step.read);
        }
    }
}

#[test]
fn repairs_raw_control_characters_and_invalid_escapes_like_the_whole_document_reader() {
    let mut rand = Random::new(6);
    let malformed = [
        "\n", "\t", "\u{0}", "\\d", "\\x41", "\\u12", "\\uZZZZ", "\\u00e9", "\\\\", "\\q",
    ];
    for round in 0..300 {
        let mut content = String::new();
        for _ in 0..6 {
            content.push_str(pick(&mut rand, &malformed));
        }
        content.push_str(pick(&mut rand, &["a", " ", "é"]));
        let text = format!("{{\"path\":\"p\",\"content\":\"{content}\",\"n\":{round}}}");
        // 完整的文字：整份读出来与“修一遍再读”相同（上游比的是 `parseStreamingJson`，差异表 #103）。
        assert_eq!(
            final_snapshot(&text, ChunkSize::Fixed(1)),
            Some(parse_json_with_repair(&text).expect("a readable document")),
            "{text:?}"
        );
        for step in stream_ok(&text, ChunkSize::Fixed(1)) {
            assert_eq!(
                step.snapshot,
                reference_snapshot(&step.read, true),
                "{:?}",
                step.read
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 字符串
// ---------------------------------------------------------------------------

#[test]
fn holds_an_escape_back_until_it_is_complete_at_every_split_position() {
    for (text, value) in [
        (r#"{"a":"x\u00e9y"}"#, "xéy"),
        (r#"{"a":"x\ny"}"#, "x\ny"),
        (r#"{"a":"x\\y"}"#, "x\\y"),
        (r#"{"a":"x\"y"}"#, "x\"y"),
        (r#"{"a":"x\/y"}"#, "x/y"),
    ] {
        let mut splits: Vec<usize> = (0..=text.len())
            .filter(|index| text.is_char_boundary(*index))
            .collect();
        splits.sort_unstable();
        for split in splits {
            let mut parser = StreamingJsonParser::new();
            parser.append(&text[..split]).expect("readable");
            assert_eq!(
                parser.snapshot().as_deref().cloned(),
                reference_snapshot(&text[..split], true),
                "{:?}",
                &text[..split]
            );
            parser.append(&text[split..]).expect("readable");
            assert_eq!(parser.snapshot().as_deref(), Some(&json!({ "a": value })));
        }
    }
}

#[test]
fn joins_surrogate_pairs_split_across_pieces_escaped_or_raw() {
    let escaped = r#"{"a":"\ud83d\ude00!"}"#;
    let raw = "{\"a\":\"😀!\"}";
    for text in [escaped, raw] {
        for split in (0..=text.len()).filter(|index| text.is_char_boundary(*index)) {
            let mut parser = StreamingJsonParser::new();
            parser.append(&text[..split]).expect("readable");
            parser.snapshot();
            parser.append(&text[split..]).expect("readable");
            assert_eq!(parser.snapshot().as_deref(), Some(&json!({ "a": "😀!" })));
        }
    }
    // 高半没有低半：pi 的字符串里留着一个孤立代理项，Rust 的 `String` 没有这个值（差异表 #104）：
    // 低半可能还在后面，先压着，所以此刻显示空的。
    let mut parser = StreamingJsonParser::new();
    parser.append(r#"{"a":"\ud83d"#).expect("readable");
    assert_eq!(parser.snapshot().as_deref(), Some(&json!({ "a": "" })));
    // 字符串到此结束：显示成 U+FFFD。
    parser.append("\"").expect("readable");
    assert_eq!(
        parser.snapshot().as_deref(),
        Some(&json!({ "a": "\u{FFFD}" }))
    );
}

#[test]
fn keeps_trailing_whitespace_of_an_unterminated_string() {
    let mut parser = StreamingJsonParser::new();
    parser.append("{\"code\":\"x\t\t  ").expect("readable");
    assert_eq!(
        parser.snapshot().as_deref(),
        Some(&json!({ "code": "x\t\t  " }))
    );
}

#[test]
fn keeps_raw_control_characters_and_invalid_escapes_visible_while_streaming() {
    let mut parser = StreamingJsonParser::new();
    parser
        .append("{\"a\":\"x\ny \\\\d \\\\u12\"")
        .expect("readable");
    assert_eq!(
        parser.snapshot().as_deref(),
        Some(&json!({ "a": "x\ny \\d \\u12" }))
    );
}

#[test]
fn keeps_invalid_unicode_escapes_literal_without_dropping_fields() {
    assert_eq!(
        final_snapshot(
            r#"{"path":"a.tex","content":"\usepackage{x}","mode":"w"}"#,
            ChunkSize::Fixed(1)
        ),
        Some(json!({ "path": "a.tex", "content": "\\usepackage{x}", "mode": "w" }))
    );
    assert_eq!(
        final_snapshot(r#"{"text":"caf\u00e9 \url"}"#, ChunkSize::Fixed(3)),
        Some(json!({ "text": "café \\url" }))
    );
    let mut parser = StreamingJsonParser::new();
    parser.append(r#"{"text":"ab\u00"#).expect("readable");
    assert_eq!(parser.snapshot().as_deref(), Some(&json!({ "text": "ab" })));
}

#[test]
fn makes_proto_an_own_member_as_json_parse_does() {
    let text = r#"{"__proto__":{"polluted":true},"b":{"__proto__":"x"}}"#;
    for split in (0..=text.len()).filter(|index| text.is_char_boundary(*index)) {
        let mut parser = StreamingJsonParser::new();
        parser.append(&text[..split]).expect("readable");
        parser.snapshot();
        parser.append(&text[split..]).expect("readable");
        let value = parser.snapshot().as_deref().cloned();
        assert_eq!(value, Some(expected(text)));
    }
    // Rust 的对象没有原型链，`__proto__` 只是一个普通成员（差异表 #104）：值里读得到它。
    let value = final_snapshot(text, ChunkSize::Fixed(1)).expect("a value");
    assert_eq!(value["__proto__"]["polluted"], Value::Bool(true));
}

#[test]
fn keeps_the_last_value_of_a_duplicate_key_in_the_first_keys_position() {
    let text = r#"{"a":1,"b":2,"a":"three"}"#;
    let value = final_snapshot(text, ChunkSize::Fixed(1)).expect("a value");
    let expected: Value = serde_json::from_str(text).expect("valid JSON");
    assert_eq!(value, expected);
    let keys: Vec<&String> = value.as_object().expect("an object").keys().collect();
    let expected_keys: Vec<&String> = expected.as_object().expect("an object").keys().collect();
    assert_eq!(keys, expected_keys);
}

// ---------------------------------------------------------------------------
// 上游记下的两组测试向量
// ---------------------------------------------------------------------------

type Vector = (&'static str, Option<Value>);

/// partial-json（MIT）的公开测试用例（每种部分类型都允许，因为 `parseStreamingJson` 就这么用它）。
fn partial_json_vectors() -> Vec<Vector> {
    vec![
        ("\"", Some(json!(""))),
        ("[\"", Some(json!([""]))),
        ("{\"\": \"", Some(json!({ "": "" }))),
        ("n", Some(Value::Null)),
        ("t", Some(Value::Bool(true))),
        ("f", Some(Value::Bool(false))),
        ("0", Some(json!(0))),
        ("-1.25e+4", Some(json!(-1.25e4))),
        ("-1.25e+", Some(json!(-1.25))),
        ("-1.25e", Some(json!(-1.25))),
        // 差异（`streaming-json.ts` 里写明）：非法转义留着可见（partial-json 在反斜杠处截断），
        // `Infinity`/`NaN` 不是 JSON、丢掉。
        ("\" \\x12", Some(json!(" \\x12"))),
        ("I", None),
        ("-I", None),
        ("N", None),
    ]
}

/// Vercel AI SDK 的 fixJson（Apache-2.0）测试用例；期望值是它修好之后的文字的 `JSON.parse`。
fn fix_json_vectors() -> Vec<Vector> {
    vec![
        ("", None),
        ("nul", Some(Value::Null)),
        ("t", Some(Value::Bool(true))),
        ("fals", Some(Value::Bool(false))),
        ("12.2", Some(json!(12.2))),
        ("-12", Some(json!(-12))),
        ("-", None),
        ("2.5e", Some(json!(2.5))),
        ("2.5e-", Some(json!(2.5))),
        ("2.5e3", Some(json!(2.5e3))),
        ("-2.5e3", Some(json!(-2.5e3))),
        ("2.5E", Some(json!(2.5))),
        ("2.5E-", Some(json!(2.5))),
        ("2.5E3", Some(json!(2.5e3))),
        ("-2.5E3", Some(json!(-2.5e3))),
        ("12.34e", Some(json!(12.34))),
        ("5e", Some(json!(5))),
        ("\"abc", Some(json!("abc"))),
        (
            "\"value with \\\"quoted\\\" text and \\\\ escape",
            Some(json!("value with \"quoted\" text and \\ escape")),
        ),
        ("\"value with \\", Some(json!("value with "))),
        ("\"\\u", Some(json!(""))),
        ("\"\\u12", Some(json!(""))),
        ("\"text \\u00", Some(json!("text "))),
        ("{\"a\":\"\\u12", Some(json!({ "a": "" }))),
        ("\"value with unicode <\"", Some(json!("value with unicode <"))),
        ("[", Some(json!([]))),
        ("[[1], [2", Some(json!([[1], [2]]))),
        ("[[\"1\"], [\"2", Some(json!([["1"], ["2"]]))),
        ("[[false], [nu", Some(json!([[false], [null]]))),
        ("[[[]], [[]", Some(json!([[[]], [[]]]))),
        ("[[{}], [{", Some(json!([[{}], [{}]]))),
        ("[1, ", Some(json!([1]))),
        ("[[], 123", Some(json!([[], 123]))),
        ("{\"key\":", Some(json!({}))),
        (
            "{\"a\": {\"b\": 1}, \"c\": {\"d\": 2",
            Some(json!({ "a": { "b": 1 }, "c": { "d": 2 } })),
        ),
        (
            "{\"a\": {\"b\": \"1\"}, \"c\": {\"d\": 2",
            Some(json!({ "a": { "b": "1" }, "c": { "d": 2 } })),
        ),
        (
            "{\"a\": {\"b\": false}, \"c\": {\"d\": 2",
            Some(json!({ "a": { "b": false }, "c": { "d": 2 } })),
        ),
        (
            "{\"a\": {\"b\": []}, \"c\": {\"d\": 2",
            Some(json!({ "a": { "b": [] }, "c": { "d": 2 } })),
        ),
        (
            "{\"a\": {\"b\": {}}, \"c\": {\"d\": 2",
            Some(json!({ "a": { "b": {} }, "c": { "d": 2 } })),
        ),
        ("{\"ke", Some(json!({}))),
        ("{\"k1\": 1, \"k2", Some(json!({ "k1": 1 }))),
        ("{\"k1\": 1, \"k2\":", Some(json!({ "k1": 1 }))),
        ("{\"key\": \"value\"  ", Some(json!({ "key": "value" }))),
        ("{\"a\": {\"b\": {}", Some(json!({ "a": { "b": {} } }))),
        ("[1, [2, 3, [", Some(json!([1, [2, 3, []]]))),
        ("[false, [true, [", Some(json!([false, [true, []]]))),
        ("{\"key\": {\"subKey\":", Some(json!({ "key": {} }))),
        (
            "{\"key\": 123, \"key2\": {\"subKey\":",
            Some(json!({ "key": 123, "key2": {} })),
        ),
        (
            "{\"key\": null, \"key2\": {\"subKey\":",
            Some(json!({ "key": null, "key2": {} })),
        ),
        (
            "{\"key\": [1, 2, {",
            Some(json!({ "key": [1, 2, {}] })),
        ),
        (
            "[1, 2, {\"key\": \"value\",",
            Some(json!([1, 2, { "key": "value" }])),
        ),
        (
            "{\"a\": {\"b\": [\"c\", {\"d\": \"e\",",
            Some(json!({ "a": { "b": ["c", { "d": "e" }] } })),
        ),
        (
            "{\"a\": {\"b\": {\"c\": {\"d\":",
            Some(json!({ "a": { "b": { "c": {} } } })),
        ),
        ("{\"a\": 1, \"b\": [", Some(json!({ "a": 1, "b": [] }))),
        ("{\"a\": 1, \"b\": {", Some(json!({ "a": 1, "b": {} }))),
        ("{\"a\": 1, \"b\": \"", Some(json!({ "a": 1, "b": "" }))),
        (
            "{\n  \"a\": [\n    {\n      \"a1\": \"v1\",\n      \"a2\": \"v2\",\n      \"a3\": \"v3\"\n    }\n  ],\n  \"b\": [\n    {\n      \"b1\": \"n",
            Some(json!({ "a": [{ "a1": "v1", "a2": "v2", "a3": "v3" }], "b": [{ "b1": "n" }] })),
        ),
        (
            "{\"type\":\"div\",\"children\":[{\"type\":\"Card\",\"props\":{}",
            Some(json!({ "type": "div", "children": [{ "type": "Card", "props": {} }] })),
        ),
        // 差异：fixJson 把 `12.` 补成 12；与 `parseStreamingJson` 一样，小数点后面没数字的数不显示。
        ("12.", None),
        ("12.e", None),
    ]
}

#[test]
fn reads_the_recorded_test_vectors() {
    for (input, value) in partial_json_vectors().into_iter().chain(fix_json_vectors()) {
        for chunk in [
            ChunkSize::Fixed(input.len().max(1)),
            ChunkSize::Fixed(1),
            ChunkSize::Fixed(2),
        ] {
            assert_eq!(
                final_snapshot(input, chunk),
                value.clone(),
                "{input:?} (chunk size {chunk:?})"
            );
        }
    }
}

/// 上游的 `data/json-test-suite.json`（JSONTestSuite，原样复制，差异表 #54 的同类做法）。
fn json_test_suite() -> Map<String, Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("test/data/json-test-suite.json");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let suite: Value = serde_json::from_str(&text).expect("a JSON suite");
    suite
        .get("files")
        .and_then(Value::as_object)
        .expect("the suite lists files")
        .clone()
}

#[test]
fn reads_every_json_test_suite_document_json_parse_accepts_and_never_throws_on_the_rest() {
    let suite = json_test_suite();
    assert_eq!(suite.len(), 316, "the fixture is the whole suite");
    for (name, encoded) in suite {
        let encoded = encoded.as_str().expect("base64 text");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("base64");
        let text = String::from_utf8_lossy(&bytes).to_string();
        // `serde_json` 的判定代替 `JSON.parse` 的判定；两者判得不一样的那几份（`1e400` 这样的数、
        // 孤立代理项）在下面走“不抛错”那一路，差异记在 `PORTING.md` 差异表 #101、#104。
        let parsed = serde_json::from_str::<Value>(&text).ok();
        for chunk in [
            ChunkSize::Fixed(text.len().max(1)),
            ChunkSize::Fixed(1),
            ChunkSize::Fixed(3),
        ] {
            let steps = stream(&text, chunk);
            let snapshot = steps.last().and_then(|step| step.snapshot.as_ref());
            match &parsed {
                Some(expected) => assert_eq!(snapshot, Some(expected), "{name}"),
                None => assert_plain_json(snapshot, &name),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 写坏的文字
// ---------------------------------------------------------------------------

const MALFORMED: &[&str] = &[
    "{\"a\":1 \"b\":2}",
    "{\"a\" 1, \"b\": 2}",
    "{a:1, 'b': 2}",
    "{\"a\":1,,\"b\":2,}",
    "[1 2, , 3,]",
    "{\"a\":[1,2}, \"b\":3]",
    "]]]}}}{",
    "{\"a\":NaN,\"b\":Infinity,\"c\":-Infinity,\"d\":undefined,\"e\":1}",
    "{\"a\":01,\"b\":1.,\"c\":.5,\"d\":+1,\"e\":0x10,\"f\":1e}",
    "{\"a\":\"x\"} trailing garbage {\"b\":2}",
    "{\"a\": tru, \"b\": nulll, \"c\": falsey}",
    "{\"a\":{\"b\":{\"c\":",
    "{\"a\\\": 1}",
    "\"unterminated \\",
    "// comment\n{\"a\":1}",
    "\u{0}{\u{1}\"a\"\u{2}:\u{3}1}",
    "{\"a\":\"x\"}}}]]]",
];

#[test]
fn never_throws_keeps_snapshots_plain_json_and_unchanged_at_every_chunk_size() {
    for text in MALFORMED {
        for chunk in CHUNK_SIZES {
            let steps = stream_ok(text, *chunk);
            for step in &steps {
                assert_plain_json(step.snapshot.as_ref(), text);
                assert_eq!(step.snapshot, step.copy, "{text}");
            }
            // 每种分段读写坏的文字读得一样。
            assert_eq!(
                steps.last().and_then(|step| step.snapshot.clone()),
                final_snapshot(text, ChunkSize::Fixed(text.len().max(1))),
                "{text}"
            );
        }
    }
}

#[test]
fn tolerates_missing_commas_and_colons_and_drops_values_in_invalid_positions() {
    assert_eq!(
        final_snapshot("{\"a\":1 \"b\":2}", ChunkSize::Fixed(1)),
        Some(json!({ "a": 1, "b": 2 }))
    );
    assert_eq!(
        final_snapshot("{\"a\" 1, \"b\": 2}", ChunkSize::Fixed(1)),
        Some(json!({ "a": 1, "b": 2 }))
    );
    assert_eq!(
        final_snapshot("[1 2, , 3,]", ChunkSize::Fixed(1)),
        Some(json!([1, 2, 3]))
    );
    assert_eq!(
        final_snapshot("{\"a\":NaN,\"b\":Infinity,\"e\":1}", ChunkSize::Fixed(1)),
        Some(json!({ "e": 1 }))
    );
    assert_eq!(
        final_snapshot(
            "{\"a\":\"x\"} trailing garbage {\"b\":2}",
            ChunkSize::Fixed(1)
        ),
        Some(json!({ "a": "x" }))
    );
}

#[test]
fn survives_random_corruption_of_valid_documents() {
    let mut rand = Random::new(7);
    let noise = [
        "\"", "\\", "{", "}", "[", "]", ",", ":", "x", "1", "-", "\n", "t",
    ];
    for text in random_documents(8, 300, true) {
        let mut characters: Vec<char> = text.chars().collect();
        for _ in 0..3 {
            let at = rand.range(characters.len() + 1);
            if rand.next() < 0.5 && at < characters.len() {
                characters.remove(at);
            } else {
                characters.insert(at, pick(&mut rand, &noise).chars().next().expect("a char"));
            }
        }
        let corrupted: String = characters.into_iter().collect();
        for step in stream_ok(&corrupted, ChunkSize::Random) {
            assert_plain_json(step.snapshot.as_ref(), &corrupted);
        }
        // 改动拼出来的一份与解析器自己的值相同，写坏的文字也一样。
        let mut parser = StreamingJsonParser::new();
        let mut mirror = Mirror::default();
        let mut index = 0;
        while index < corrupted.len() {
            let end = next_boundary(&corrupted, (index + 4).min(corrupted.len()));
            mirror.apply(&parser.append(&corrupted[index..end]).expect("readable"));
            index = end;
        }
        // 空格结束文字末尾的数字或字面量，快照会显示它，改动还没报。
        for change in parser.append(" ").expect("readable") {
            mirror.apply(std::slice::from_ref(&change));
        }
        assert_eq!(
            mirror.value.as_ref(),
            parser.snapshot().as_deref(),
            "{corrupted}"
        );
    }
}

// ---------------------------------------------------------------------------
// 改动
// ---------------------------------------------------------------------------

/// 靠应用解析器的改动跟进的一份值副本，就像 Durable 那种写者。
#[derive(Default)]
struct Mirror {
    value: Option<Value>,
}

impl Mirror {
    fn apply(&mut self, changes: &[JsonChange]) {
        for change in changes {
            match change {
                JsonChange::Set { path, value } => {
                    let value = value.clone();
                    if path.is_empty() {
                        self.value = Some(value);
                        continue;
                    }
                    let parent = self.parent(&path[..path.len() - 1]);
                    match path.last().expect("a segment") {
                        JsonPathSegment::Index(index) => {
                            let elements = parent.as_array_mut().expect("an array");
                            if *index == elements.len() {
                                elements.push(value);
                            } else {
                                elements[*index] = value;
                            }
                        }
                        JsonPathSegment::Key(key) => {
                            parent
                                .as_object_mut()
                                .expect("an object")
                                .insert(key.clone(), value);
                        }
                    }
                }
                JsonChange::Append { path, text } => {
                    if path.is_empty() {
                        let current = self.value.take().expect("a root string");
                        self.value = Some(Value::String(format!(
                            "{}{text}",
                            current.as_str().expect("a string")
                        )));
                        continue;
                    }
                    let parent = self.parent(&path[..path.len() - 1]);
                    let slot = match path.last().expect("a segment") {
                        JsonPathSegment::Index(index) => {
                            &mut parent.as_array_mut().expect("an array")[*index]
                        }
                        JsonPathSegment::Key(key) => parent
                            .as_object_mut()
                            .expect("an object")
                            .get_mut(key)
                            .expect("a member"),
                    };
                    *slot = Value::String(format!("{}{text}", slot.as_str().expect("a string")));
                }
            }
        }
    }

    fn parent(&mut self, path: &[JsonPathSegment]) -> &mut Value {
        let mut current = self.value.as_mut().expect("a parent value");
        for segment in path {
            current = match segment {
                JsonPathSegment::Index(index) => {
                    &mut current.as_array_mut().expect("an array")[*index]
                }
                JsonPathSegment::Key(key) => current
                    .as_object_mut()
                    .expect("an object")
                    .get_mut(key)
                    .expect("a member"),
            };
        }
        current
    }
}

/// 把 `text` 分成小段喂进解析器与它的副本；每段之后核对副本。
fn mirror_stream(text: &str, chunk: ChunkSize) -> Mirror {
    let mut parser = StreamingJsonParser::new();
    let mut mirror = Mirror::default();
    let mut rand = matches!(chunk, ChunkSize::Random).then(|| Random::new(read32(text)));
    let mut index = 0;
    while index < text.len() {
        let size = match &mut rand {
            Some(rand) => 1 + rand.range(12),
            None => match chunk {
                ChunkSize::Fixed(size) => size,
                ChunkSize::Random => unreachable!("the random seed is built above"),
            },
        };
        let end = next_boundary(text, (index + size).min(text.len()));
        mirror.apply(&parser.append(&text[index..end]).expect("readable"));
        index = end;
        // 改动不含没写完的数字与字面量，快照含。
        assert_eq!(
            mirror.value,
            reference_snapshot(&text[..index], false),
            "{:?}",
            &text[..index]
        );
    }
    // 文字末尾的数字或字面量要等一个分隔符才算结束；改动不报没结束的，快照报（上游的
    // `survives random corruption` 用例末尾同样补一个空格）。
    for change in parser.append(" ").expect("readable") {
        mirror.apply(std::slice::from_ref(&change));
    }
    mirror
}

#[test]
fn changes_keep_a_copy_equal_to_the_value_read_so_far_and_to_json_parse_at_the_end() {
    for text in random_documents(9, 200, true) {
        for chunk in CHUNK_SIZES {
            let mirror = mirror_stream(&text, *chunk);
            assert_eq!(
                mirror.value,
                Some(serde_json::from_str(&text).expect("valid JSON")),
                "{text}"
            );
        }
    }
}

fn key(path: &str) -> JsonPathSegment {
    JsonPathSegment::Key(path.to_string())
}

fn index(index: usize) -> JsonPathSegment {
    JsonPathSegment::Index(index)
}

#[test]
fn changes_report_a_number_that_ends_and_a_string_that_starts_in_one_piece_in_order() {
    let mut parser = StreamingJsonParser::new();
    assert_eq!(
        parser.append("{\"n\":1").expect("readable"),
        vec![JsonChange::Set {
            path: Vec::new(),
            value: json!({})
        }]
    );
    assert_eq!(
        parser.append("2,\"s\":\"ab").expect("readable"),
        vec![
            JsonChange::Set {
                path: vec![key("n")],
                value: json!(12)
            },
            JsonChange::Set {
                path: vec![key("s")],
                value: json!("ab")
            }
        ]
    );
    assert_eq!(
        parser.append("c\",\"t\":tru").expect("readable"),
        vec![JsonChange::Append {
            path: vec![key("s")],
            text: "c".to_string()
        }]
    );
    assert_eq!(
        parser.append("e}").expect("readable"),
        vec![JsonChange::Set {
            path: vec![key("t")],
            value: Value::Bool(true)
        }]
    );
    assert_eq!(
        parser.append(" {\"ignored\":1}").expect("readable"),
        Vec::new()
    );
}

#[test]
fn changes_report_nested_objects_and_arrays_as_empty_containers_then_their_members() {
    let mut parser = StreamingJsonParser::new();
    assert_eq!(
        parser.append("{\"a\":[1,{\"b\":[\"x\"").expect("readable"),
        vec![
            JsonChange::Set {
                path: Vec::new(),
                value: json!({})
            },
            JsonChange::Set {
                path: vec![key("a")],
                value: json!([])
            },
            JsonChange::Set {
                path: vec![key("a"), index(0)],
                value: json!(1)
            },
            JsonChange::Set {
                path: vec![key("a"), index(1)],
                value: json!({})
            },
            JsonChange::Set {
                path: vec![key("a"), index(1), key("b")],
                value: json!([])
            },
            JsonChange::Set {
                path: vec![key("a"), index(1), key("b"), index(0)],
                value: json!("x")
            }
        ]
    );
    assert_eq!(
        parser.append(",\"y").expect("readable"),
        vec![JsonChange::Set {
            path: vec![key("a"), index(1), key("b"), index(1)],
            value: json!("y")
        }]
    );
    assert_eq!(
        parser.append("z\"]}],\"a\":\"again\"}").expect("readable"),
        vec![
            JsonChange::Append {
                path: vec![key("a"), index(1), key("b"), index(1)],
                text: "z".to_string()
            },
            JsonChange::Set {
                path: vec![key("a")],
                value: json!("again")
            }
        ]
    );
}

#[test]
fn changes_hold_split_escapes_back_and_report_their_decoded_text_once_complete() {
    let text = r#"{"a":"x\u00e9\n\\\"\ud83d\ude00\q"}"#;
    let expected_value = expected(r#"{"a":"x\u00e9\n\\\"\ud83d\ude00\\q"}"#);
    let boundaries: Vec<usize> = (0..=text.len())
        .filter(|index| text.is_char_boundary(*index))
        .collect();
    for first in 1..text.len() {
        if !text.is_char_boundary(first) {
            continue;
        }
        for second in boundaries.iter().copied().filter(|second| *second >= first) {
            let mut parser = StreamingJsonParser::new();
            let mut mirror = Mirror::default();
            for piece in [&text[..first], &text[first..second], &text[second..]] {
                mirror.apply(&parser.append(piece).expect("readable"));
            }
            assert_eq!(mirror.value, Some(expected_value.clone()));
        }
    }
    let mut parser = StreamingJsonParser::new();
    parser.append("{\"a\":\"x").expect("readable");
    assert_eq!(parser.append("\\u00").expect("readable"), Vec::new());
    assert_eq!(
        parser.append("e9").expect("readable"),
        vec![JsonChange::Append {
            path: vec![key("a")],
            text: "é".to_string()
        }]
    );
}

#[test]
fn changes_report_a_container_deeper_than_32_levels_once_whole_when_it_closes() {
    let depth = 40;
    let text = format!("{}\"deep\"{}", "{\"a\":".repeat(depth), "}".repeat(depth));
    let mut parser = StreamingJsonParser::new();
    let mut mirror = Mirror::default();
    let mut index = 0;
    while index < text.len() {
        let end = next_boundary(&text, (index + 3).min(text.len()));
        mirror.apply(&parser.append(&text[index..end]).expect("readable"));
        index = end;
    }
    assert_eq!(mirror.value, Some(expected(&text)));
    let changes = StreamingJsonParser::new().append(&text).expect("readable");
    // 路径长度 32 以内的容器，然后长度 32 的那个在关闭时整份再报一次。
    assert_eq!(changes.len(), 34);
    let last = changes.last().expect("a last change");
    match last {
        JsonChange::Set { path, value } => {
            assert_eq!(path.len(), 32);
            assert_eq!(*value, expected(&text[32 * 5..text.len() - 32]));
        }
        JsonChange::Append { .. } => panic!("the last change is a set"),
    }
}

#[test]
fn changes_report_nothing_for_values_in_invalid_positions_and_text_after_the_root() {
    let mut parser = StreamingJsonParser::new();
    assert_eq!(
        parser.append("{\"a\":1 2, \"b\" \"x\"}").expect("readable"),
        vec![
            JsonChange::Set {
                path: Vec::new(),
                value: json!({})
            },
            JsonChange::Set {
                path: vec![key("a")],
                value: json!(1)
            },
            JsonChange::Set {
                path: vec![key("b")],
                value: json!("x")
            }
        ]
    );
    assert_eq!(parser.append("{\"c\":3}").expect("readable"), Vec::new());
}

// ---------------------------------------------------------------------------
// 代价
// ---------------------------------------------------------------------------

/// 数当前线程的分配量。`memory-helpers.ts` 的 `allocatedBytes` 的替代：只数测试线程自己的分配，
/// 别的测试并行跑不影响。
struct CountingAllocator;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            count_allocation(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_pointer = unsafe { System.realloc(pointer, layout, new_size) };
        if !new_pointer.is_null() {
            count_allocation(new_size.saturating_sub(layout.size()));
        }
        new_pointer
    }
}

fn count_allocation(bytes: usize) {
    let _ = COUNTING.try_with(|counting| {
        if counting.get() {
            let _ = ALLOCATED.try_with(|allocated| allocated.set(allocated.get() + bytes));
        }
    });
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// 跑 `run`，返回它在这一线程上分配掉的字节数。
fn allocated_bytes<T>(run: impl FnOnce() -> T) -> (usize, T) {
    ALLOCATED.with(|allocated| allocated.set(0));
    COUNTING.with(|counting| counting.set(true));
    let value = run();
    COUNTING.with(|counting| counting.set(false));
    (ALLOCATED.with(Cell::get), value)
}

fn write_content() -> String {
    (0..20_000)
        .map(|line| format!("const line{line} = \"value {line}\"; // \\\\ done\n"))
        .collect()
}

/// `write` 式的参数，带 `size` 个字符的文件内容。
fn write_arguments(content: &str, size: usize) -> String {
    serde_json::to_string(&json!({
        "path": "/src/file.ts",
        "content": content.chars().take(size).collect::<String>(),
    }))
    .expect("a JSON value")
}

#[test]
fn allocates_linearly_in_the_streamed_text_never_per_accumulated_text() {
    let content = write_content();
    let small = write_arguments(&content, 50_000);
    let large = write_arguments(&content, 500_000);
    let stream_allocation = |text: &str| {
        allocated_bytes(|| {
            let mut args = ToolCallArgumentsStream::new();
            let mut index = 0;
            while index < text.len() {
                let end = next_boundary(text, (index + 32).min(text.len()));
                args.append(&text[index..end]);
                index = end;
            }
            args.finish()
        })
        .0
    };
    let small_bytes = stream_allocation(&small);
    let large_bytes = stream_allocation(&large);
    assert!(
        large_bytes as f64 / large.len() as f64 <= 200.0,
        "{} bytes for {} characters",
        large_bytes,
        large.len()
    );
    // 十倍文字：线性增长约十倍，平方约一百倍。
    assert!(
        large_bytes as f64 / small_bytes as f64 <= 20.0,
        "{large_bytes} vs {small_bytes}"
    );
}

#[test]
fn keeps_very_large_open_arrays_linear_by_letting_their_preview_lag() {
    let content = write_content();
    let _ = content;
    let elements = |count: usize| {
        serde_json::to_string(&json!({
            "items": (0..count).collect::<Vec<usize>>(),
        }))
        .expect("a JSON value")
    };
    let small = elements(10_000);
    let large = elements(100_000);
    let stream_allocation = |text: &str| {
        allocated_bytes(|| {
            let mut parser = StreamingJsonParser::new();
            let mut index = 0;
            while index < text.len() {
                let end = next_boundary(text, (index + 16).min(text.len()));
                parser.append(&text[index..end]).expect("readable");
                parser.snapshot();
                index = end;
            }
            parser.snapshot()
        })
        .0
    };
    let small_bytes = stream_allocation(&small);
    let large_bytes = stream_allocation(&large);
    // 上游这里卡 2000 字节/字符（V8 的数组复制）；Rust 的值是深复制的，一个成员约 32 字节
    // （差异表 #107），所以绝对量比 V8 高，线性增长由下一条断言守住。
    assert!(
        large_bytes as f64 / large.len() as f64 <= 4000.0,
        "{} bytes for {} characters",
        large_bytes,
        large.len()
    );
    assert!(
        large_bytes as f64 / small_bytes as f64 <= 20.0,
        "{large_bytes} vs {small_bytes}"
    );

    let mut parser = StreamingJsonParser::new();
    let mut shown = 0;
    let mut index = 0;
    while index < large.len() {
        let end = next_boundary(&large, (index + 16).min(large.len()));
        parser.append(&large[index..end]).expect("readable");
        let items = parser
            .snapshot()
            .as_deref()
            .and_then(|value| value.get("items"))
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        // 预览只往前走。
        assert!(items >= shown, "{items} < {shown}");
        shown = items;
        index = end;
    }
    assert_eq!(
        parser.snapshot().as_deref(),
        Some(&serde_json::from_str(&large).expect("valid JSON"))
    );
}

#[test]
fn reads_deep_nesting_without_recursion_and_in_linear_time() {
    // 上游用 200_000 层；`serde_json` 的值最多 127 层容器（差异表 #102），所以这里用上限本身。
    let depth = 127;
    let text = format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
    let started = std::time::Instant::now();
    let mut parser = StreamingJsonParser::new();
    let mut index = 0;
    while index < text.len() {
        let end = next_boundary(&text, (index + 7).min(text.len()));
        parser.append(&text[index..end]).expect("readable");
        parser.snapshot();
        index = end;
    }
    let mut value = Some(parser.snapshot().expect("a value").as_ref().clone());
    for _ in 0..depth {
        let next = match value {
            Some(Value::Array(items)) => items[0].clone(),
            _ => panic!("a nested array"),
        };
        value = Some(next);
    }
    assert_eq!(value, Some(json!(1)));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn reads_one_huge_piece_such_as_a_providers_complete_arguments_in_linear_time() {
    let content = write_content();
    let text = write_arguments(&content, 500_000);
    let (bytes, _) = allocated_bytes(|| {
        let mut parser = StreamingJsonParser::new();
        parser.append(&text).expect("readable");
        parser.snapshot()
    });
    assert!(
        bytes as f64 / text.len() as f64 <= 100.0,
        "{bytes} bytes for {} characters",
        text.len()
    );
}

// ---------------------------------------------------------------------------
// 一个数大过 f64、一份文字深过 127 层（差异表 #101、#102）
// ---------------------------------------------------------------------------

#[test]
fn reports_a_number_too_large_for_f64_the_way_rust_reads_it() {
    let mut parser = StreamingJsonParser::new();
    parser.append("{\"n\":1").expect("readable");
    // 没写完的数不显示、也不报错。
    parser.append("e400").expect("the number is not finished");
    assert_eq!(parser.snapshot().as_deref(), Some(&json!({})));
    // 分隔符一到，这个数就结束了：按 Rust 的读法报错。
    assert_eq!(
        parser.append("}").expect_err("a number out of range"),
        StreamingJsonError::NumberOutOfRange {
            token: "1e400".to_string()
        }
    );
    // 报错之后解析停住，快照还是最后一个能读的值。
    assert_eq!(parser.snapshot().as_deref(), Some(&json!({})));
    assert!(parser.append("}").is_err());
    // 一份完整文字：`finish()` 的整份解析报 `serde_json` 的话，带模型发来的原文。
    let mut args = ToolCallArgumentsStream::new();
    let partial = args.append("{\"n\":1e400}");
    assert_eq!(*partial, Map::new());
    let finished = args.finish();
    assert!(finished.arguments_error.is_some());
    assert!(finished
        .arguments_error
        .as_deref()
        .expect("an error")
        .ends_with("{\"n\":1e400}"));
    // 1e400 之外的两个边界：1e-400 是 0，1e308 读得了。
    assert_eq!(
        final_snapshot("[1e-400]", ChunkSize::Fixed(1)),
        Some(json!([0.0]))
    );
    assert_eq!(
        final_snapshot("[1e308]", ChunkSize::Fixed(1))
            .expect("a value")
            .as_array()
            .expect("an array")[0]
            .as_f64(),
        Some(1e308)
    );
}

#[test]
fn reports_a_document_deeper_than_serde_jsons_recursion_limit() {
    let readable = format!("{}1{}", "[".repeat(127), "]".repeat(127));
    assert_eq!(
        final_snapshot(&readable, ChunkSize::Fixed(3)),
        Some(serde_json::from_str(&readable).expect("valid JSON"))
    );
    let too_deep = format!("{}1{}", "[".repeat(128), "]".repeat(128));
    let steps = stream(&too_deep, ChunkSize::Fixed(3));
    assert!(
        steps
            .iter()
            .any(|step| step.error == Some(StreamingJsonError::RecursionLimitExceeded)),
        "the 128th container is reported"
    );
    // 读到的那一份只到 127 层，仍是普通的 JSON。
    assert_plain_json(
        steps.last().and_then(|step| step.snapshot.as_ref()),
        "too deep",
    );
    // 整份解析报 `serde_json` 的话。
    let mut args = ToolCallArgumentsStream::new();
    args.append(&too_deep);
    let finished = args.finish();
    assert!(finished
        .arguments_error
        .as_deref()
        .expect("an error")
        .starts_with("recursion limit exceeded"));
}

#[test]
fn keeps_negative_zero_as_negative_zero() {
    let mut parser = StreamingJsonParser::new();
    parser.append("[-0]").expect("readable");
    let value = parser.snapshot().expect("a value");
    let number = value.as_array().expect("an array")[0]
        .as_f64()
        .expect("a number");
    assert_eq!(number, 0.0);
    assert!(number.is_sign_negative(), "-0 keeps its sign");
}

// ---------------------------------------------------------------------------
// ToolCallArgumentsStream
// ---------------------------------------------------------------------------

#[test]
fn tool_call_arguments_show_partial_arguments_as_an_object_and_finish_with_one_full_parse() {
    let mut args = ToolCallArgumentsStream::new();
    assert_eq!(*args.append("  "), Map::new());
    assert_eq!(
        *args.append("{\"path\":\"a.ts\",\"content\":\"x"),
        json!({ "path": "a.ts", "content": "x" })
            .as_object()
            .expect("an object")
            .clone()
    );
    let before = args.append("y");
    assert!(
        Arc::ptr_eq(&args.append(""), &before),
        "an unchanged snapshot"
    );
    assert_eq!(args.text(), "  {\"path\":\"a.ts\",\"content\":\"xy");
    assert_eq!(
        args.finish(),
        parse_tool_call_arguments("  {\"path\":\"a.ts\",\"content\":\"xy")
    );
    args.append("\"}");
    assert_eq!(
        args.finish(),
        parse_tool_call_arguments("  {\"path\":\"a.ts\",\"content\":\"xy\"}")
    );
}

#[test]
fn tool_call_arguments_show_non_object_roots_as_no_arguments_yet() {
    let mut args = ToolCallArgumentsStream::new();
    assert_eq!(*args.append("[1,2"), Map::new());
    args.append("]");
    assert!(args
        .finish()
        .arguments_error
        .as_deref()
        .expect("an error")
        .starts_with("Tool arguments must be a JSON object"));
}

#[test]
fn tool_call_arguments_replace_the_text_with_the_providers_complete_text_returning_the_missing_piece(
) {
    let mut args = ToolCallArgumentsStream::new();
    args.append("{\"a\":\"x");
    let replaced = args.replace("{\"a\":\"xyz\"}");
    assert_eq!(replaced.delta.as_deref(), Some("yz\"}"));
    assert_eq!(
        *replaced.arguments,
        json!({ "a": "xyz" })
            .as_object()
            .expect("an object")
            .clone()
    );
    let replaced = args.replace("{\"b\":2}");
    assert_eq!(replaced.delta, None);
    assert_eq!(
        *replaced.arguments,
        json!({ "b": 2 }).as_object().expect("an object").clone()
    );
    assert_eq!(args.text(), "{\"b\":2}");
}

#[test]
fn tool_call_arguments_are_owned_by_the_stream_once_the_stream_is_dropped() {
    // 上游这里等 GC 回收后再看 `WeakRef`；Rust 没有 GC：流连同它持有的文字、快照一起按所有权释放
    // （差异表 #105）。这一条守住“交出去的是流自己的值，没有全局状态”。
    let mut args = ToolCallArgumentsStream::new();
    args.append("{\"a\":\"x");
    assert_eq!(args.finish().arguments, Map::new(), "the text is cut off");
    args.append("y\"}");
    let partial = args.append(" ");
    assert_eq!(
        *partial,
        json!({ "a": "xy" }).as_object().expect("an object").clone()
    );
    drop(args);
}
