use std::sync::Arc;

use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use xjpcode_ai::models::Models;
use xjpcode_coding_agent::core::agent_session::{
    AgentSession, AgentSessionEvent, AgentSessionSubscription,
};
use xjpcode_coding_agent::core::model_setup::thinking_level_name;

use crate::login::LoginHandle;
use crate::session_setup::SessionSetup;
use crate::ui::App;

/// 当前显示的那一场会话；订阅句柄一放下，事件就停了。
pub struct Live {
    pub session: Arc<AgentSession>,
    _subscription: AgentSessionSubscription,
}

impl Live {
    pub fn attach(
        session: Arc<AgentSession>,
    ) -> (Self, mpsc::UnboundedReceiver<AgentSessionEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let subscription = session.subscribe(move |event| {
            let _ = tx.send(event.clone());
        });
        (
            Self {
                session,
                _subscription: subscription,
            },
            rx,
        )
    }
}

/// 事件处理链共享的 I/O 资源束：会话、模型登记、运行时、后台任务、登录流程。
pub struct IoContext {
    pub live: Live,
    pub setup: SessionSetup,
    pub models: Arc<Models>,
    pub handle: Handle,
    pub auto_scroll_bottom: bool,
    pub prompt_task: Option<JoinHandle<Result<(), String>>>,
    pub compaction_task: Option<JoinHandle<Result<(), String>>>,
    pub login: Option<LoginHandle>,
}

impl IoContext {
    pub fn session(&self) -> &Arc<AgentSession> {
        &self.live.session
    }

    pub fn spawn_prompt(&mut self, text: String, app: &mut App) {
        app.spinner.start(crate::ui::spinner::get_random_verb());
        self.auto_scroll_bottom = true;
        let session = Arc::clone(&self.live.session);
        self.prompt_task = Some(
            self.handle
                .spawn(async move { session.prompt(&text, None).await }),
        );
    }

    pub fn spawn_compaction(&mut self, app: &mut App) {
        if !app.spinner.is_processing {
            app.spinner.start("正在压缩上下文...");
        } else {
            app.spinner.set_verb("正在压缩上下文...");
        }
        let session = Arc::clone(&self.live.session);
        self.compaction_task = Some(
            self.handle
                .spawn(async move { session.compact(None).await.map(|_| ()) }),
        );
    }

    /// 已排队的插话收回输入框（排在前面，当前输入接在后面）。
    pub fn restore_queued(&self, app: &mut App) -> usize {
        let (steering, follow_up) = self.live.session.clear_queue();
        let mut queued = steering;
        queued.extend(follow_up);
        if queued.is_empty() {
            return 0;
        }
        let queued_text = queued.join("\n\n");
        let current = app.input.trim().to_string();
        app.input = if current.is_empty() {
            queued_text
        } else {
            format!("{queued_text}\n\n{}", app.input)
        };
        app.cursor_pos = app.input.len();
        queued.len()
    }

    pub fn abort_all_tasks(&mut self) {
        self.live.session.request_abort();
        if let Some(task) = self.prompt_task.take() {
            task.abort();
        }
        if let Some(task) = self.compaction_task.take() {
            task.abort();
        }
        if let Some(login) = &self.login {
            login.cancel();
        }
        self.login = None;
    }

    pub fn refresh_model(&self, app: &mut App) {
        let model = self.live.session.model();
        app.model_id = model.id.clone();
        app.model_name = model.name.clone();
        app.model_provider = model.provider.clone();
        app.context_window = model.context_window;
        app.thinking = thinking_level_name(self.live.session.thinking_level()).to_string();
        app.tool_count = self.live.session.agent().state().tools.len();
    }
}
