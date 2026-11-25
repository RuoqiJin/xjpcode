//! 对应上游 `packages/ai/src/utils/streaming-json.ts`。
//!
//! 把一段一段到达的 JSON 文字增量、宽容地读成当下的值，比如流式工具调用的参数。
//! `StreamingJsonParser::append()` 把每段文字只读一遍，所以一份文字流花的时间与内存与它的长度成正比；
//! `snapshot()` 给出现在读到的值。快照是普通的 JSON 值，交出去以后不再变：解析器把还开着的容器
//! （“打开路径”）复制进每一份新快照，已完成的部分与字符串共享。没写完的文字的规矩照
//! `parseStreamingJson()`（上游 `json-parse.ts`，本次没搬，见 `PORTING.md` 差异表）：
//!
//! - 没写完的字符串先显示已经读到的字符；没写完的转义先压着，等它写完。
//! - 没写完的容器先显示已经读到的成员与元素。
//! - 键或值还没开始的成员不显示。
//! - 没写完的字面量在它还是 `true`、`false`、`null` 的前缀时显示；没写完的数字在已读到的
//!   数字本身构成一个数时显示（指数部分还没数字时不显示）。
//!
//! 写坏的文字按宽容的办法读，与 `repairJson()`、`parseStreamingJson()` 一样：字符串里的原始控制字符留着，
//! `\d` 这样的非法转义留成字面文字，缺逗号、缺冒号都算了，位置不对的值与不认识的词丢掉，根值之后的文字
//! 忽略。解析器不抛错：读不了的东西只有两样，都是 Rust 的值类型逼出来的（见下面的差异与 `PORTING.md`
//! 差异表）：大过 `f64` 的数（pi 给 `Infinity`），以及深过 `serde_json` 递归上限的容器。
//!
//! 与 `parseStreamingJson()` 在没写完的文字上不同的地方：没写完的字符串尾部的空白留着（上游会去掉），
//! 含原始控制字符或非法转义的字符串边流边可见（上游要等整份文字完整了才可见），`NaN` 与 `Infinity`
//! 两个词丢掉。
//!
//! `append()` 还返回这一段文字造成的改动，给自己留着一份值副本、靠应用改动跟进的使用方：键、数组元素、
//! 结束的值出现时是 `set`（新字符串在第一次有文字时报一次，容器报空的）；这一段文字延长了一个没写完的
//! 字符串时是 `append`。没写完的数字与字面量要等它们结束才报。嵌套深过 32 层的容器在关闭时整份报一次
//! `set`，好让路径短。
//!
//! 代价：复制打开路径的代价是它的成员与元素个数。每次 `append()` 按文字长度加一个常数挣得复制额度；
//! `snapshot()` 花掉它，打开路径比额度贵时返回上一份快照。所以一直开着一个很大的容器（几千个元素）的
//! 文字，预览会比文字稍微落后一点，而没有输入能让总工作量比文字增长得更快。
//!
//! 与上游必须不同的地方（见 `PORTING.md` 差异表 #101–#109）：
//!
//! - 值固定是 `serde_json::Value`：`1e400`（pi 给 `Infinity`）与深过 127 层容器的文字按 Rust 的读法报错，
//!   流式读到那里就停在最后一个能读的值上，整份文字读完时由 `finish()` 的整份解析报出来；
//! - `snapshot()` 交出 `Arc<JsonValue>`（上游交出同一个对象引用），`ToolCallArgumentsStream` 交出的参数
//!   也是 `Arc<JsonObject>`；
//! - 一份快照复制打开路径里已完成的部分是深复制（上游按引用共享），所以代价按成员数记，比实际略低；
//!   没写完的字符串按已读到的长度记（上游字符串按引用共享，不计代价），长字符串的预览会更落后；
//! - 孤立代理项（`\ud83d` 后面没有低半）在 Rust 的 `String` 里没有对应值：低半可能还在后面时先压着，
//!   字符串结束时显示成 U+FFFD；被拆到两段文字里的原始代理对不可能出现（`&str` 总是完整的 UTF-8）。

