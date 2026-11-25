/// 「工具」标识文字（唯一出处）：`[工具]` 半角方括号风格。
pub const TOOL_BADGE: &str = "工具";

/// 思考过程卡的标题（唯一出处）。
pub const REASONING_TITLE: &str = "[思考]";

pub fn tool_call_title(name: &str) -> String {
    format!("[{TOOL_BADGE}] {}", name)
}

/// "{字符数}字符 · {耗时}" 统计后缀（唯一出处），工具调用标题靠它拼。
/// 当耗时不足 1ms 时，以微秒(μs)展示精确执行时间。
fn result_stats_suffix(char_count: usize, elapsed_secs: Option<f32>) -> String {
    match elapsed_secs {
        Some(secs) if secs < 1.0 => {
            let ms = (secs * 1000.0).round() as u64;
            if ms == 0 {
                let us = (secs * 1_000_000.0).round() as u64;
                format!("({}字符 · {}μs)", char_count, us)
            } else {
                format!("({}字符 · {}ms)", char_count, ms)
            }
        }
        Some(secs) => format!("({}字符 · {:.1}s)", char_count, secs),
        None => format!("({}字符)", char_count),
    }
}

/// TUI 里工具调用块拿到结果后，原地把统计信息并入同一个块的标题（唯一出处）。
pub fn tool_call_title_with_result(
    name: &str,
    char_count: usize,
    elapsed_secs: Option<f32>,
) -> String {
    format!(
        "[{TOOL_BADGE}] {} {}",
        name,
        result_stats_suffix(char_count, elapsed_secs)
    )
}

/// 剥离标题与用括号包裹的后缀统计文本（例如 `"[工具] read (15字符)"` -> `("[工具] read", " (15字符)")`）。
/// 括号统计后缀排版时降级为暗灰色。唯一出处。
pub fn split_title_and_suffix(title: &str) -> (&str, &str) {
    if let Some(index) = title.rfind(" (") {
        (&title[..index], &title[index..])
    } else {
        (title, "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_stats_suffix_formatting() {
        assert_eq!(result_stats_suffix(100, Some(0.00035)), "(100字符 · 350μs)");
        assert_eq!(result_stats_suffix(200, Some(0.001)), "(200字符 · 1ms)");
        assert_eq!(result_stats_suffix(300, Some(1.5)), "(300字符 · 1.5s)");
        assert_eq!(result_stats_suffix(400, None), "(400字符)");
    }

    #[test]
    fn split_title_and_suffix_keeps_the_parenthesis_with_the_suffix() {
        assert_eq!(
            split_title_and_suffix("[工具] read (15字符)"),
            ("[工具] read", " (15字符)")
        );
        assert_eq!(split_title_and_suffix("Plain Title"), ("Plain Title", ""));
    }

    #[test]
    fn tool_titles_use_the_chinese_badge() {
        assert_eq!(tool_call_title("read"), "[工具] read");
    }
}
