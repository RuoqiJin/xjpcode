//! 对应上游 `packages/schema/src/formats/typebox/date_time.ts`。
//!
//! 改编自 TypeBox 1.3.27（MIT），许可证见 `THIRD_PARTY_NOTICES/typebox-LICENSE`。

use super::date::is_date;
use super::time::is_time;

/// 值是不是一个 ISO8601 日期时间串。
///
/// https://datatracker.ietf.org/doc/html/rfc3339 ；例 `2020-12-12T20:20:40+00:00`。
pub fn is_date_time(value: &str) -> bool {
    // 上游 `value.split(/T/i)`：大小写不敏感地按 T 切，必须恰好切成两段。
    let separators: Vec<usize> = value
        .char_indices()
        .filter(|(_, ch)| *ch == 'T' || *ch == 't')
        .map(|(index, _)| index)
        .collect();
    if separators.len() != 1 {
        return false;
    }
    let at = separators[0];
    is_date(&value[..at]) && is_time(&value[at + 1..])
}
