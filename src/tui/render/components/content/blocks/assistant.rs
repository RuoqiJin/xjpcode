use super::reasoning;
use crate::tui::render::markdown::parse_markdown;
use crate::ui::ContentBlock;
use ratatui::text::Line;

/// 助手回答块。思考区显示状态机（核心规则，四象限）：
/// 1. 无主回复 + 折叠（流式思考中、纯工具段）：只显示一行 ▸ [思考]
/// 2. 无主回复 + 展开（用户点击）：▾ [思考] + 引用条正文
/// 3. 有主回复 + 折叠（默认）：思考完全隐藏，呈现极紧凑的正式回复
/// 4. 有主回复 + 展开（用户点击）：正式回复 + 块内空行 + ▾ [思考]（思考在主回复下方向下展开）
pub fn draw(block: &ContentBlock, content_lines: &mut Vec<Line<'static>>) {
    let has_reasoning = block
        .reasoning
        .as_ref()
        .is_some_and(|reasoning| !reasoning.trim().is_empty());
    let has_content = !block.content.trim().is_empty();

    if !block.content.is_empty() {
        content_lines.extend(parse_markdown(&block.content));
    }

    if has_reasoning {
        let reasoning_text = block.reasoning.as_deref().unwrap_or("");
        match (has_content, block.collapsed) {
            (false, true) => reasoning::draw_collapsed_title(content_lines),
            (false, false) => reasoning::draw_section(reasoning_text, content_lines),
            (true, true) => {}
            (true, false) => {
                content_lines.push(Line::from(""));
                reasoning::draw_section(reasoning_text, content_lines);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::titles;
    use crate::ui::BlockKind;

    #[test]
    fn reasoning_state_machine_quadrants() {
        let mut block1 = ContentBlock::new(
            1,
            BlockKind::AssistantAnswer,
            String::new(),
            String::new(),
            true,
        );
        block1.reasoning = Some("thinking...".to_string());
        let mut lines1 = Vec::new();
        draw(&block1, &mut lines1);
        assert_eq!(lines1.len(), 1);
        let first_line = lines1[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(first_line.contains("▸ "));
        assert!(first_line.contains(titles::REASONING_TITLE));

        let mut block2 = ContentBlock::new(
            2,
            BlockKind::AssistantAnswer,
            String::new(),
            String::new(),
            false,
        );
        block2.reasoning = Some("thinking...".to_string());
        let mut lines2 = Vec::new();
        draw(&block2, &mut lines2);
        assert!(lines2.len() > 1);
        let title_line2 = lines2[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(title_line2.contains("▾ "));
        assert!(title_line2.contains(titles::REASONING_TITLE));

        let mut block3 = ContentBlock::new(
            3,
            BlockKind::AssistantAnswer,
            String::new(),
            "Hello world".to_string(),
            true,
        );
        block3.reasoning = Some("thinking...".to_string());
        let mut lines3 = Vec::new();
        draw(&block3, &mut lines3);
        let joined3 = lines3
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!joined3.contains(titles::REASONING_TITLE));
        assert!(joined3.contains("Hello world"));

        let mut block4 = ContentBlock::new(
            4,
            BlockKind::AssistantAnswer,
            String::new(),
            "Hello world".to_string(),
            false,
        );
        block4.reasoning = Some("thinking...".to_string());
        let mut lines4 = Vec::new();
        draw(&block4, &mut lines4);
        let joined4 = lines4
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined4.contains("Hello world"));
        assert!(joined4.contains("▾ "));
        assert!(joined4.contains(titles::REASONING_TITLE));
    }
}