use std::fmt;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::types::{JsonObject, JsonValue};
use crate::utils::json_parse::{parse_tool_call_arguments, ParsedToolCallArguments};

/// pi: `JsonPath` = `readonly (string | number)[]`：从根值走过来的成员名与数组下标。
pub type JsonPath = Vec<JsonPathSegment>;

/// 路径上的一段。上游是 `string | number` 的联合，Rust 里要有名字。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonPathSegment {
    /// 对象成员名。
    Key(String),
    /// 数组下标。
    Index(usize),
}

/// pi: `JsonChange`：一段文字造成的改动，见模块注释。
#[derive(Debug, Clone, PartialEq)]
pub enum JsonChange {
    /// 出现了一个键、一个数组元素或一个结束的值。
    Set { path: JsonPath, value: JsonValue },
    /// 这一段文字延长了一个没写完的字符串。
    Append { path: JsonPath, text: String },
}

/// 读不了一段文字。
///
/// pi 的解析器什么都不抛，这两样在 Rust 里没有对应的值，只能报错（见 `PORTING.md` 差异表 #101、#102）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamingJsonError {
    /// pi 的 `Number("1e400")` 是 `Infinity`，`serde_json::Value` 装不下；`serde_json` 读这个数是
    /// `number out of range`。
    NumberOutOfRange {
        /// 读不了的那个数，JSON 写法（例如 `1e400`）。
        token: String,
    },
    /// 容器嵌套深过 `serde_json` 的递归上限（127 层），整份解析在这里也会报同一句话。
    RecursionLimitExceeded,
}

impl fmt::Display for StreamingJsonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NumberOutOfRange { token } => write!(formatter, "number out of range: {token}"),
            Self::RecursionLimitExceeded => {
                write!(
                    formatter,
                    "recursion limit exceeded ({MAX_DEPTH} nested containers)"
                )
            }
        }
    }
}

impl std::error::Error for StreamingJsonError {}

/// 一份值在它父容器里的位置；`None` 是“位置不对、读但不收”的值。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Slot {
    Root,
    Member(String),
    Element(usize),
}

/// 一个没写完的对象或数组。开着的容器不交出去，快照拿的是复制。
#[derive(Debug)]
struct Frame {
    container: JsonValue,
    slot: Option<Slot>,
    /// 容器所在的位置；不报告改动的容器没有。
    path: Option<JsonPath>,
    /// 里面的改动是否报告；不报告时，它在关闭时整份报为一个值。
    reports_inside: bool,
    /// 成员或元素个数；复制这个容器要花的代价。
    size: usize,
    /// 对象：最后读到的键，成员的值放到这里。
    key: Option<String>,
    phase: Phase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Key,
    Colon,
    Value,
    Comma,
}

/// 一个没写完的字符串。
#[derive(Debug)]
struct OpenString {
    target: Target,
    slot: Option<Slot>,
    text: String,
    /// 已读到的转义：空 = 没有，否则是一个反斜杠加最多五个字符。
    escape: String,
    /// 值字符串的位置；不报告改动时没有。
    path: Option<JsonPath>,
    /// 这段文字延长的那个改动（`changes` 里的下标），当它还是最新的一条时。
    change: Option<usize>,
    /// `\uXXXX` 读到的孤立高半，等低半；pi 的 JS 字符串里它本来就在。
    pending_high: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Key,
    Value,
}

/// 改动最多报到这么深，好让一条路径的代价是常数。
const MAX_REPORTED_DEPTH: usize = 32;

/// `serde_json` 的默认递归上限：最多 127 层容器。比这深的值装不进 `Value`，也不能安全地丢掉。
const MAX_DEPTH: usize = 127;

/// 每次 append 与每个追加字符挣得的复制额度。地板值让小文档每份快照都是新的。
const CREDIT_PER_APPEND: usize = 256;
const CREDIT_PER_CHARACTER: usize = 8;

/// 比这长的没写完数字/字面量在结束前不显示，好让 `snapshot()` 便宜。
const MAX_PENDING_SCALAR_LENGTH: usize = 64;

