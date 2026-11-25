/// 人性化 Token / 字符计数（1024 进制唯一出处）：
/// < 1024 显示原数；>= 1024 显示 "Xk"；>= 1048576 显示 "XM"
pub fn humanize_count(n: u64) -> String {
    if n >= 1_048_576 {
        format!("{}M", n / 1_048_576)
    } else if n >= 1024 {
        format!("{}k", n / 1024)
    } else {
        format!("{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_humanize_count() {
        assert_eq!(humanize_count(500), "500");
        assert_eq!(humanize_count(1024), "1k");
        assert_eq!(humanize_count(283_115), "276k");
        assert_eq!(humanize_count(1_048_576), "1M");
    }
}
