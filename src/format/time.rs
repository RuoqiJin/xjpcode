//! 时间与日期格式化助手（唯一出处）

fn is_leap_year(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

/// 将 Unix 秒数时间戳格式化为 YYYYMMDD_HHMMSS (UTC) 字符串
pub fn format_timestamp_datetime(secs: u64) -> String {
    let total_secs = secs;
    let sec = (total_secs % 60) as u32;
    let total_mins = total_secs / 60;
    let min = (total_mins % 60) as u32;
    let total_hours = total_mins / 60;
    let hour = (total_hours % 24) as u32;
    let mut days = (total_hours / 24) as u32;

    let mut year = 1970u32;
    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let is_leap = is_leap_year(year);
    let days_in_months = [
        31,
        if is_leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];

    let mut month = 1u32;
    for &d in &days_in_months {
        if days < d {
            break;
        }
        days -= d;
        month += 1;
    }
    let day = days + 1;

    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        year, month, day, hour, min, sec
    )
}

/// 生产带年月日时分秒的 Session ID：`YYYYMMDD_HHMMSS_{nanos:04x}`
pub fn generate_timestamp_session_id(secs: u64, nanos: u32) -> String {
    let dt = format_timestamp_datetime(secs);
    format!("{}_{:04x}", dt, nanos % 0x10000)
}
