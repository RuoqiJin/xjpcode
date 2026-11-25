#![allow(dead_code)]
//! 移植自上游 `packages/agent/test/utils/get-current-time.ts`。
//!
//! 上游用 `Date.toLocaleString("en-US", { dateStyle: "full", timeStyle: "long", timeZone })`；
//! Rust 标准库没有时区库与本地化，这里只给 UTC 的 ISO 时间，`timezone` 参数照样收、照样回一份
//! 时间（见 `PORTING.md` 差异表 #75 的测试辅助那条）。agent 自己的测试没有用它，搬过来是为了
//! 对照表完整、且能编译。

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use xjpcode_agent::AgentToolResult;
use xjpcode_ai::JsonObject;

use crate::tools::{js_text, text_block, TestTool};

/// 上游 `obj({ timezone: opt(str({ description })) }, { open: true })`。
pub fn get_current_time_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "timezone": {
                "type": "string",
                "description": "Optional timezone (e.g., 'America/New_York', 'Europe/London')"
            }
        }
    })
}

/// 现在的时间。上游返回本地化文字；这里给 `YYYY-MM-DDTHH:MM:SSZ` 与 Unix 毫秒。
pub fn get_current_time(timezone: Option<&str>) -> AgentToolResult {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0);
    let text = match timezone {
        Some(timezone) => format!("{timezone}: {}", iso_utc(timestamp)),
        None => iso_utc(timestamp),
    };
    AgentToolResult {
        content: vec![text_block(text)],
        details: Some(json!({ "utcTimestamp": timestamp })),
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

/// 上游 `getCurrentTimeTool`。
pub fn get_current_time_tool() -> TestTool {
    TestTool::new(
        "get_current_time",
        "Current Time",
        "Get the current date and time",
        get_current_time_schema(),
        Arc::new(|_id, params: &JsonObject, _signal, _update| {
            let timezone = params.get("timezone").map(js_text);
            Box::pin(async move { Ok(get_current_time(timezone.as_deref())) })
        }),
    )
}

/// Unix 毫秒 → `YYYY-MM-DDTHH:MM:SSZ`。
fn iso_utc(timestamp: u64) -> String {
    let seconds = (timestamp / 1000) as i64;
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3600,
        (second_of_day % 3600) / 60,
        second_of_day % 60
    )
}

/// Howard Hinnant 的 `civil_from_days`：把 1970-01-01 起的天数换成公历。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (year + i64::from(month <= 2), month, day)
}
