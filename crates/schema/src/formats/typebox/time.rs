//! 对应上游 `packages/schema/src/formats/typebox/time.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use std::sync::LazyLock;

use regress::Regex;

static TIME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::with_flags(
        r"^(\d\d):(\d\d):(\d\d)(?:\.\d+)?(?:([Zz])|([+-])(\d\d):(\d\d))?$",
        "",
    )
    .expect("time pattern")
});

/// 值是不是一个 ISO 时间串。
///
/// https://datatracker.ietf.org/doc/html/rfc3339
pub fn is_time(value: &str) -> bool {
    is_time_with(value, true)
}

/// 同上；`strict_time_zone` 为假时允许省略时区。
pub fn is_time_with(value: &str, strict_time_zone: bool) -> bool {
    let Some(matched) = TIME.find(value) else {
        return false;
    };
    let group = |index: usize| -> Option<&str> { matched.group(index).map(|range| &value[range]) };
    let number = |index: usize| -> i32 {
        group(index)
            .map(|text| text.parse().unwrap_or(0))
            .unwrap_or(0)
    };
    let has_zulu = group(4).is_some();
    let has_offset = group(5).is_some();
    // strictTimeZone 为真时，时区偏移或 Z/z 必须给出来。
    if strict_time_zone && !has_zulu && !has_offset {
        return false;
    }
    let hour = number(1);
    let minute = number(2);
    let second = number(3);
    if hour > 23 || minute > 59 || second > 60 {
        return false;
    }
    if has_offset {
        let tz_hour = number(6);
        let tz_minute = number(7);
        if tz_hour > 23 || tz_minute > 59 {
            return false;
        }
    }
    if second < 60 {
        return true;
    }
    // 闰秒处理：必须归一到 23:59:60 UTC（UTC 分钟数 1439）。
    let sign = if group(5) == Some("-") { -1 } else { 1 };
    let tz_hour = number(6);
    let tz_minute = number(7);
    let total_utc_minutes = hour * 60 + minute - sign * (tz_hour * 60 + tz_minute);
    ((total_utc_minutes % 1440) + 1440) % 1440 == 1439
}
