/// 默认闭合底边框字符串（唯一的 3 槽收口出处）
pub const DEFAULT_BOX_CLOSE_BAR: &str = "└───";

/// 产生指定划线长度的闭合底边框（用于流式/多尺寸控制）
pub fn make_box_close_bar(dashes: usize) -> String {
    let mut bar = String::with_capacity(3 + dashes * 3);
    bar.push('└');
    for _ in 0..dashes {
        bar.push('─');
    }
    bar
}

/// Reasoning 流式块收尾关门线（CLI 增量流式渲染唯一出处）
pub fn reasoning_close_bar() -> String {
    make_box_close_bar(40)
}

/// 统一的 box-drawing 文本块格式（┌── header / │ body / └───），全项目唯一出处
pub fn boxed_block(header: &str, body: &str) -> String {
    format!(
        "┌── {}\n│ {}\n{}",
        header,
        body.replace('\n', "\n│ "),
        DEFAULT_BOX_CLOSE_BAR
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_make_box_close_bar() {
        assert_eq!(make_box_close_bar(3), "└───");
        assert_eq!(make_box_close_bar(5), "└─────");
    }
}