/// 增量、宽容的 JSON 解析器；规则与代价见模块注释。
#[derive(Debug, Default)]
pub struct StreamingJsonParser {
    root: Option<JsonValue>,
    done: bool,
    stack: Vec<Frame>,
    string: Option<OpenString>,
    scalar: String,
    scalar_slot: Option<Slot>,
    /// 打开路径上 `size + 1` 的和：一份新快照要复制的量。
    path_cost: usize,
    credit: usize,
    dirty: bool,
    snapshot: Option<Arc<JsonValue>>,
    /// 这一段文字造成的改动。
    changes: Vec<JsonChange>,
    /// 读不了的东西：粘住，之后的 `append()` 一直报它。
    error: Option<StreamingJsonError>,
}

impl StreamingJsonParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// 读下一段文字；返回这一段造成的改动，按顺序。代价与 `text.len()` 成正比。
    ///
    /// 读不了一个数（`1e400`）或嵌套过深时是 `Err`；那时解析停在最后一个能读的值上，之后的调用
    /// 继续报同一个错（`PORTING.md` 差异表 #101、#102）。
    pub fn append(&mut self, text: &str) -> Result<Vec<JsonChange>, StreamingJsonError> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        self.changes.clear();
        self.credit += CREDIT_PER_APPEND + CREDIT_PER_CHARACTER * text.len();
        if let Some(open) = &mut self.string {
            open.change = None;
        }
        let mut index = 0;
        while index < text.len() && !self.done {
            if self.string.is_some() {
                let mut open = self.string.take().expect("a string is open");
                let (next, closed) = self.read_string(&mut open, text, index);
                if !closed {
                    self.string = Some(open);
                }
                index = next;
            } else if !self.scalar.is_empty() {
                match self.read_scalar(text, index) {
                    Ok(next) => index = next,
                    Err(error) => {
                        // 粘住：之后的调用一直报它（见 `append` 的文档）。
                        self.error = Some(error.clone());
                        return Err(error);
                    }
                }
            } else {
                let char = text[index..].chars().next().expect("a character boundary");
                self.structural(char);
                index += char.len_utf8();
            }
            if let Some(error) = &self.error {
                return Err(error.clone());
            }
        }
        Ok(std::mem::take(&mut self.changes))
    }

    /// 现在读到的值：值开始之前是 `None`，否则是一个之后不再变的普通 JSON 值。文字自上次以来没有
    /// 改变值时，或者一份打开路径的新复制还没挣够额度时，返回上一份快照（见模块注释）。
    ///
    /// 交出去的是 `Arc` 句柄（上游交出同一个对象引用）：用过它的地方不必复制就能留住。
    pub fn snapshot(&mut self) -> Option<Arc<JsonValue>> {
        if self.dirty {
            let cost = self.path_cost + self.pending_cost();
            if cost <= self.credit {
                self.credit -= cost;
                self.dirty = false;
                let pending = self.pending();
                self.snapshot = self.copy_open_path(pending).map(Arc::new);
            }
        }
        self.snapshot.clone()
    }

    /// 读不了的那一样东西，如果已经读到过。
    pub fn error(&self) -> Option<&StreamingJsonError> {
        self.error.as_ref()
    }

    /// 从 `start` 读字符串里的字符，直到字符串结束或文字没有了；返回下一个下标与是否结束。
    fn read_string(&mut self, open: &mut OpenString, text: &str, start: usize) -> (usize, bool) {
        let mut run_start = start;
        let mut index = start;
        while index < text.len() {
            let char = text[index..].chars().next().expect("a character boundary");
            let width = char.len_utf8();
            if !open.escape.is_empty() {
                if self.continue_escape(open, char) {
                    index += width;
                    run_start = index;
                    continue;
                }
                // 不属于这个转义：下面当普通字符串字符读。
                run_start = index;
            }
            if char == '"' {
                if index > run_start {
                    self.append_string(open, &text[run_start..index]);
                }
                self.end_string(open);
                return (index + 1, true);
            }
            if char == '\\' {
                if index > run_start {
                    self.append_string(open, &text[run_start..index]);
                }
                open.escape.push('\\');
                run_start = index + 1;
            }
            index += width;
        }
        if text.len() > run_start {
            self.append_string(open, &text[run_start..]);
        }
        (text.len(), false)
    }

    /// 从 `start` 读数字或字面量的字符，直到下一个分隔符；返回分隔符的下标。
    fn read_scalar(&mut self, text: &str, start: usize) -> Result<usize, StreamingJsonError> {
        let bytes = text.as_bytes();
        let mut end = start;
        while end < bytes.len() && !is_delimiter(bytes[end]) {
            end += 1;
        }
        if end > start {
            self.scalar.push_str(&text[start..end]);
            if self.scalar_slot.is_some() {
                self.dirty = true;
            }
        }
        if end < bytes.len() {
            self.end_scalar()?;
        }
        Ok(end)
    }

    /// 处理字符串外面的一个字符。
    fn structural(&mut self, char: char) {
        match char {
            ' ' | '\t' | '\n' | '\r' => {}
            '"' => self.start_string(),
            '{' => self.open_container(Value::Object(Map::new())),
            '[' => self.open_container(Value::Array(Vec::new())),
            '}' | ']' => self.close_container(),
            ':' => {
                if let Some(frame) = self.stack.last_mut() {
                    if frame.container.is_object() && frame.phase == Phase::Colon {
                        frame.phase = Phase::Value;
                    }
                }
            }
            ',' => {
                if let Some(frame) = self.stack.last_mut() {
                    frame.phase = if frame.container.is_array() {
                        Phase::Value
                    } else {
                        Phase::Key
                    };
                }
            }
            _ => {
                self.scalar.clear();
                self.scalar.push(char);
                self.scalar_slot = self.value_slot();
                if self.scalar_slot.is_some() {
                    self.dirty = true;
                }
            }
        }
    }

    /// 现在开始的值放在哪里；这个位置不收值时是 `None`。
    fn value_slot(&self) -> Option<Slot> {
        let Some(frame) = self.stack.last() else {
            return if self.root.is_none() {
                Some(Slot::Root)
            } else {
                None
            };
        };
        if let Value::Array(elements) = &frame.container {
            return Some(Slot::Element(elements.len()));
        }
        // 缺冒号也算了：`{"a" 1}` 读成 `{"a": 1}`。
        if matches!(frame.phase, Phase::Value | Phase::Colon) {
            if let Some(key) = &frame.key {
                return Some(Slot::Member(key.clone()));
            }
        }
        None
    }

    fn start_string(&mut self) {
        if let Some(frame) = self.stack.last() {
            // 缺逗号也算了：`{"a": 1 "b": 2}` 读第二个成员。
            if frame.container.is_object() && matches!(frame.phase, Phase::Key | Phase::Comma) {
                self.string = Some(OpenString {
                    target: Target::Key,
                    slot: None,
                    text: String::new(),
                    escape: String::new(),
                    path: None,
                    change: None,
                    pending_high: None,
                });
                return;
            }
        }
        let slot = self.value_slot();
        let path = self.reported_path(slot.as_ref());
        self.place(slot.as_ref(), Value::String(String::new()));
        let change = path.as_ref().map(|path| {
            self.changes.push(JsonChange::Set {
                path: path.clone(),
                value: Value::String(String::new()),
            });
            self.changes.len() - 1
        });
        self.string = Some(OpenString {
            target: Target::Value,
            slot,
            text: String::new(),
            escape: String::new(),
            path,
            change,
            pending_high: None,
        });
    }

    /// `slot` 上的值在它所在的打开容器里的路径；改动不报告时是 `None`。
    fn reported_path(&self, slot: Option<&Slot>) -> Option<JsonPath> {
        let slot = slot?;
        if *slot == Slot::Root {
            return Some(Vec::new());
        }
        let frame = self.stack.last()?;
        if !frame.reports_inside {
            return None;
        }
        let mut path = frame.path.clone()?;
        match slot {
            Slot::Member(key) => path.push(JsonPathSegment::Key(key.clone())),
            Slot::Element(index) => path.push(JsonPathSegment::Index(*index)),
            Slot::Root => unreachable!("handled above"),
        }
        Some(path)
    }

    /// 用 `char` 继续一个转义；`char` 不属于它、要按普通字符读时返回 false。
    fn continue_escape(&mut self, open: &mut OpenString, char: char) -> bool {
        if open.escape == "\\" {
            if let Some(decoded) = simple_escape(char) {
                open.escape.clear();
                self.append_string(open, decoded);
                return true;
            }
            if char == 'u' {
                open.escape.push('u');
                return true;
            }
            // 非法转义：反斜杠留成文字，这个字符按普通字符读，与 `repairJson()` 一样。
            open.escape.clear();
            self.append_string(open, "\\");
            return false;
        }
        if char.is_ascii_hexdigit() {
            open.escape.push(char);
            if open.escape.len() < 6 {
                return true;
            }
            let code = u16::from_str_radix(&open.escape[2..], 16).expect("four hex digits");
            open.escape.clear();
            self.append_code_unit(open, code);
            return true;
        }
        // `\u` 后面不是四个十六进制数字：留成文字，这个字符按普通字符读，与 `repairJson()` 一样。
        let literal = std::mem::take(&mut open.escape);
        self.append_string(open, &literal);
        false
    }

    /// 把一个 `\uXXXX` 读到的码元接上去。孤立代理项在 Rust 的字符串里没有对应值（见模块注释）。
    fn append_code_unit(&mut self, open: &mut OpenString, code: u16) {
        if let Some(high) = open.pending_high.take() {
            if (0xDC00..=0xDFFF).contains(&code) {
                let combined =
                    0x10000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(code) - 0xDC00);
                let char = char::from_u32(combined).expect("a surrogate pair is a character");
                let mut buffer = [0u8; 4];
                self.append_string(open, char.encode_utf8(&mut buffer));
                return;
            }
            self.append_string(open, "\u{FFFD}");
        }
        if (0xD800..=0xDBFF).contains(&code) {
            open.pending_high = Some(code);
            return;
        }
        if (0xDC00..=0xDFFF).contains(&code) {
            self.append_string(open, "\u{FFFD}");
            return;
        }
        let char = char::from_u32(u32::from(code)).expect("a BMP code unit is a character");
        let mut buffer = [0u8; 4];
        self.append_string(open, char.encode_utf8(&mut buffer));
    }

    fn append_string(&mut self, open: &mut OpenString, text: &str) {
        open.text.push_str(text);
        if open.target != Target::Value {
            return;
        }
        // pi 的 `#place(open.slot, open.text)`：文字留在 buffer 里，快照时才放进容器（见模块注释）。
        if open.slot.is_some() {
            self.dirty = true;
        }
        if open.path.is_none() {
            return;
        }
        // 这段字符串还是本段文字最新的一条改动时延长它，否则报一条 append。
        if let Some(index) = open.change {
            if index + 1 == self.changes.len() {
                match &mut self.changes[index] {
                    JsonChange::Set { value, .. } => {
                        if let Value::String(value) = value {
                            value.push_str(text);
                        }
                    }
                    JsonChange::Append { text: current, .. } => current.push_str(text),
                }
                return;
            }
        }
        let path = open.path.clone().expect("checked above");
        self.changes.push(JsonChange::Append {
            path,
            text: text.to_string(),
        });
        open.change = Some(self.changes.len() - 1);
    }

    fn end_string(&mut self, open: &mut OpenString) {
        // 孤立的高半到这里不会有低半了：显示成替换字符。
        if open.pending_high.take().is_some() {
            open.text.push(char::REPLACEMENT_CHARACTER);
        }
        if open.target == Target::Key {
            if let Some(frame) = self.stack.last_mut() {
                frame.key = Some(open.text.clone());
                frame.phase = Phase::Colon;
            }
            return;
        }
        self.place(open.slot.as_ref(), Value::String(open.text.clone()));
        self.value_ended();
    }

    fn end_scalar(&mut self) -> Result<(), StreamingJsonError> {
        let token = std::mem::take(&mut self.scalar);
        let slot = self.scalar_slot.take();
        // 位置不对的值读了也不收：pi 只把它丢掉，值本身算不算得出来无关紧要，所以这里也不读它
        // （`{1e400:1}` 这样的键位置不该为一个大数报错）。
        if slot.is_some() {
            if let Some(value) = scalar_value(&token)? {
                self.place(slot.as_ref(), value.clone());
                let path = self.reported_path(slot.as_ref());
                if let Some(path) = path {
                    self.changes.push(JsonChange::Set { path, value });
                }
            }
        }
        self.value_ended();
        Ok(())
    }

    /// 没写完的那份值要复制多少：与 [`Self::pending`] 同一套判定，但不复制文字。
    fn pending_cost(&self) -> usize {
        if let Some(open) = &self.string {
            if open.target == Target::Value && open.slot.is_some() {
                return open.text.len().max(1);
            }
        }
        if self.scalar_slot.is_some()
            && !self.scalar.is_empty()
            && self.scalar.len() <= MAX_PENDING_SCALAR_LENGTH
            && pending_scalar_value(&self.scalar).is_some()
        {
            return 1;
        }
        0
    }

    /// 文字末尾那个没写完的数字或字面量显示的值。
    fn pending(&self) -> Option<(Slot, JsonValue)> {
        if let Some(open) = &self.string {
            if open.target == Target::Value {
                if let Some(slot) = &open.slot {
                    return Some((slot.clone(), Value::String(open.text.clone())));
                }
            }
        }
        let slot = self.scalar_slot.clone()?;
        if self.scalar.is_empty() || self.scalar.len() > MAX_PENDING_SCALAR_LENGTH {
            return None;
        }
        let value = pending_scalar_value(&self.scalar)?;
        Some((slot, value))
    }

    fn open_container(&mut self, container: JsonValue) {
        if self.stack.len() >= MAX_DEPTH {
            self.error = Some(StreamingJsonError::RecursionLimitExceeded);
            return;
        }
        let slot = self.value_slot();
        let path = self.reported_path(slot.as_ref());
        if let Some(path) = &path {
            // 新容器：工作的那份一直在变，改动里的这份不变。
            self.changes.push(JsonChange::Set {
                path: path.clone(),
                value: if container.is_object() {
                    Value::Object(Map::new())
                } else {
                    Value::Array(Vec::new())
                },
            });
        }
        let phase = if container.is_array() {
            Phase::Value
        } else {
            Phase::Key
        };
        self.stack.push(Frame {
            container,
            slot,
            path: path.clone(),
            reports_inside: path
                .as_ref()
                .is_some_and(|path| path.len() < MAX_REPORTED_DEPTH),
            size: 0,
            key: None,
            phase,
        });
        self.path_cost += 1;
        self.dirty = true;
    }

    fn close_container(&mut self) {
        // 容器外面多出来的结束符忽略。
        let Some(frame) = self.stack.pop() else {
            return;
        };
        self.path_cost -= frame.size + 1;
        self.dirty = true;
        // 太深、里面不报告改动：整份此刻终定的值报一次。
        if let (Some(path), false) = (&frame.path, frame.reports_inside) {
            self.changes.push(JsonChange::Set {
                path: path.clone(),
                value: frame.container.clone(),
            });
        }
        self.place(frame.slot.as_ref(), frame.container);
        self.value_ended();
    }

    /// `frame`（没有 frame 时是根值）里有一个值结束了。
    fn value_ended(&mut self) {
        match self.stack.last_mut() {
            None => {
                // 根值之后的文字忽略，但只在根值已经有了之后。
                if self.root.is_some() {
                    self.done = true;
                }
            }
            Some(frame) => frame.phase = Phase::Comma,
        }
    }

    /// 把值放到打开路径上的 `slot`；位置不对的值什么都不做。
    fn place(&mut self, slot: Option<&Slot>, value: JsonValue) {
        let Some(slot) = slot else {
            return;
        };
        self.dirty = true;
        if *slot == Slot::Root {
            self.root = Some(value);
            return;
        }
        let frame = self.stack.last_mut().expect("a slot inside a container");
        match slot {
            Slot::Element(index) => {
                let elements = frame
                    .container
                    .as_array_mut()
                    .expect("an element slot in an array");
                if *index == elements.len() {
                    frame.size += 1;
                    self.path_cost += 1;
                }
                if *index < elements.len() {
                    elements[*index] = value;
                } else {
                    elements.push(value);
                }
            }
            Slot::Member(key) => {
                let members = frame
                    .container
                    .as_object_mut()
                    .expect("a member slot in an object");
                if !members.contains_key(key) {
                    frame.size += 1;
                    self.path_cost += 1;
                }
                // `Map::insert` 与 JS 一样：同名的键保住第一次的位置，值换成新的。
                members.insert(key.clone(), value);
            }
            Slot::Root => unreachable!("handled above"),
        }
    }

    /// 一份新快照：还开着的容器各复制一份，从里往外，每一份装进它父容器的那一份里。
    fn copy_open_path(&self, mut pending: Option<(Slot, JsonValue)>) -> Option<JsonValue> {
        if self.stack.is_empty() {
            return match pending {
                Some((Slot::Root, value)) => Some(value),
                _ => self.root.clone(),
            };
        }
        let mut child: Option<JsonValue> = None;
        let mut child_slot: Option<Slot> = None;
        for depth in (0..self.stack.len()).rev() {
            let frame = &self.stack[depth];
            let mut copy = frame.container.clone();
            if depth + 1 == self.stack.len() {
                if let Some((slot, value)) = pending.take() {
                    set_slot(&mut copy, &slot, value);
                }
            }
            if let Some(slot) = child_slot.take() {
                if let Some(value) = child.take() {
                    set_slot(&mut copy, &slot, value);
                }
            }
            child = Some(copy);
            child_slot = frame.slot.clone();
        }
        match child_slot {
            Some(Slot::Root) => child,
            _ => self.root.clone(),
        }
    }
}

