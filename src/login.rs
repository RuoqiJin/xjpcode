//! 界面里的登录：把 `Models::login` 的提问与通知接到 TUI 上。
//!
//! pi 的 `/login` 是登录对话框（`components/login-dialog.ts`）：向用户要钥匙、打开浏览器、
//! 等回调或让他贴回链接。这里把同样的流程跑在后台任务里，界面线程只负责显示与收一行输入。

use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};
use xjpcode_ai::auth::{
    AuthEvent, AuthInteraction, AuthPrompt, AuthType, Credential, ModelsError, ProviderAuthInteraction,
};
use xjpcode_ai::models::Models;
use xjpcode_ai::types::BoxFuture;
use xjpcode_ai::AbortSignal;

/// 登录途中交给界面的事件。
pub enum LoginEvent {
    Notify(AuthEvent),
    Prompt(AuthPrompt),
    Done(Result<Credential, ModelsError>),
}

/// 一次进行中的登录。
pub struct LoginHandle {
    pub provider_id: String,
    pub events: mpsc::UnboundedReceiver<LoginEvent>,
    answer: Arc<Mutex<Option<oneshot::Sender<String>>>>,
    task: tokio::task::JoinHandle<()>,
    finished: bool,
}

impl LoginHandle {
    /// 在后台跑 `Models::login`。`provider_id` 与 `auth_type` 来自 `/login` 的选择。
    pub fn start(
        handle: &tokio::runtime::Handle,
        models: Arc<Models>,
        provider_id: impl Into<String>,
        auth_type: AuthType,
    ) -> Self {
        let provider_id = provider_id.into();
        let (events_tx, events) = mpsc::unbounded_channel();
        let answer: Arc<Mutex<Option<oneshot::Sender<String>>>> = Arc::new(Mutex::new(None));
        let interaction = Arc::new(ChannelInteraction {
            signal: AbortSignal::never(),
            events: events_tx.clone(),
            answer: Arc::clone(&answer),
        });
        let provider = provider_id.clone();
        let task = handle.spawn(async move {
            let result = models.login(&provider, auth_type, interaction, None).await;
            let _ = events_tx.send(LoginEvent::Done(result));
        });
        Self {
            provider_id,
            events,
            answer,
            task,
            finished: false,
        }
    }

    /// 界面收到 `Prompt`、用户按了 Enter 之后把这一行交回去。
    pub fn answer(&self, value: String) {
        if let Some(sender) = self.answer.lock().expect("login answer").take() {
            let _ = sender.send(value);
        }
    }

    /// 取消这次登录（丢掉正在跑的流程）。
    pub fn cancel(&self) {
        self.task.abort();
    }

    pub fn is_finished(&self) -> bool {
        self.finished || self.task.is_finished()
    }

    pub fn mark_finished(&mut self) {
        self.finished = true;
    }
}

impl Drop for LoginHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct ChannelInteraction {
    signal: AbortSignal,
    events: mpsc::UnboundedSender<LoginEvent>,
    answer: Arc<Mutex<Option<oneshot::Sender<String>>>>,
}

impl AuthInteraction for ChannelInteraction {
    fn signal(&self) -> Option<AbortSignal> {
        Some(self.signal.clone())
    }

    fn prompt<'a>(
        &'a self,
        prompt: AuthPrompt,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'a, Result<String, ModelsError>> {
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            *self.answer.lock().expect("login answer") = Some(tx);
            let _ = self.events.send(LoginEvent::Prompt(prompt));
            let prompt_signal = signal.unwrap_or_else(AbortSignal::never);
            let result: Result<String, ModelsError> = tokio::select! {
                value = rx => value.map_err(|_| ModelsError::login("Login cancelled")),
                _ = prompt_signal.cancelled() => Err(ModelsError::login("Login cancelled")),
                _ = self.signal.cancelled() => Err(ModelsError::login("Login cancelled")),
            };
            *self.answer.lock().expect("login answer") = None;
            result
        })
    }

    fn notify(&self, event: AuthEvent) {
        let _ = self.events.send(LoginEvent::Notify(event));
    }
}

impl ProviderAuthInteraction for ChannelInteraction {
    fn provider_signal(&self) -> AbortSignal {
        self.signal.clone()
    }
}
