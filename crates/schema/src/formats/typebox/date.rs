//! 对应上游 `packages/schema/src/formats/typebox/date.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use std::sync::LazyLock;

use regress::Regex;

const DAYS: [u32; 13] = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

static DATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::with_flags(r"^(\d\d\d\d)-(\d\d)-(\d\d)$", "").expect("date pattern"));

fn is_leap_year(year: u32) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

/// 值是不是一个 ISO8601 日期串。
///
/// https://datatracker.ietf.org/doc/html/rfc3339 ；例 `2020-12-12`。
pub fn is_date(value: &str) -> bool {
    let Some(matched) = DATE.find(value) else {
        return false;
    };
    let group = |index: usize| -> u32 {
        matched
            .group(index)
            .map(|range| value[range].parse().unwrap_or(0))
            .unwrap_or(0)
    };
    let year = group(1);
    let month = group(2);
    let day = group(3);
    if !(1..=12).contains(&month) {
        return false;
    }
    let last = if month == 2 && is_leap_year(year) {
        29
    } else {
        DAYS[month as usize]
    };
    (1..=last).contains(&day)
}
