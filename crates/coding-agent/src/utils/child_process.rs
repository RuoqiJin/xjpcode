//! 对应上游 `src/utils/child-process.ts` 的 `waitForChildProcess`：进程退出后还要把两路管道读完。
//!
//! 上游在 `exit` 之后等管道安静 100 毫秒（每来一块数据就重新计时），这样后台子进程还在写的输出
//! 不会被截掉；一直安静的继承句柄也不会让调用方挂住。这里的做法相同，只是管道读取由 tokio 任务
//! 代劳。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Child;
use tokio::task::JoinHandle;

/// 上游 `EXIT_STDIO_GRACE_MS`
pub const EXIT_STDIO_GRACE_MS: u64 = 100;

/// 一路管道读完时的收尾句柄。
pub struct PipeReaders {
    tasks: Vec<JoinHandle<()>>,
    last_data_ms: Arc<AtomicU64>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// 把 stdout / stderr 各交给一个任务读，读到一块就交给 `on_data`。
pub fn spawn_pipe_readers(
    streams: Vec<Option<Box<dyn AsyncRead + Unpin + Send>>>,
    on_data: Arc<dyn Fn(Vec<u8>) + Send + Sync>,
) -> PipeReaders {
    let last_data_ms = Arc::new(AtomicU64::new(now_ms()));
    let mut tasks = Vec::new();
    for stream in streams.into_iter().flatten() {
        let last_data_ms = Arc::clone(&last_data_ms);
        let on_data = Arc::clone(&on_data);
        tasks.push(tokio::spawn(async move {
            let mut stream = stream;
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                match stream.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        last_data_ms.store(now_ms(), Ordering::SeqCst);
                        on_data(buffer[..read].to_vec());
                    }
                }
            }
        }));
    }
    PipeReaders { tasks, last_data_ms }
}

impl PipeReaders {
    /// 上游 `waitForChildProcess`：`exit` 之后等两路管道安静下来，最多 100 毫秒。
    pub async fn finish_after_exit(self) {
        loop {
            if self.tasks.iter().all(|task| task.is_finished()) {
                break;
            }
            let idle_ms = now_ms().saturating_sub(self.last_data_ms.load(Ordering::SeqCst));
            if idle_ms > EXIT_STDIO_GRACE_MS {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// pi: `waitForChildProcess`——等进程退出，再收完两路输出。
pub async fn wait_for_child_process(
    child: &mut Child,
    readers: PipeReaders,
) -> std::io::Result<std::process::ExitStatus> {
    let status = child.wait().await?;
    readers.finish_after_exit().await;
    Ok(status)
}
