//! 对应上游 `packages/schema/src/formats/typebox/idna/format/puny.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。
//!
//! PunyCode（RFC 3492）：解码把 A-label 的载荷变回 Unicode，编码算一个 U-label 真正的 A-label
//! 长度（RFC 5891 §4.2.4 的 63 字节限制针对编码后的形状，不是码点数）。
//!
//! 上游出错时 `throw null`（只在内部用 try/catch 接住）；Rust 侧用 `Result<_, PunycodeError>`。

use super::super::pattern::pattern::RE_NON_ASCII;

const PUNYCODE_BASE: u64 = 36;
const PUNYCODE_TMIN: u64 = 1;
const PUNYCODE_TMAX: u64 = 26;
const PUNYCODE_SKEW: u64 = 38;
const PUNYCODE_DAMP: u64 = 700;
const PUNYCODE_INITIAL_BIAS: u64 = 72;
const PUNYCODE_INITIAL_N: u64 = 128;

/// Punycode 载荷坏了（上游 `ThrowDontCare` 抛出的那个）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PunycodeError;

/// 值是不是带 `xn--` 前缀（大小写不敏感）。
pub fn is_ace_prefixed(value: &str) -> bool {
    value.to_lowercase().starts_with("xn--")
}

fn adapt(mut delta: u64, num_points: u64, first_time: bool) -> u64 {
    delta = if first_time {
        delta / PUNYCODE_DAMP
    } else {
        delta >> 1
    };
    delta += delta / num_points;
    let mut k = 0;
    while delta > ((PUNYCODE_BASE - PUNYCODE_TMIN) * PUNYCODE_TMAX) >> 1 {
        delta /= PUNYCODE_BASE - PUNYCODE_TMIN;
        k += PUNYCODE_BASE;
    }
    k + ((PUNYCODE_BASE - PUNYCODE_TMIN + 1) * delta) / (delta + PUNYCODE_SKEW)
}

/// 解码一个 Punycode 载荷（不含 `xn--`）。
pub fn decode(value: &str) -> Result<String, PunycodeError> {
    let mut output: Vec<u32> = Vec::new();
    let mut n = PUNYCODE_INITIAL_N;
    let mut i: u64 = 0;
    let mut bias = PUNYCODE_INITIAL_BIAS;
    let bytes = value.as_bytes();
    let delimiter = value.rfind('-');
    if let Some(delimiter) = delimiter.filter(|index| *index > 0) {
        for &byte in &bytes[..delimiter] {
            if byte >= 128 {
                return Err(PunycodeError); // 分隔符前不是基本字符
            }
            output.push(byte as u32);
        }
    }
    let mut index = match delimiter {
        Some(delimiter) => delimiter + 1,
        None => 0,
    };
    while index < bytes.len() {
        let old_i = i;
        let mut w: u64 = 1;
        let mut k = PUNYCODE_BASE;
        loop {
            if index >= bytes.len() {
                return Err(PunycodeError); // 输入意外结束
            }
            let ch = bytes[index];
            index += 1;
            let digit: u64 = match ch {
                b'a'..=b'z' => (ch - b'a') as u64,
                b'0'..=b'9' => (ch - b'0') as u64 + 26,
                _ => return Err(PunycodeError), // 坏的数码字符
            };
            i = i
                .checked_add(digit.checked_mul(w).ok_or(PunycodeError)?)
                .ok_or(PunycodeError)?;
            let t = if k <= bias {
                PUNYCODE_TMIN
            } else if k >= bias + PUNYCODE_TMAX {
                PUNYCODE_TMAX
            } else {
                k - bias
            };
            if digit < t {
                break;
            }
            w = w.checked_mul(PUNYCODE_BASE - t).ok_or(PunycodeError)?;
            k += PUNYCODE_BASE;
        }
        let out_len = output.len() as u64 + 1;
        bias = adapt(i - old_i, out_len, old_i == 0);
        n = n.checked_add(i / out_len).ok_or(PunycodeError)?;
        i %= out_len;
        let code_point = u32::try_from(n).map_err(|_| PunycodeError)?;
        output.insert(i as usize, code_point);
        i += 1;
    }
    let mut decoded = String::new();
    for code_point in output {
        decoded.push(char::from_u32(code_point).ok_or(PunycodeError)?);
    }
    Ok(decoded)
}

fn digit_to_char(digit: u64) -> char {
    if digit < 26 {
        char::from_u32(digit as u32 + 0x61).expect("ascii")
    } else {
        char::from_u32(digit as u32 - 26 + 0x30).expect("ascii")
    }
}

/// 编码一个 Unicode 串，返回 `xn--` 后面的载荷。
pub fn encode(input: &str) -> String {
    // 取出 ASCII 基本字符作为初始输出，其余码点留到最后追加。
    let basic: String = input.chars().filter(|ch| ch.is_ascii()).collect();
    let basic_length = basic.chars().count() as u64;
    let mut result: Vec<char> = Vec::new();
    if basic_length > 0 {
        result.extend(basic.chars());
        result.push('-');
    }
    let code_points: Vec<u64> = input.chars().map(|ch| ch as u64).collect();
    let mut n = PUNYCODE_INITIAL_N;
    let mut delta: u64 = 0;
    let mut bias = PUNYCODE_INITIAL_BIAS;
    let mut handled = basic_length;
    while handled < code_points.len() as u64 {
        let mut m = u64::MAX;
        for &code_point in &code_points {
            if code_point >= n && code_point < m {
                m = code_point;
            }
        }
        delta += (m - n) * (handled + 1);
        n = m;
        for &code_point in &code_points {
            if code_point < n {
                delta += 1;
            }
            if code_point == n {
                let mut q = delta;
                let mut k = PUNYCODE_BASE;
                loop {
                    let t = if k <= bias {
                        PUNYCODE_TMIN
                    } else if k >= bias + PUNYCODE_TMAX {
                        PUNYCODE_TMAX
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    result.push(digit_to_char(t + ((q - t) % (PUNYCODE_BASE - t))));
                    q = (q - t) / (PUNYCODE_BASE - t);
                    k += PUNYCODE_BASE;
                }
                result.push(digit_to_char(q));
                bias = adapt(delta, handled + 1, handled == basic_length);
                delta = 0;
                handled += 1;
            }
        }
        delta += 1;
        n += 1;
    }
    result.into_iter().collect()
}

/// 值里有没有非 ASCII 字符（上游 `RE_NON_ASCII.test`）。
pub fn has_non_ascii(value: &str) -> bool {
    RE_NON_ASCII.find(value).is_some()
}