/// 流式工具调用的参数：原始文字（调用结束时整份解析用一次）与中间的
/// [`StreamingJsonParser`]。provider 在各自的流状态里每次调用留一份，从不放在内容块上，所以原始文字
/// 不会进到部分消息或 transcript 里。
#[derive(Debug, Default)]
pub struct ToolCallArgumentsStream {
    text: String,
    parser: StreamingJsonParser,
    snapshot: Option<Arc<JsonValue>>,
    arguments: Arc<JsonObject>,
}

impl ToolCallArgumentsStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// 已经收到的原始参数文字。
    pub fn text(&self) -> &str {
        &self.text
    }

    /// 接上一段参数文字；返回此刻的部分参数，变过时是一份新的对象。
    ///
    /// 与上游一样不报错：大过 `f64` 的数或深过 127 层的文字会让解析停住，整份文字读完时由
    /// [`Self::finish`] 的整份解析报出来（`PORTING.md` 差异表 #101、#102）。交出去的是 `Arc`：
    /// 与上一份是同一个句柄时，值没变（上游比较对象引用），`Arc::ptr_eq` 看得出这一点。
    pub fn append(&mut self, delta: &str) -> Arc<JsonObject> {
        self.text.push_str(delta);
        let _ = self.parser.append(delta);
        self.partial()
    }

    /// 拿 provider 给的完整参数文字。它接着已经收到的文字时，返回缺的那一段 `delta`；否则从头用它，
    /// 那一段是 `None`。两种情形都返回 `text` 的部分参数。
    pub fn replace(&mut self, text: &str) -> ReplacedToolCallArguments {
        if let Some(delta) = text.strip_prefix(self.text.as_str()) {
            let delta = delta.to_string();
            let arguments = if delta.is_empty() {
                self.partial()
            } else {
                self.append(&delta)
            };
            return ReplacedToolCallArguments {
                delta: Some(delta),
                arguments,
            };
        }
        self.text.clear();
        self.parser = StreamingJsonParser::new();
        ReplacedToolCallArguments {
            delta: None,
            arguments: self.append(text),
        }
    }

    /// 最后的参数：把完整文字整份解析一次，写坏的参数会报出来。
    pub fn finish(&self) -> ParsedToolCallArguments {
        parse_tool_call_arguments(&self.text)
    }

    fn partial(&mut self) -> Arc<JsonObject> {
        let snapshot = self.parser.snapshot();
        let unchanged = match (&snapshot, &self.snapshot) {
            (None, None) => true,
            (Some(current), Some(previous)) => Arc::ptr_eq(current, previous),
            _ => false,
        };
        if unchanged {
            return Arc::clone(&self.arguments);
        }
        self.snapshot = snapshot.clone();
        // 工具参数是一个对象；别的部分值先当成还没有参数。
        self.arguments = match snapshot.as_deref() {
            Some(Value::Object(members)) => Arc::new(members.clone()),
            _ => Arc::new(JsonObject::new()),
        };
        Arc::clone(&self.arguments)
    }
}

