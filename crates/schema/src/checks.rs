//! 对应上游 `packages/schema/src/checks.ts`。
//!
//! 检查器工厂：每个函数返回一个闭包，闭包只捕获自己的参数，从不捕获编译器，所以编译好的 schema
//! 不会把编译期的状态留着。上游把叶子检查（类型、`const`、`enum`、边界）登记成“代码 + 参数”，
//! 让对象和数组的循环内联执行；Rust 侧不搬这件内联外衣（它是给 V8 的枚举缓存让路的），判定结果
//! 相同，见 `PORTING.md` 差异表。

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, OnceLock, Weak};

use regress::Regex;
use serde_json::Value;

use crate::types::{Check, FormatCheck};

// ---------------------------------------------------------------- 空检查

static ALWAYS_CHECK: LazyLock<Check> = LazyLock::new(|| Arc::new(|_: &Value| true));
static NEVER_CHECK: LazyLock<Check> = LazyLock::new(|| Arc::new(|_: &Value| false));

/// `true` schema：任何值都合格。
pub fn always() -> Check {
    ALWAYS_CHECK.clone()
}

/// `false` schema：任何值都不合格。
pub fn never() -> Check {
    NEVER_CHECK.clone()
}

/// `v` 是不是一个 JSON 对象：不是 null、数组或字节串。
pub fn is_object(v: &Value) -> bool {
    v.is_object()
}

// ---------------------------------------------------------------- 类型

fn is_string(v: &Value) -> bool {
    v.is_string()
}

/// 上游 `Number.isFinite`：JSON 里没有 NaN 和 Infinity，只有代码里造的数才会是。
fn is_number(v: &Value) -> bool {
    v.as_f64().is_some_and(f64::is_finite)
}

fn is_integer(v: &Value) -> bool {
    v.as_f64().is_some_and(is_integer_number)
}

fn is_integer_number(x: f64) -> bool {
    x.is_finite() && x.fract() == 0.0
}

fn is_boolean(v: &Value) -> bool {
    v.is_boolean()
}

fn is_null(v: &Value) -> bool {
    v.is_null()
}

/// `v` 是不是数组。
pub fn is_array(v: &Value) -> bool {
    v.is_array()
}

fn is_bytes(_v: &Value) -> bool {
    // 上游 `v instanceof Uint8Array`：本次移植被检查的值固定是 JSON，字节串无法出现。
    false
}

fn is_bigint(_v: &Value) -> bool {
    // 上游 `typeof v === "bigint"`：JSON 里没有 bigint。
    false
}

fn is_undefined(_v: &Value) -> bool {
    // 上游 `v === undefined`：JSON 里没有 undefined。
    false
}

fn static_check(check: fn(&Value) -> bool) -> Check {
    Arc::new(check)
}

/// `type` 名到检查器。
///
/// `bytes`（上游是 `Uint8Array`）、`bigint`（TypeBox 的 `Type.BigInt`）和 `undefined`
/// （TypeBox 的 `Type.Undefined`）在上游扩展了 JSON Schema，用于不经过 JSON 的值；本次移植的值
/// 固定为 `serde_json::Value`，因此这三项对任何值都不合格（见 `PORTING.md` 差异表）。
/// `number` 要求有限，与 TypeBox 一致。
pub fn type_checks() -> &'static [(&'static str, Check)] {
    TYPE_CHECKS.as_slice()
}

static TYPE_CHECKS: LazyLock<Vec<(&'static str, Check)>> = LazyLock::new(|| {
    vec![
        ("string", static_check(is_string)),
        ("number", static_check(is_number)),
        ("integer", static_check(is_integer)),
        ("boolean", static_check(is_boolean)),
        ("null", static_check(is_null)),
        ("array", static_check(is_array)),
        ("object", static_check(is_object)),
        ("bytes", static_check(is_bytes)),
        ("bigint", static_check(is_bigint)),
        ("undefined", static_check(is_undefined)),
    ]
});

/// 一个 `type` 名的检查器。
pub fn type_check(name: &str) -> Option<Check> {
    TYPE_CHECKS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, check)| check.clone())
}

