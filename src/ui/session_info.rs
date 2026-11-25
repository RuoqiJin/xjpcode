use std::sync::Arc;

use xjpcode_coding_agent::core::agent_session::AgentSession;
use xjpcode_coding_agent::core::model_setup::thinking_level_name;

use super::state::App;

/// 把会话里的模型、思考档、编号、上下文用量同步到界面状态。
pub fn sync_session(app: &mut App, session: &Arc<AgentSession>) {
    let model = session.model();
    app.cwd = session.cwd().display().to_string();
    app.model_id = model.id.clone();
    app.model_name = model.name.clone();
    app.model_provider = model.provider.clone();
    app.context_window = model.context_window;
    app.thinking = thinking_level_name(session.thinking_level()).to_string();
    app.session_id = session.session_id();
    app.session_file = session.session_file();
    app.tool_count = session.agent().state().tools.len();
    if let Some((tokens, window)) = session.context_usage() {
        app.context_prompt_tokens = tokens.min(u32::MAX as u64) as u32;
        if window > 0 {
            app.context_window = window;
        }
    }
}

/// 新开一场：清空卡片与排队，保留输入历史与鼠标设置。
pub fn reset_for_session(app: &mut App, session: &Arc<AgentSession>) {
    app.reset_for_session();
    sync_session(app, session);
}

/// 接着聊：清空后按会话消息重画。
pub fn rebuild_for_session(app: &mut App, session: &Arc<AgentSession>) {
    app.reset_for_session();
    sync_session(app, session);
    super::hydrate::hydrate(app, &session.messages());
    app.add_system_notice(
        "会话已恢复",
        format!("已载入历史会话 [{}]", session.session_id()),
    );
}
