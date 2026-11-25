/// TUI 工具调用块正文的纯文本段落格式（唯一出处）：
/// 标签行 + 内容行，不含任何 box-drawing，渲染层的引用条样式见
/// tui/render/components/content/blocks/collapsible.rs 的 prefix_quote_bar。
pub fn args_section(raw_args: &str) -> String {
    format!("参数\n{}", raw_args)
}

pub fn result_section(preview: &str) -> String {
    format!("结果\n{}", preview)
}

pub fn append_result_section(content: &mut String, result: &str) {
    content.push('\n');
    content.push_str(&result_section(result));
}
