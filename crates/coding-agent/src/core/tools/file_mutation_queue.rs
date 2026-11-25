//! 对应上游 `src/core/tools/file-mutation-queue.ts`：同一个文件的改、写排队执行；不同文件并行。
//!
//! 上游按 `realpath`（解析符号链接）当键，键在队列空时删掉；这里也按解析后的路径当键，但条目留着
//! 不删（一次会话碰到的路径有限，省掉「删早一步让后来者另起一把锁」的竞态）。

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::Mutex as AsyncMutex;

fn queues() -> &'static Mutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>> {
    static QUEUES: OnceLock<Mutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>> = OnceLock::new();
    QUEUES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// pi: `getMutationQueueKey`
async fn get_mutation_queue_key(file_path: &Path) -> PathBuf {
    let resolved = std::path::absolute(file_path).unwrap_or_else(|_| file_path.to_path_buf());
    match tokio::fs::canonicalize(&resolved).await {
        Ok(path) => path,
        Err(_) => resolved,
    }
}

/// pi: `withFileMutationQueue`——同一文件的回调一个接一个跑。
pub async fn with_file_mutation_queue<T, F, Fut>(file_path: &Path, function: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    // 先登记、后排队：登记这一步按调用顺序串起来，先调用的那条一定先拿到锁（上游同义）。
    let lock = {
        let _registration = {
            static REGISTRATION: OnceLock<AsyncMutex<()>> = OnceLock::new();
            let mutex = REGISTRATION.get_or_init(|| AsyncMutex::new(()));
            mutex.lock().await
        };
        let key = get_mutation_queue_key(file_path).await;
        let mut queues = queues().lock().expect("file mutation queues");
        Arc::clone(queues.entry(key).or_default())
    };
    let _guard = lock.lock_owned().await;
    function().await
}
