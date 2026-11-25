use super::block::{BlockKind, ContentBlock};
use super::state::App;

impl App {
    pub fn add_block(
        &mut self,
        kind: BlockKind,
        title: String,
        content: String,
        collapsed: bool,
    ) -> usize {
        let id = self.next_block_id;
        self.next_block_id += 1;
        self.blocks
            .push(ContentBlock::new(id, kind, title, content, collapsed));
        id
    }

    pub fn add_system_notice(&mut self, title: impl Into<String>, msg: impl Into<String>) {
        self.add_block(BlockKind::SystemNotice, title.into(), msg.into(), false);
    }

    pub fn add_user_block(&mut self, prompt: impl Into<String>) -> usize {
        self.add_block(
            BlockKind::UserPrompt,
            "You".to_string(),
            prompt.into(),
            false,
        )
    }

    pub fn add_tool_call_block(&mut self, name: &str, call_id: &str, raw_args: &str) -> usize {
        let title = crate::format::titles::tool_call_title(name);
        let content = crate::format::tool_sections::args_section(raw_args);
        let id = self.next_block_id;
        self.next_block_id += 1;
        self.blocks.push(
            ContentBlock::new(id, BlockKind::ToolCall, title, content, false)
                .with_tool_name(name)
                .with_tool_call_id(call_id),
        );
        id
    }

    /// 把工具返回结果并入那个调用块（标题带上字符数与耗时，正文追加结果段）。
    pub fn merge_tool_result(
        &mut self,
        tool_call_id: &str,
        name: &str,
        result: &str,
        elapsed_secs: Option<f32>,
        is_error: bool,
    ) -> bool {
        let Some(block) = self.blocks.iter_mut().rev().find(|block| {
            block.kind == BlockKind::ToolCall && block.tool_call_id.as_deref() == Some(tool_call_id)
        }) else {
            return false;
        };
        block.title =
            crate::format::titles::tool_call_title_with_result(name, result.len(), elapsed_secs);
        let preview = crate::format::preview::collapse_lines(result);
        crate::format::tool_sections::append_result_section(&mut block.content, &preview);
        block.is_error = is_error;
        true
    }

    pub fn toggle_block_by_y(
        &mut self,
        mouse_y: u16,
        scroll_offset: u16,
    ) -> Option<(Option<String>, bool)> {
        let virtual_y = mouse_y.saturating_add(scroll_offset);
        for block in &mut self.blocks {
            if let Some((y_start, y_end)) = block.render_y_range {
                if virtual_y >= y_start && virtual_y <= y_end {
                    block.collapsed = !block.collapsed;
                    return Some((block.tool_call_id.clone(), block.collapsed));
                }
            }
        }
        None
    }

    pub fn last_assistant_block(&self) -> Option<&ContentBlock> {
        self.blocks
            .iter()
            .rev()
            .find(|block| block.kind == BlockKind::AssistantAnswer)
    }
}
