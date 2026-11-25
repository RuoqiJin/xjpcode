use super::text_spacing::compact_text_spacing;

/// 将思考过程文本按中英文句号（`。` / `.`）与问号（`？` / `?`）智能拆分换行，
/// 并借助 text_spacing 压缩 CJK/英文混合字间距（A7 唯一出处）。
pub fn format_reasoning_content(content: &str) -> String {
    let mut formatted = String::with_capacity(content.len() + 32);

    for line in content.lines() {
        let trimmed_line = line.trim();
        if trimmed_line.is_empty() {
            continue;
        }

        let chars: Vec<char> = trimmed_line.chars().collect();
        let len = chars.len();
        let mut sentence_buf = String::new();
        let mut i = 0;

        while i < len {
            let c = chars[i];
            sentence_buf.push(c);

            let is_sentence_end = match c {
                '。' | '？' => true,
                '?' => true,
                '.' => {
                    let next_is_space_or_end = i + 1 == len || chars[i + 1].is_whitespace();
                    let is_list = is_numbered_list_item(&sentence_buf);
                    let is_abbreviation = is_abbreviation_prefix(&sentence_buf);

                    next_is_space_or_end && !is_list && !is_abbreviation
                }
                _ => false,
            };

            if is_sentence_end {
                let compact = compact_text_spacing(&sentence_buf);
                if !compact.is_empty() {
                    formatted.push_str(&compact);
                    formatted.push('\n');
                }
                sentence_buf.clear();

                while i + 1 < len && chars[i + 1].is_whitespace() {
                    i += 1;
                }
            }

            i += 1;
        }

        if !sentence_buf.is_empty() {
            let compact = compact_text_spacing(&sentence_buf);
            if !compact.is_empty() {
                formatted.push_str(&compact);
                formatted.push('\n');
            }
        }
    }
    formatted
}

fn is_numbered_list_item(buf: &str) -> bool {
    let trimmed = buf.trim();
    if !trimmed.ends_with('.') {
        return false;
    }
    let number_part = &trimmed[..trimmed.len() - 1];
    !number_part.is_empty() && number_part.chars().all(|c| c.is_ascii_digit())
}

fn is_abbreviation_prefix(buf: &str) -> bool {
    let trimmed = buf.trim();
    let lower = trimmed.to_lowercase();
    lower.ends_with("e.g.")
        || lower.ends_with("i.e.")
        || lower.ends_with("mr.")
        || lower.ends_with("mrs.")
        || lower.ends_with("dr.")
        || lower.ends_with("vs.")
        || lower.ends_with("etc.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_reasoning_content_splits_by_chinese_and_english_punctuation() {
        let input = "从 .zshrc 可以看到 $HOME/bin 被多次加入 PATH。 之前我们在 ~/bin 目录下看到了 xjp 等文件。";
        let output = format_reasoning_content(input);
        assert_eq!(
            output,
            "从.zshrc可以看到$HOME/bin被多次加入PATH。\n之前我们在~/bin目录下看到了xjp等文件。\n"
        );

        let english_input = "But I haven't found the actual xj executable. Let me check if there's an xj command? Let me check the ~/.local/bin directory.";
        let english_output = format_reasoning_content(english_input);
        assert_eq!(
            english_output,
            "But I haven't found the actual xj executable.\nLet me check if there's an xj command?\nLet me check the ~/.local/bin directory.\n"
        );
    }
}
