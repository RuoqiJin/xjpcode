/// 空闲/思考中的默认 spinner 动词（唯一出处）；`VERBS[0]` 必须等于它。
pub const DEFAULT_VERB: &str = "思考中...";

/// Spinner 随机动词池（规则归 app 层；render 层只持有帧画面 SPINNER_FRAMES）。
pub const VERBS: &[&str] = &[
    DEFAULT_VERB,
    "正在组织回答...",
    "正在查资料...",
    "正在读文件...",
    "正在跑命令...",
    "正在整理上下文...",
    "正在想办法...",
    "正在收尾...",
];

pub fn get_random_verb() -> &'static str {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as usize;
    VERBS[nanos % VERBS.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbs_first_is_default() {
        assert_eq!(VERBS[0], DEFAULT_VERB);
    }
}
