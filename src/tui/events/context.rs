use std::io::{Read, Seek, SeekFrom};
use std::time::Instant;

use tokio::sync::mpsc;
use xjpcode_coding_agent::core::agent_session::AgentSessionEvent;

use crate::session_setup::{OpenedSession, SessionChoice, SessionSetup};
use crate::ui::{session_info, App};

use super::io::{IoContext, Live};

pub struct EventContext {
    pub io: IoContext,
    pub rx: mpsc::UnboundedReceiver<AgentSessionEvent>,
    pub total_rendered_lines: u16,
    pub needs_redraw: bool,
    /// 上次实际重绘时刻：engine 的 60FPS 渲染闸门据此合帧。
    pub last_draw: Option<Instant>,
    /// 切回来时先清一次屏（窗口大小变了、或者终端被别的程序擦过）。
    pub force_clear: bool,
}

fn history_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|home| home.join(".xjpcode").join("history"))
}

fn load_history(app: &mut App) {
    let Some(path) = history_path() else {
        return;
    };
    app.history_path = Some(path.clone());
    let Ok(metadata) = std::fs::metadata(&path) else {
        return;
    };
    if !metadata.is_file() {
        return;
    }
    let max = crate::limits::INPUT_HISTORY_BYTES_DEFAULT;
    let truncated = metadata.len() > max as u64;
    let Ok(mut file) = std::fs::File::open(&path) else {
        return;
    };
    if truncated
        && file
            .seek(SeekFrom::End(-(max.min(i64::MAX as usize) as i64)))
            .is_err()
    {
        return;
    }
    let mut content = String::with_capacity(max);
    if file.take(max as u64).read_to_string(&mut content).is_err() {
        return;
    }
    let content = if truncated {
        content
            .split_once('\n')
            .map(|(_, suffix)| suffix)
            .unwrap_or("")
    } else {
        &content
    };
    app.history = content
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    let limit = crate::limits::INPUT_HISTORY_ENTRIES_DEFAULT;
    if app.history.len() > limit {
        let drain = app.history.len() - limit;
        app.history.drain(..drain);
    }
}

impl EventContext {
    /// 事件循环里偶尔要等一句短结果（列模型、查登录、删登录）。
    /// 不能直接 `Handle::block_on`：那是在运行时里面再开一个运行时，会 panic。
    pub fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        tokio::task::block_in_place(|| futures::executor::block_on(future))
    }

    pub fn new(
        app: &mut App,
        setup: SessionSetup,
        opened: OpenedSession,
        handle: tokio::runtime::Handle,
    ) -> Self {
        let models = setup.models();
        let (live, rx) = Live::attach(opened.session.clone());
        session_info::sync_session(app, &opened.session);
        load_history(app);

        if opened.resumed {
            session_info::rebuild_for_session(app, &opened.session);
        }

        let io = IoContext {
            live,
            setup,
            models,
            handle,
            auto_scroll_bottom: true,
            prompt_task: None,
            compaction_task: None,
            login: None,
        };

        Self {
            io,
            rx,
            total_rendered_lines: 0,
            needs_redraw: true,
            last_draw: None,
            force_clear: false,
        }
    }

    /// 打开另一场会话（/new、/clear、/resume）。原来的会话与后台任务先收尾。
    pub fn open_session(&mut self, app: &mut App, choice: SessionChoice) -> Result<bool, String> {
        if self.io.prompt_task.is_some() {
            self.io.live.session.request_abort();
        }
        // 拿掉句柄只是不再等它；旧会话收到 abort 后会自己收尾（工具、子进程都算在里面）。
        self.io.prompt_task = None;
        if self.io.compaction_task.is_some() {
            self.io.live.session.abort_compaction();
        }
        self.io.compaction_task = None;
        self.io.login = None;
        app.clear_login();

        let opened = self.io.setup.open(&choice)?;
        let resumed = opened.resumed;
        let (live, rx) = Live::attach(opened.session.clone());
        self.io.live = live;
        self.rx = rx;
        self.io.models = self.io.setup.models();
        self.io.auto_scroll_bottom = true;
        self.needs_redraw = true;

        if resumed {
            session_info::rebuild_for_session(app, &opened.session);
        } else {
            session_info::reset_for_session(app, &opened.session);
        }
        Ok(resumed)
    }
}
