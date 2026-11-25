use super::fixtures::text;
use crate::tui::render::markdown::blocks::header::try_render_header;

/// 一到三级标题无 Emoji，纯文本渲染
#[test]
fn headers_are_clean_without_emojis() {
    for (raw, expected) in [
        ("# 一级", "一级"),
        ("## 二级", "二级"),
        ("### 三级", "三级"),
    ] {
        let line = try_render_header(raw).unwrap_or_else(|| panic!("{raw} 应识别为标题"));
        let rendered = text(&line);
        assert_eq!(rendered, expected, "输入 {raw}");
        assert!(!rendered.contains('📌'));
        assert!(!rendered.contains('🔹'));
    }
}

/// 井号后必须有空格,否则不是标题(#tag 之类不误判)
#[test]
fn header_requires_space_after_hashes() {
    assert!(try_render_header("#没有空格").is_none());
    assert!(try_render_header("##没有空格").is_none());
}

/// 当前只支持到三级,四级及以上按普通段落处理
#[test]
fn header_supports_three_levels_only() {
    assert!(try_render_header("#### 四级").is_none());
    assert!(try_render_header("##### 五级").is_none());
}

#[test]
fn plain_paragraph_is_not_a_header() {
    assert!(try_render_header("普通段落").is_none());
    assert!(try_render_header("").is_none());
}

/// 标题正文里的行内标记被解析,井号与标记符号都不留在输出里
#[test]
fn header_body_parses_inline_markup() {
    let line = try_render_header("## 带 **重点** 的标题").expect("应识别为标题");

    let rendered = text(&line);
    assert!(!rendered.contains('#'));
    assert!(!rendered.contains("**"));
    assert!(rendered.contains("带 重点 的标题"));
}

/// CJK 标题不因按字节切片而 panic
#[test]
fn cjk_header_does_not_panic() {
    let line = try_render_header("# 中文标题").expect("应识别为标题");

    assert!(text(&line).contains("中文标题"));
}
