//! 字符与字间距纯文本格式化工具（唯一出处）。
//!
//! 拥有中英文/符号混排字间距压缩、多余连续空格合并以及 CJK 字符边界识别规则。

/// 检查字符是否属于 CJK（中日韩统一表意文字及 CJK 标点）
pub fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{4e00}'..='\u{9fff}' | '\u{3000}'..='\u{303f}' | '\u{ff00}'..='\u{ffef}'
    )
}

/// 规范并压缩字间距：
/// 1. 吞掉连续的多个空格；
/// 2. 剥离紧邻 CJK 字符两侧的冗余空格，使 Terminal 显示紧凑干练。
pub fn compact_text_spacing(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();

    let mut i = 0;
    while i < len {
        if chars[i] == ' ' {
            let prev_char = if i > 0 { Some(chars[i - 1]) } else { None };

            // 吞掉连续的多个空格
            while i + 1 < len && chars[i + 1] == ' ' {
                i += 1;
            }

            let next_char = if i + 1 < len {
                Some(chars[i + 1])
            } else {
                None
            };

            let prev_cjk = prev_char.map(is_cjk).unwrap_or(false);
            let next_cjk = next_char.map(is_cjk).unwrap_or(false);

            // 若空格紧邻 CJK 字符，予以剥离以达致紧凑排版
            if prev_cjk || next_cjk {
                i += 1;
                continue;
            }
        }
        result.push(chars[i]);
        i += 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compact_text_spacing_cjk_and_english() {
        assert_eq!(
            compact_text_spacing("你能找到  xj  这个程序的所在位置吗"),
            "你能找到xj这个程序的所在位置吗"
        );
        assert_eq!(
            compact_text_spacing("主目录下有  .xj  目录"),
            "主目录下有.xj目录"
        );
    }

    #[test]
    fn test_compact_text_spacing_preserves_pure_english_single_space() {
        assert_eq!(
            compact_text_spacing("cargo    test  --release"),
            "cargo test --release"
        );
    }
}