/// pi: `replace()` 的返回值 `{ delta: string | undefined; arguments }`。
#[derive(Debug, Clone)]
pub struct ReplacedToolCallArguments {
    /// 接着已经收到的文字时缺的那一段；从头用它时是 `None`。
    pub delta: Option<String>,
    /// `text` 的部分参数。
    pub arguments: Arc<JsonObject>,
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

/// pi: `isDelimiter`
fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b'\n' | b'\r' | b',' | b':' | b'{' | b'}' | b'[' | b']' | b'"'
    )
}

/// JSON 数字的写法：`^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?$`。
fn is_json_number(token: &str) -> bool {
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

/// 一个完整的数字或字面量的值；别的都丢掉。数字大过 `f64` 能表示的范围时是 `Err`。
fn scalar_value(token: &str) -> Result<Option<JsonValue>, StreamingJsonError> {
    match token {
        "true" => return Ok(Some(Value::Bool(true))),
        "false" => return Ok(Some(Value::Bool(false))),
        "null" => return Ok(Some(Value::Null)),
        _ => {}
    }
    if !is_json_number(token) {
        return Ok(None);
    }
    let value =
        serde_json::from_str::<Value>(token).map_err(|_| StreamingJsonError::NumberOutOfRange {
            token: token.to_string(),
        })?;
    Ok(Some(value))
}

/// 没写完的数字或字面量显示的值。
fn pending_scalar_value(token: &str) -> Option<JsonValue> {
    for (literal, value) in [
        ("true", Value::Bool(true)),
        ("false", Value::Bool(false)),
        ("null", Value::Null),
    ] {
        if literal.starts_with(token) {
            return Some(value);
        }
    }
    if is_json_number(token) {
        return serde_json::from_str::<Value>(token).ok();
    }
    let mantissa = mantissa_without_exponent_digits(token)?;
    serde_json::from_str::<Value>(mantissa).ok()
}

/// `^(-?(?:0|[1-9]\d*)(?:\.\d+)?)[eE][+-]?$` 里那个尾数。
fn mantissa_without_exponent_digits(token: &str) -> Option<&str> {
    let mantissa = ["e+", "e-", "E+", "E-"]
        .iter()
        .find_map(|suffix| token.strip_suffix(suffix))
        .or_else(|| token.strip_suffix('e'))
        .or_else(|| token.strip_suffix('E'))?;
    is_json_number(mantissa).then_some(mantissa)
}

fn set_slot(container: &mut JsonValue, slot: &Slot, value: JsonValue) {
    match slot {
        Slot::Element(index) => {
            let elements = container
                .as_array_mut()
                .expect("an element slot in an array");
            if *index < elements.len() {
                elements[*index] = value;
            } else {
                elements.push(value);
            }
        }
        Slot::Member(key) => {
            // `__proto__` 在 Rust 里就是一个普通成员（JS 里要 `Object.defineProperty`），与 `JSON.parse()` 相同。
            container
                .as_object_mut()
                .expect("a member slot in an object")
                .insert(key.clone(), value);
        }
        Slot::Root => {}
    }
}