/// 这个检查器是不是某个裸类型（`type` 名）的检查器：`anyOf` 把这类分支合成一次类型测试。
pub fn bare_type_of(check: &Check) -> Option<&'static str> {
    TYPE_CHECKS
        .iter()
        .find(|(_, c)| Arc::ptr_eq(c, check))
        .map(|(name, _)| *name)
}

const TYPE_BITS: &[(&str, u64)] = &[
    ("string", 1),
    ("number", 2),
    ("integer", 4),
    ("boolean", 8),
    ("null", 16),
    ("array", 32),
    ("object", 64),
    ("bytes", 128),
    ("bigint", 256),
    ("undefined", 512),
];

/// 这些类型名的位；不认识的类型名不置位。
pub fn type_mask_of_names(names: &[&str]) -> u64 {
    let mut mask = 0;
    for name in names {
        mask |= TYPE_BITS
            .iter()
            .find(|(n, _)| n == name)
            .map_or(0, |(_, bit)| *bit);
    }
    mask
}

/// 这些类型的位；不认识的类型名不置位。
pub fn type_mask(types: &[&Value]) -> u64 {
    let names: Vec<&str> = types.iter().filter_map(|t| t.as_str()).collect();
    type_mask_of_names(&names)
}

/// `v` 的类型位：整数同时有 `number` 和 `integer` 两位；NaN 和 Infinity 一位都没有。
fn type_bits(v: &Value) -> u64 {
    match v {
        Value::String(_) => 1,
        Value::Number(n) => {
            let Some(x) = n.as_f64() else {
                return 0;
            };
            if is_integer_number(x) {
                6
            } else if x.is_finite() {
                2
            } else {
                0
            }
        }
        Value::Bool(_) => 8,
        Value::Null => 16,
        Value::Array(_) => 32,
        Value::Object(_) => 64,
    }
}

/// 上游 `bytesCheck`：`Uint8Array` 且长度在界内。本次移植被检查的值固定是 JSON，字节串无法出现，
/// 因此等价于“任何值都不合格”（见 `PORTING.md` 差异表）。
pub fn bytes_check(_min: f64, _max: f64) -> Check {
    never()
}

fn mask_check(mask: u64) -> Check {
    Arc::new(move |v: &Value| (type_bits(v) & mask) != 0)
}

/// 指名类型之一。
pub fn type_check_of(types: &[&Value]) -> Check {
    if types.len() == 1 {
        return types[0].as_str().and_then(type_check).unwrap_or_else(never);
    }
    mask_check(type_mask(types))
}

/// `anyOf` 里那些裸类型分支由一次类型测试负责，其余分支各自跑自己的检查。
pub fn masked_any_check(mask: u64, checks: Vec<Check>) -> Check {
    if checks.is_empty() {
        return mask_check(mask);
    }
    let rest = if checks.len() == 1 {
        checks.into_iter().next().expect("one check")
    } else {
        any_check(checks)
    };
    Arc::new(move |v: &Value| (type_bits(v) & mask) != 0 || rest(v))
}

// ---------------------------------------------------------------- 组合

/// 一个对象或数组检查是否连类型一起查：不查、严格查，或放行其它类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    None,
    Strict,
    Loose,
}

/// `type: "object"` 的检查器。
pub fn object_type_check() -> Check {
    type_check("object").expect("object type check")
}

/// `type: "array"` 的检查器。
pub fn array_type_check() -> Check {
    type_check("array").expect("array type check")
}

/// `parts` 全部成立。一个和两个的部分给不带循环的闭包。
pub fn and(parts: Vec<Check>) -> Check {
    match parts.len() {
        0 => always(),
        1 => parts.into_iter().next().expect("one part"),
        _ => Arc::new(move |v: &Value| parts.iter().all(|part| part(v))),
    }
}

// ---------------------------------------------------------------- 递归引用

/// 递归引用穿过的那层间接的槽：目标建好之前先放一个占位检查，建好后把目标装进去。
///
/// 上游的槽直接持有检查器，环由 JS 的垃圾回收顺着引用走一遍收掉；`Arc` 的环永远收不掉，所以这里只
/// 装弱引用——目标由根检查器一路强引用着，根还在，槽就总能取到（见 `PORTING.md` 差异表）。
pub struct Slot<T: ?Sized>(OnceLock<Weak<T>>);

