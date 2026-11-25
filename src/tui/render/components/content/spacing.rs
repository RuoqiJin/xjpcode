use crate::ui::BlockKind;
use ratatui::text::Line;

/// 块间距策略：统一控制各 ContentBlock 渲染后的尾部行间距（Single Source of Truth）。
pub fn append_block_spacing(kind: &BlockKind, content_lines: &mut Vec<Line<'static>>) {
    if should_append_trailing_blank_line(kind) {
        content_lines.push(Line::from(""));
    }
}

fn should_append_trailing_blank_line(kind: &BlockKind) -> bool {
    matches!(kind, BlockKind::AssistantAnswer | BlockKind::SystemNotice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::render::components::content::blocks;
    use crate::ui::ContentBlock;

    fn draw_block(kind: BlockKind) -> Vec<Line<'static>> {
        let block = ContentBlock::new(
            0,
            kind.clone(),
            "title".to_string(),
            "content".to_string(),
            false,
        );
        let mut lines = Vec::new();
        match kind {
            BlockKind::UserPrompt => blocks::user::draw(&block, &mut lines),
            BlockKind::ToolCall => blocks::tool_call::draw(&block, &mut lines),
            BlockKind::AssistantAnswer => blocks::assistant::draw(&block, &mut lines),
            BlockKind::SystemNotice => blocks::system_notice::draw(&block, &mut lines),
            BlockKind::Divider => blocks::divider::draw(1, 80, &mut lines),
        }
        append_block_spacing(&kind, &mut lines);
        lines
    }

    fn trailing_blank_count(lines: &[Line<'static>]) -> usize {
        lines
            .iter()
            .rev()
            .take_while(|line| line.width() == 0)
            .count()
    }

    #[test]
    fn trailing_blank_line_matches_policy() {
        use BlockKind::*;
        let cases = [
            (UserPrompt, 0),
            (ToolCall, 0),
            (AssistantAnswer, 1),
            (SystemNotice, 1),
            (Divider, 0),
        ];
        for (kind, expected) in cases {
            let lines = draw_block(kind.clone());
            assert_eq!(
                trailing_blank_count(&lines),
                expected,
                "kind={kind:?} produced wrong trailing blank line count"
            );
        }
    }
}
