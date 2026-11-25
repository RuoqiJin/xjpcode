#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    UserPrompt,
    ToolCall,
    AssistantAnswer,
    SystemNotice,
    Divider,
}

#[derive(Debug, Clone)]
pub struct ContentBlock {
    pub id: usize,
    pub kind: BlockKind,
    pub title: String,
    pub content: String,
    pub reasoning: Option<String>,
    pub collapsed: bool,
    pub render_y_range: Option<(u16, u16)>,
    pub round: Option<usize>,
    pub tool_name: Option<String>,
    /// 工具调用编号：结果按它对上调用；划选复制也用它识别。
    pub tool_call_id: Option<String>,
    pub is_error: bool,
}

impl ContentBlock {
    pub fn new(
        id: usize,
        kind: BlockKind,
        title: String,
        content: String,
        collapsed: bool,
    ) -> Self {
        Self {
            id,
            kind,
            title,
            content,
            reasoning: None,
            collapsed,
            render_y_range: None,
            round: None,
            tool_name: None,
            tool_call_id: None,
            is_error: false,
        }
    }

    pub fn with_round(mut self, round: usize) -> Self {
        self.round = Some(round);
        self
    }

    pub fn with_tool_name(mut self, tool_name: impl Into<String>) -> Self {
        self.tool_name = Some(tool_name.into());
        self
    }

    pub fn with_tool_call_id(mut self, tool_call_id: impl Into<String>) -> Self {
        self.tool_call_id = Some(tool_call_id.into());
        self
    }

    pub fn with_error(mut self, is_error: bool) -> Self {
        self.is_error = is_error;
        self
    }
}