impl<T: ?Sized> Slot<T> {
    pub fn new() -> Arc<Self> {
        Arc::new(Self(OnceLock::new()))
    }

    /// 目标建好后装上。
    pub fn set(&self, check: &Arc<T>) {
        let _ = self.0.set(Arc::downgrade(check));
    }

    /// 装上的目标；目标已经没了就没有。
    pub(crate) fn get(&self) -> Option<Arc<T>> {
        self.0.get().and_then(Weak::upgrade)
    }
}

/// 一个普通检查的间接槽。
pub type CheckSlot = Slot<dyn Fn(&Value) -> bool + Send + Sync>;

type RefFrame = (usize, usize);

thread_local! {
    /// 正在跑的引用间接调用：`(间接槽地址, 被检查值的地址)`。同一对再进来一次就是环。
    static REFS_IN_FLIGHT: RefCell<Vec<RefFrame>> = const { RefCell::new(Vec::new()) };
}

/// 一次间接调用的记录，析构时把这一层摘掉。
pub(crate) struct RefInFlight;

impl Drop for RefInFlight {
    fn drop(&mut self) {
        REFS_IN_FLIGHT.with(|in_flight| {
            in_flight.borrow_mut().pop();
        });
    }
}

/// 记下这次间接调用；同一个槽拿同一个值已经在栈上时返回 `None`。
///
/// 那是检查器的环：这个 schema 位置在同一个值上又要算自己一次，它永远不会返回。上游此时由 JS 的
/// 调用栈抛出 `RangeError`；Rust 里同样的无限递归会把进程 abort，所以到这里就停（见差异表）。
pub(crate) fn enter_ref(slot: usize, value: &Value) -> Option<RefInFlight> {
    let value = value as *const Value as usize;
    REFS_IN_FLIGHT.with(|in_flight| {
        let mut in_flight = in_flight.borrow_mut();
        if in_flight.contains(&(slot, value)) {
            return None;
        }
        in_flight.push((slot, value));
        Some(RefInFlight)
    })
}

/// 实现稍后才装上的检查：递归引用走的那层间接。
pub fn indirect(slot: Arc<CheckSlot>) -> Check {
    let id = Arc::as_ptr(&slot) as *const () as usize;
    Arc::new(move |v: &Value| {
        let Some(check) = slot.get() else {
            return false;
        };
        let Some(_in_flight) = enter_ref(id, v) else {
            return false;
        };
        check(v)
    })
}

// ---------------------------------------------------------------- 动态作用域
//
// 上游把 `scope: string[]` 放在编译器里，由闭包按引用共享。Rust 侧检查器要 `Send + Sync` 且
// 能跨任务共用，而一次检查永远在同一个线程上跑完，所以作用域栈放线程本地（见 PORTING.md）。

thread_local! {
    static DYNAMIC_SCOPE: RefCell<Vec<Arc<str>>> = const { RefCell::new(Vec::new()) };
}

struct ScopeGuard;

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        DYNAMIC_SCOPE.with(|scope| {
            scope.borrow_mut().pop();
        });
    }
}

/// 把 `uri` 压到当前线程的动态作用域上，守卫析构时弹掉。
pub(crate) fn enter_scope(uri: Arc<str>) -> impl Drop {
    DYNAMIC_SCOPE.with(|scope| scope.borrow_mut().push(uri));
    ScopeGuard
}

/// 当前线程的动态作用域，由内向外（栈顶在末尾）。
pub(crate) fn scope_snapshot() -> Vec<Arc<str>> {
    DYNAMIC_SCOPE.with(|scope| scope.borrow().clone())
}

/// `check` 运行期间把 `uri` 压到动态作用域上（只有带 `$dynamicRef` 的 schema 会用到）。
pub fn scoped_check(uri: Arc<str>, check: Check) -> Check {
    Arc::new(move |v: &Value| {
        let _guard = enter_scope(uri.clone());
        check(v)
    })
}

// ---------------------------------------------------------------- 字符串

