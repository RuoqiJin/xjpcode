use super::block::BlockKind;
use super::state::App;

/// 流式卡片归属规则：
/// 一轮回复里，思考（reasoning）与正式回答（content）同住一张 AssistantAnswer 卡，
/// 两类 delta 都续写进 `active_assistant_block_id` 指向的块，互不切断；
/// 唯一的切断点是工具段（调用/结果）——工具之后到来的下一轮思考/回答另起新卡。
impl App {
    pub fn cut_active_assistant_block(&mut self) {
        self.active_assistant_block_id = None;
    }

    pub fn ensure_assistant_block(&mut self) -> usize {
        if let Some(id) = self.active_assistant_block_id {
            if self.blocks.iter().any(|block| block.id == id) {
                return id;
            }
        }
        let id = self.add_block(
            BlockKind::AssistantAnswer,
            String::new(),
            String::new(),
            true,
        );
        self.active_assistant_block_id = Some(id);
        id
    }

    pub fn append_reasoning(&mut self, text: &str) {
        let id = self.ensure_assistant_block();
        if let Some(block) = self.blocks.iter_mut().find(|block| block.id == id) {
            match block.reasoning {
                Some(ref mut reasoning) => reasoning.push_str(text),
                None => block.reasoning = Some(text.to_string()),
            }
        }
    }

    pub fn append_content(&mut self, text: &str) {
        let id = self.ensure_assistant_block();
        if let Some(block) = self.blocks.iter_mut().find(|block| block.id == id) {
            block.content.push_str(text);
        }
    }
}
