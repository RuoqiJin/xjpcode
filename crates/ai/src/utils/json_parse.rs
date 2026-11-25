//! 对应上游 `packages/ai/src/utils/json-parse.ts`。
//!
//! 模型把一次工具调用的参数文字给完以后，整份解析一次：先按 JSON 读，读不了就修一遍（字符串里的原始控制
//! 字符转义、非法转义的反斜杠加倍）再读。与 `parseStreamingJson()` 不同，这里不留“读到哪算哪”的前缀：调用
//! 结束了，不是合法 JSON 对象的文字就是模型发坏了参数，`arguments` 是空的，`arguments_error` 说明问题并
//! 附上模型发来的原文，好让工具参数检查把它读回给模型。
//!
//! 本次不搬的（见 `PORTING.md` 差异表 #103、#106）：
//!
//! - `parseStreamingJson()`：pi 里没有任何调用方，它用的第三方库 `partial-json` 一起不搬；流式读取由
//!   `streaming_json.rs` 的 `StreamingJsonParser` 承担（它照的就是这套规则）。
//! - 两处文字与 pi 不同，都是 Rust 的读法逼出来的：`JSON.parse` 的报错原文（这里用 `serde_json` 的话），
//!   以及孤立代理项（JS 的字符串能装下，Rust 的 `String` 装不下）。

use serde_json::{Map, Value};

use crate::types::{JsonObject, JsonValue};

/// JSON 字符串里认的转义字符。
const VALID_JSON_ESCAPES: &[char] = &['"', '\\', '/', 'b', 'f', 'n', 'r', 't'];

/// pi: `repairJson`——修好写坏的 JSON 字符串：
///
/// - 字符串里的原始控制字符转义；
/// - 非法转义字符前面的反斜杠加倍。
pub fn repair_json(json: &str) -> String {
    let mut repaired = String::with_capacity(json.len());
    let mut in_string = false;
    let mut index = 0;
    while index < json.len() {
        let char = json[index..].chars().next().expect("a character boundary");
        if !in_string {
            repaired.push(char);
            if char == '"' {
                in_string = true;
            }
            index += char.len_utf8();
            continue;
        }
        if char == '"' {
            repaired.push(char);
            in_string = false;
            index += 1;
            continue;
        }
        if char == '\\' {
            let next = json[index + 1..].chars().next();
            let Some(next) = next else {
                // 末尾只有一个反斜杠：加倍，字符串不闭合的问题留给解析器报。
                repaired.push_str("\\\\");
                index += 1;
                continue;
            };
            if next == 'u' {
                let digits: String = json[index + 2..].chars().take(4).collect();
                if digits.len() == 4 && digits.chars().all(|digit| digit.is_ascii_hexdigit()) {
                    repaired.push_str("\\u");
                    repaired.push_str(&digits);
                    index += 6;
                    continue;
                }
            }
            if VALID_JSON_ESCAPES.contains(&next) {
                repaired.push('\\');
                repaired.push(next);
                index += 2;
                continue;
            }
            // 非法转义：反斜杠加倍，后面的字符下一轮按普通字符读。
            repaired.push_str("\\\\");
            index += 1;
            continue;
        }
        if (char as u32) <= 0x1f {
            repaired.push_str(&escape_control_character(char));
        } else {
            repaired.push(char);
        }
        index += char.len_utf8();
    }
    repaired
}

/// 一个控制字符的 JSON 转义写法。
fn escape_control_character(char: char) -> String {
    match char {
        '\u{8}' => "\\b".to_string(),
        '\u{c}' => "\\f".to_string(),
        '\n' => "\\n".to_string(),
        '\r' => "\\r".to_string(),
        '\t' => "\\t".to_string(),
        _ => format!("\\u{:04x}", char as u32),
    }
}

/// pi: `parseJsonWithRepair`——先按 JSON 读；读不了就修一遍再读，修过没变就报原来的错。
pub fn parse_json_with_repair(json: &str) -> Result<JsonValue, serde_json::Error> {
    match serde_json::from_str::<JsonValue>(json) {
        Ok(value) => Ok(value),
        Err(error) => {
            let repaired = repair_json(json);
            if repaired != json {
                serde_json::from_str(&repaired)
            } else {
                Err(error)
            }
        }
    }
}

/// pi: `Pick<ToolCall, "arguments" | "argumentsError">`——整份参数文字读出来的两样东西。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedToolCallArguments {
    /// 参数；读不了或不是一个对象时是空的。
    pub arguments: JsonObject,
    /// 读不了时的说明，含模型发来的原文。
    pub arguments_error: Option<String>,
}

/// pi: `parseToolCallArguments`——解析模型给完的一次工具调用的完整参数文字。
///
/// 上游的参数可以是 `undefined`（等同于空文字）；这里空文字就是那个情形。
pub fn parse_tool_call_arguments(json: &str) -> ParsedToolCallArguments {
    if json.trim().is_empty() {
        return ParsedToolCallArguments::default();
    }
    match parse_json_with_repair(json) {
        Ok(Value::Object(arguments)) => ParsedToolCallArguments {
            arguments,
            arguments_error: None,
        },
        Ok(_) => ParsedToolCallArguments {
            arguments: Map::new(),
            arguments_error: Some(format!(
                "Tool arguments must be a JSON object\n\nReceived arguments:\n{json}"
            )),
        },
        Err(error) => ParsedToolCallArguments {
            arguments: Map::new(),
            arguments_error: Some(format!("{error}\n\nReceived arguments:\n{json}")),
        },
    }
}