/// `s` 的 Unicode 码点数：JSON Schema 的字符串长度数码点，不是 UTF-16 单元。
pub fn code_points(s: &str) -> usize {
    s.chars().count()
}

/// 码点数在界内。上游用 UTF-16 单元数给码点数封上下界来省掉计数，结论相同，Rust 侧直接数码点。
pub fn length_ok(s: &str, min: f64, max: f64) -> bool {
    let points = if s.is_ascii() {
        s.len()
    } else {
        code_points(s)
    } as f64;
    points >= min && points <= max
}

fn regex_test(re: &Regex, value: &str) -> bool {
    re.find(value).is_some()
}

/// 字符串关键字：长度界、pattern 和 format。
pub fn string_check(
    strict: bool,
    min: f64,
    max: f64,
    re: Option<Arc<Regex>>,
    format: Option<FormatCheck>,
) -> Check {
    if min == 0.0 && max == f64::INFINITY && format.is_none() {
        if let Some(re) = re {
            return if strict {
                Arc::new(move |v: &Value| v.as_str().is_some_and(|s| regex_test(&re, s)))
            } else {
                Arc::new(move |v: &Value| v.as_str().is_none_or(|s| regex_test(&re, s)))
            };
        }
    }
    if re.is_none() && format.is_none() {
        return if strict {
            Arc::new(move |v: &Value| v.as_str().is_some_and(|s| length_ok(s, min, max)))
        } else {
            Arc::new(move |v: &Value| v.as_str().is_none_or(|s| length_ok(s, min, max)))
        };
    }
    Arc::new(move |v: &Value| {
        let Some(s) = v.as_str() else {
            return !strict;
        };
        length_ok(s, min, max)
            && re.as_ref().is_none_or(|re| regex_test(re, s))
            && format.as_ref().is_none_or(|format| format(s))
    })
}

// ---------------------------------------------------------------- 数字

/// 数字关键字查哪种值：0 放行其它类型，1 要求 number，2 要求 integer，3 要求 bigint。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberMode {
    Any,
    Number,
    Integer,
    Bigint,
}

/// 数字关键字，对 number 和 bigint 一视同仁（与 TypeBox 相同）。
///
/// 上游的 bigint 分支在本次移植里对任何 JSON 值都不合格：值是 JSON，bigint 无法出现。
pub fn number_check(
    mode: NumberMode,
    min: f64,
    max: f64,
    xmin: f64,
    xmax: f64,
    multiple_of: Option<f64>,
) -> Check {
    Arc::new(move |v: &Value| {
        let Some(x) = v.as_f64() else {
            return mode == NumberMode::Any;
        };
        if mode == NumberMode::Bigint {
            return false;
        }
        if mode != NumberMode::Any && !x.is_finite() {
            return false;
        }
        if mode == NumberMode::Integer && !is_integer_number(x) {
            return false;
        }
        x >= min
            && x <= max
            && x > xmin
            && x < xmax
            && multiple_of.is_none_or(|d| is_multiple(x, d))
    })
}

/// `v` 是不是 `d` 的倍数，小数也按十进制精确算：0.0075 / 0.0001 在浮点里是 74.99999999999999。
///
/// 上游用 `BigInt` 做精确整数运算；Rust 侧用十进制数字串做长除法（见 `PORTING.md` 差异表）。
///
/// 有意与 TypeBox 不同，它在那里会抛错（`BigInt(0.5)`、除零）：检查要对任何值回答，从不抛错，
/// 而真实 schema 也不会拿 bigint 配一个分数或零 `multipleOf`。
pub fn is_multiple(v: f64, d: f64) -> bool {
    let q = v / d;
    if q.is_finite() && is_integer_number(q) {
        return true;
    }
    if !q.is_finite() {
        return v % d == 0.0;
    }
    let (v_digits, v_exp) = decimal(v);
    let (d_digits, d_exp) = decimal(d);
    if v_exp >= d_exp {
        divides(&with_zeros(&v_digits, (v_exp - d_exp) as usize), &d_digits)
    } else {
        divides(&v_digits, &with_zeros(&d_digits, (d_exp - v_exp) as usize))
    }
}

fn with_zeros(digits: &[u8], zeros: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(digits.len() + zeros);
    out.extend_from_slice(digits);
    out.extend(std::iter::repeat_n(0, zeros));
    out
}

/// `x` 的最短往返十进制表示：`(数字串, 10 的指数)`，例如 `0.0075` → `("75", -4)`。
///
/// 对应上游 `Math.abs(x).toString()` 拆出的 `(BigInt(尾数), 指数)`：Rust 的 `{:e}` 同样给最短
/// 往返位数，只是总带指数，拆出来换算成同一个十进制值。
fn decimal(x: f64) -> (Vec<u8>, i32) {
    let scientific = format!("{:e}", x.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let (whole, fraction) = match mantissa.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (mantissa, ""),
    };
    let mut digits: Vec<u8> = whole
        .bytes()
        .chain(fraction.bytes())
        .map(|b| b - b'0')
        .collect();
    // 上游 `BigInt("0012")` 就是 12：去掉前导零。
    let first_nonzero = digits.iter().position(|&d| d != 0);
    match first_nonzero {
        Some(index) => {
            digits.drain(..index);
        }
        None => digits = vec![0],
    }
    (digits, exponent - fraction.len() as i32)
}

/// `num` 是否整除 `den`（十进制数字串，`den` 非零）。
fn divides(num: &[u8], den: &[u8]) -> bool {
    if den.iter().all(|&d| d == 0) {
        return false;
    }
    // 长除法只留余数：余数始终小于 den，接上下一位（乘 10 再加）后最多减 9 次 den。
    let mut rem: Vec<u8> = Vec::new();
    for &digit in num {
        if !rem.is_empty() || digit != 0 {
            rem.push(digit);
        }
        while compare_digits(&rem, den) >= 0 {
            subtract_digits(&mut rem, den);
        }
    }
    rem.is_empty()
}

fn normalize(digits: &mut Vec<u8>) {
    let first_nonzero = digits.iter().position(|&d| d != 0);
    match first_nonzero {
        Some(index) => {
            digits.drain(..index);
        }
        None => digits.clear(),
    }
}

fn compare_digits(a: &[u8], b: &[u8]) -> i32 {
    if a.len() != b.len() {
        return if a.len() < b.len() { -1 } else { 1 };
    }
    for (x, y) in a.iter().zip(b.iter()) {
        if x != y {
            return if x < y { -1 } else { 1 };
        }
    }
    0
}

fn subtract_digits(a: &mut Vec<u8>, b: &[u8]) {
    let mut borrow = 0i32;
    let offset = a.len() - b.len();
    for i in (0..a.len()).rev() {
        let b_digit = if i >= offset { b[i - offset] as i32 } else { 0 };
        let mut value = a[i] as i32 - borrow - b_digit;
        if value < 0 {
            value += 10;
            borrow = 1;
        } else {
            borrow = 0;
        }
        a[i] = value as u8;
    }
    normalize(a);
}

// ---------------------------------------------------------------- 对象

/// 对象检查里一个声明过的属性。
#[derive(Clone)]
pub struct ObjectEntry {
    pub key: String,
    pub check: Check,
    pub required: bool,
}

fn entries_by_key(entries: &[ObjectEntry]) -> HashMap<String, (Check, bool)> {
    entries
        .iter()
        .map(|entry| (entry.key.clone(), (entry.check.clone(), entry.required)))
        .collect()
}

/// `inner` 只对对象成立；别的值除 `strict` 外都放行。
pub fn guard_object(inner: Check, strict: bool) -> Check {
    if strict {
        Arc::new(move |v: &Value| is_object(v) && inner(v))
    } else {
        Arc::new(move |v: &Value| !is_object(v) || inner(v))
    }
}

/// `inner` 只对数组成立；别的值除 `strict` 外都放行。
pub fn guard_array(inner: Check, strict: bool) -> Check {
    if strict {
        Arc::new(move |v: &Value| is_array(v) && inner(v))
    } else {
        Arc::new(move |v: &Value| !is_array(v) || inner(v))
    }
}

/// 有声明属性的对象：`entries` 每个属性一项，`closed` 拒绝任何别的键。
///
/// 上游按 `for...in` 走值自己的键（V8 从枚举缓存里读），在这里就是按对象自己的键走。
pub fn declared_object(
    entries: Vec<ObjectEntry>,
    required_count: usize,
    closed: bool,
    guard: Guard,
) -> Check {
    let by_key = entries_by_key(&entries);
    Arc::new(move |v: &Value| {
        if guard != Guard::None && !is_object(v) {
            return guard == Guard::Loose;
        }
        let Some(object) = v.as_object() else {
            return false;
        };
        let mut required = 0;
        for (key, value) in object {
            let Some((check, is_required)) = by_key.get(key.as_str()) else {
                if closed {
                    return false;
                }
                continue;
            };
            if !check(value) {
                return false;
            }
            if *is_required {
                required += 1;
            }
        }
        required == required_count
    })
}

/// 有声明属性的对象，另有没声明的 required 键、patternProperties 与 additionalProperties。
pub fn open_object(
    entries: Vec<ObjectEntry>,
    keys: Vec<String>,
    extra_required: Vec<String>,
    patterns: Vec<(Arc<Regex>, Check)>,
    additional: Option<Check>,
    guard: Guard,
) -> Check {
    let known: Option<std::collections::HashSet<String>> =
        if patterns.is_empty() && additional.is_none() {
            None
        } else {
            Some(keys.into_iter().collect())
        };
    Arc::new(move |v: &Value| {
        if guard != Guard::None && !is_object(v) {
            return guard == Guard::Loose;
        }
        let Some(object) = v.as_object() else {
            return false;
        };
        for entry in &entries {
            let Some(value) = object.get(&entry.key) else {
                // 缺席。上游另有“可选属性值是 undefined 也算缺席”的一条：JSON 里没有 undefined。
                if entry.required {
                    return false;
                }
                continue;
            };
            if !(entry.check)(value) {
                return false;
            }
        }
        for key in &extra_required {
            if !object.contains_key(key) {
                return false;
            }
        }
        let Some(known) = known.as_ref() else {
            return true;
        };
        for (key, value) in object {
            let mut matched = known.contains(key);
            for (re, check) in &patterns {
                if regex_test(re, key) {
                    matched = true;
                    if !check(value) {
                        return false;
                    }
                }
            }
            if !matched
                && additional
                    .as_ref()
                    .is_some_and(|additional| !additional(value))
            {
                return false;
            }
        }
        true
    })
}

/// 每个属性名都通过 `check`（`v` 是对象）。
pub fn property_names_check(check: Check) -> Check {
    Arc::new(move |v: &Value| {
        let Some(object) = v.as_object() else {
            return true;
        };
        object.keys().all(|key| check(&Value::String(key.clone())))
    })
}

/// `minProperties` 和 `maxProperties`（`v` 是对象）。
pub fn property_count_check(min: f64, max: f64) -> Check {
    Arc::new(move |v: &Value| {
        let Some(object) = v.as_object() else {
            return true;
        };
        let count = object.len() as f64;
        count >= min && count <= max
    })
}

/// `dependentRequired`（`v` 是对象）。
pub fn dependent_required_check(entries: Vec<(String, Vec<String>)>) -> Check {
    Arc::new(move |v: &Value| {
        let Some(object) = v.as_object() else {
            return true;
        };
        entries.iter().all(|(key, list)| {
            !object.contains_key(key) || list.iter().all(|required| object.contains_key(required))
        })
    })
}

/// `dependentSchemas`（`v` 是对象）。
pub fn dependent_schemas_check(entries: Vec<(String, Check)>) -> Check {
    Arc::new(move |v: &Value| {
        let Some(object) = v.as_object() else {
            return true;
        };
        entries
            .iter()
            .all(|(key, check)| !object.contains_key(key) || check(v))
    })
}

// ---------------------------------------------------------------- 数组

/// 常见数组：类型守卫、长度界、前缀项和其后的项，一个闭包搞定。
pub fn list_check(
    guard: Guard,
    min: f64,
    max: f64,
    prefix: Vec<Check>,
    rest: Option<Check>,
) -> Check {
    let prefix_len = prefix.len();
    Arc::new(move |v: &Value| {
        let Some(list) = v.as_array() else {
            return guard == Guard::Loose;
        };
        let length = list.len() as f64;
        if length < min || length > max {
            return false;
        }
        let common = list.len().min(prefix_len);
        for i in 0..common {
            if !(prefix[i])(&list[i]) {
                return false;
            }
        }
        match rest.as_ref() {
            None => true,
            Some(rest) => list[prefix_len.min(list.len())..]
                .iter()
                .all(|item| rest(item)),
        }
    })
}

/// 前缀项和其后的项（`v` 是数组）。
pub fn items_check(prefix: Vec<Check>, rest: Option<Check>) -> Check {
    list_check(Guard::None, 0.0, f64::INFINITY, prefix, rest)
}

/// `minItems` 和 `maxItems`（`v` 是数组）。
pub fn length_check(min: f64, max: f64) -> Check {
    Arc::new(move |v: &Value| {
        let Some(list) = v.as_array() else {
            return true;
        };
        let length = list.len() as f64;
        length >= min && length <= max
    })
}

/// `contains` 及其界（`v` 是数组）。
pub fn contains_check(check: Check, min: f64, max: f64) -> Check {
    Arc::new(move |v: &Value| {
        let Some(list) = v.as_array() else {
            return true;
        };
        let mut count = 0f64;
        for item in list {
            if check(item) {
                count += 1.0;
                if count > max {
                    return false;
                }
            }
        }
        count >= min
    })
}

/// `uniqueItems`（`v` 是数组）。
pub fn unique_check(v: &Value) -> bool {
    let Some(list) = v.as_array() else {
        return true;
    };
    for i in 1..list.len() {
        for j in 0..i {
            if deep_equal(&list[i], &list[j]) {
                return false;
            }
        }
    }
    true
}

// ---------------------------------------------------------------- 就地关键字

/// `enum`：逐个按 JSON 相等比较。
///
/// 上游对至多 8 个原始值用一遍扫描、更多原始值用 `Set`、对象和数组用深比较；结论与本实现相同。
pub fn enum_check(values: Vec<Value>) -> Check {
    Arc::new(move |v: &Value| values.iter().any(|value| deep_equal(value, v)))
}

/// `const`。
pub fn const_check(value: Value) -> Check {
    Arc::new(move |v: &Value| deep_equal(v, &value))
}

/// `anyOf`。
pub fn any_check(checks: Vec<Check>) -> Check {
    Arc::new(move |v: &Value| checks.iter().any(|check| check(v)))
}

/// `oneOf`。
pub fn one_check(checks: Vec<Check>) -> Check {
    Arc::new(move |v: &Value| {
        let mut count = 0;
        for check in &checks {
            if check(v) {
                count += 1;
                if count > 1 {
                    return false;
                }
            }
        }
        count == 1
    })
}

/// `not`。
pub fn not_check(check: Check) -> Check {
    Arc::new(move |v: &Value| !check(v))
}

/// `if`、`then` 和 `else`。
pub fn if_check(cond: Check, then: Check, otherwise: Check) -> Check {
    Arc::new(move |v: &Value| if cond(v) { then(v) } else { otherwise(v) })
}

/// 由某个键上的 `const` 标记的一族对象：标记挑出唯一可能成立的那个分支。
pub fn tagged_check(key: String, tags: Vec<Value>, checks: Vec<Check>) -> Check {
    Arc::new(move |v: &Value| {
        let Some(object) = v.as_object() else {
            return false;
        };
        let tag = object.get(&key);
        for (i, candidate) in tags.iter().enumerate() {
            if Some(candidate) == tag {
                return checks[i](v);
            }
        }
        false
    })
}

/// JSON 相等，`const`、`enum` 和 `uniqueItems` 就是这么比的：同样的原始值，数组逐项按序相等，
/// 对象键相同且值相等（键序无关）。
///
/// 数字按 JS 的 `===` 比：`1` 与 `1.0` 是同一个数；`serde_json` 会保留 `u64`/`f64` 的区别，
/// 所以这里显式按 `f64` 归一（见 `PORTING.md` 差异表）。
pub fn deep_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(x, y)| deep_equal(x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(key, value)| y.get(key).is_some_and(|other| deep_equal(value, other)))
        }
        _ => a == b,
    }
}
