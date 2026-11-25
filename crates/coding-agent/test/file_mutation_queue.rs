//! 对应上游 `test/file-mutation-queue.test.ts`：同一个文件的改动排队，不同文件并行。

#[path = "utils/mod.rs"]
mod utils;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;
use xjpcode_agent::AgentTool;
use xjpcode_ai::{AbortController, AbortSignal};
use xjpcode_coding_agent::core::tools::edit::create_edit_tool;
use xjpcode_coding_agent::core::tools::file_mutation_queue::with_file_mutation_queue;
use xjpcode_coding_agent::core::tools::write::create_write_tool;
use xjpcode_coding_agent::{EditOperations, EditToolOptions, WriteOperations, WriteToolOptions};

async fn delay(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

async fn resolves_within<F: std::future::Future>(future: F, ms: u64) -> bool {
    tokio::time::timeout(Duration::from_millis(ms), future)
        .await
        .is_ok()
}

#[tokio::test]
async fn serializes_operations_for_the_same_file() {
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let path = std::path::PathBuf::from("/tmp/file-mutation-queue-same");
    let first_order = Arc::clone(&order);
    let first = with_file_mutation_queue(&path, move || async move {
        first_order.lock().unwrap().push("first:start");
        delay(30).await;
        first_order.lock().unwrap().push("first:end");
    });
    let second_order = Arc::clone(&order);
    let second = with_file_mutation_queue(&path, move || async move {
        second_order.lock().unwrap().push("second:start");
        second_order.lock().unwrap().push("second:end");
    });
    tokio::join!(first, second);
    assert_eq!(
        *order.lock().unwrap(),
        vec!["first:start", "first:end", "second:start", "second:end"]
    );
}

#[tokio::test]
async fn allows_different_files_to_proceed_in_parallel() {
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let order_a = Arc::clone(&order);
    let order_b = Arc::clone(&order);
    let path_a = std::path::PathBuf::from("/tmp/file-mutation-queue-a");
    let path_b = std::path::PathBuf::from("/tmp/file-mutation-queue-b");
    tokio::join!(
        with_file_mutation_queue(&path_a, move || async move {
            order_a.lock().unwrap().push("a:start");
            delay(30).await;
            order_a.lock().unwrap().push("a:end");
        }),
        with_file_mutation_queue(&path_b, move || async move {
            order_b.lock().unwrap().push("b:start");
            delay(30).await;
            order_b.lock().unwrap().push("b:end");
        })
    );
    let order = order.lock().unwrap();
    let position = |needle: &str| order.iter().position(|entry| *entry == needle).unwrap();
    assert!(position("a:start") < position("a:end"));
    assert!(position("b:start") < position("b:end"));
    assert!(position("b:start") < position("a:end"));
}

#[cfg(unix)]
#[tokio::test]
async fn uses_the_same_queue_for_symlink_aliases() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("target.txt");
    let alias = dir.path().join("alias.txt");
    tokio::fs::write(&target, "hello\n").await.unwrap();
    std::os::unix::fs::symlink(&target, &alias).unwrap();

    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let order_target = Arc::clone(&order);
    let order_alias = Arc::clone(&order);
    tokio::join!(
        with_file_mutation_queue(&target, move || async move {
            order_target.lock().unwrap().push("target:start");
            delay(30).await;
            order_target.lock().unwrap().push("target:end");
        }),
        with_file_mutation_queue(&alias, move || async move {
            order_alias.lock().unwrap().push("alias:start");
            order_alias.lock().unwrap().push("alias:end");
        })
    );
    assert_eq!(
        *order.lock().unwrap(),
        vec!["target:start", "target:end", "alias:start", "alias:end"]
    );
}

#[tokio::test]
async fn built_in_edit_and_write_tools_preserve_parallel_edits_on_one_file() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("parallel-edit.txt");
    tokio::fs::write(&file, "alpha\nbeta\ngamma\n").await.unwrap();

    let mut operations = EditOperations::default();
    operations.read_file = Arc::new(|path| {
        Box::pin(async move {
            let content = tokio::fs::read(path).await?;
            delay(30).await;
            Ok(content)
        })
    });
    operations.write_file = Arc::new(|path, content| {
        Box::pin(async move {
            delay(30).await;
            tokio::fs::write(path, content).await
        })
    });
    let tool = create_edit_tool(
        dir.path(),
        EditToolOptions {
            operations: Some(operations),
        },
    );
    let first_args = utils::tools::object(json!({ "path": file, "edits": [{ "oldText": "alpha", "newText": "ALPHA" }] }));
    let second_args = utils::tools::object(json!({ "path": file, "edits": [{ "oldText": "beta", "newText": "BETA" }] }));
    let first = tool.execute("call-1", &first_args, None, None);
    let second = tool.execute("call-2", &second_args, None, None);
    let (first, second) = tokio::join!(first, second);
    first.unwrap();
    second.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "ALPHA\nBETA\ngamma\n"
    );
}

#[tokio::test]
async fn edit_and_write_share_the_queue() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("mixed.txt");
    tokio::fs::write(&file, "original\n").await.unwrap();

    let mut edit_operations = EditOperations::default();
    edit_operations.read_file = Arc::new(|path| {
        Box::pin(async move {
            let content = tokio::fs::read(path).await?;
            delay(30).await;
            Ok(content)
        })
    });
    edit_operations.write_file = Arc::new(|path, content| {
        Box::pin(async move {
            delay(30).await;
            tokio::fs::write(path, content).await
        })
    });
    let edit_tool = create_edit_tool(
        dir.path(),
        EditToolOptions {
            operations: Some(edit_operations),
        },
    );
    let write_operations = WriteOperations {
        mkdir: Arc::new(|_dir| Box::pin(async move { Ok(()) })),
        write_file: Arc::new(|path, content| {
            Box::pin(async move {
                delay(10).await;
                tokio::fs::write(path, content).await
            })
        }),
    };
    let write_tool = create_write_tool(
        dir.path(),
        WriteToolOptions {
            operations: Some(write_operations),
        },
    );

    let edit_args = utils::tools::object(json!({ "path": file, "edits": [{ "oldText": "original", "newText": "edited" }] }));
    let edit = edit_tool.execute("call-1", &edit_args, None, None);
    delay(5).await;
    let write_args = utils::tools::object(json!({ "path": file, "content": "replacement\n" }));
    let write = write_tool.execute("call-2", &write_args, None, None);
    let (edit, write) = tokio::join!(edit, write);
    edit.unwrap();
    write.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "replacement\n"
    );
}

#[tokio::test]
async fn keeps_the_write_queue_locked_while_an_aborted_write_is_still_in_flight() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("abort-write.txt");
    let first_write_started = Arc::new(tokio::sync::Notify::new());
    let finish_first_write = Arc::new(tokio::sync::Notify::new());
    let second_write_started = Arc::new(tokio::sync::Notify::new());
    let first_write_settled = Arc::new(AtomicBool::new(false));

    let operations = WriteOperations {
        mkdir: Arc::new(|_dir| Box::pin(async move { Ok(()) })),
        write_file: {
            let first_write_started = Arc::clone(&first_write_started);
            let finish_first_write = Arc::clone(&finish_first_write);
            let second_write_started = Arc::clone(&second_write_started);
            let first_write_settled = Arc::clone(&first_write_settled);
            Arc::new(move |path, content| {
                let first_write_started = Arc::clone(&first_write_started);
                let finish_first_write = Arc::clone(&finish_first_write);
                let second_write_started = Arc::clone(&second_write_started);
                let first_write_settled = Arc::clone(&first_write_settled);
                Box::pin(async move {
                    if content == "first\n" {
                        first_write_started.notify_one();
                        finish_first_write.notified().await;
                        tokio::fs::write(path, content).await?;
                        first_write_settled.store(true, Ordering::SeqCst);
                        return Ok(());
                    }
                    if content == "second\n" {
                        assert!(first_write_settled.load(Ordering::SeqCst));
                        second_write_started.notify_one();
                    }
                    tokio::fs::write(path, content).await
                })
            })
        },
    };
    let tool = Arc::new(create_write_tool(
        dir.path(),
        WriteToolOptions {
            operations: Some(operations),
        },
    ));

    let controller = AbortController::new();
    let signal: AbortSignal = controller.signal();
    let first_task = {
        let tool = Arc::clone(&tool);
        let file = file.clone();
        tokio::spawn(async move {
            tool.execute(
                "call-1",
                &utils::tools::object(json!({ "path": file, "content": "first\n" })),
                Some(&signal),
                None,
            )
            .await
        })
    };
    first_write_started.notified().await;
    controller.abort();

    let second_args = utils::tools::object(json!({ "path": file, "content": "second\n" }));
    let second = tool.execute("call-2", &second_args, None, None);
    assert!(!resolves_within(async { second_write_started.notified().await }, 20).await);
    finish_first_write.notify_one();
    let first_result = first_task.await.unwrap();
    assert_eq!(first_result.unwrap_err().message(), "Operation aborted");
    second.await.unwrap();
    assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "second\n");
}

#[tokio::test]
async fn keeps_the_edit_queue_locked_while_an_aborted_edit_is_still_in_flight() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("abort-edit.txt");
    tokio::fs::write(&file, "alpha\nbeta\n").await.unwrap();
    let first_write_started = Arc::new(tokio::sync::Notify::new());
    let finish_first_write = Arc::new(tokio::sync::Notify::new());
    let second_write_started = Arc::new(tokio::sync::Notify::new());
    let first_write_settled = Arc::new(AtomicBool::new(false));

    let operations = EditOperations {
        access: Arc::new(|_path| Box::pin(async move { Ok(()) })),
        read_file: Arc::new(|path| Box::pin(async move { tokio::fs::read(path).await })),
        write_file: {
            let first_write_started = Arc::clone(&first_write_started);
            let finish_first_write = Arc::clone(&finish_first_write);
            let second_write_started = Arc::clone(&second_write_started);
            let first_write_settled = Arc::clone(&first_write_settled);
            Arc::new(move |path, content| {
                let first_write_started = Arc::clone(&first_write_started);
                let finish_first_write = Arc::clone(&finish_first_write);
                let second_write_started = Arc::clone(&second_write_started);
                let first_write_settled = Arc::clone(&first_write_settled);
                Box::pin(async move {
                    if content == "ALPHA\nbeta\n" {
                        first_write_started.notify_one();
                        finish_first_write.notified().await;
                        tokio::fs::write(path, content).await?;
                        first_write_settled.store(true, Ordering::SeqCst);
                        return Ok(());
                    }
                    if content == "ALPHA\nBETA\n" || content == "alpha\nBETA\n" {
                        assert!(first_write_settled.load(Ordering::SeqCst));
                        second_write_started.notify_one();
                    }
                    tokio::fs::write(path, content).await
                })
            })
        },
    };
    let tool = Arc::new(create_edit_tool(
        dir.path(),
        EditToolOptions {
            operations: Some(operations),
        },
    ));

    let controller = AbortController::new();
    let signal: AbortSignal = controller.signal();
    let first_task = {
        let tool = Arc::clone(&tool);
        let file = file.clone();
        tokio::spawn(async move {
            tool.execute(
                "call-1",
                &utils::tools::object(json!({ "path": file, "edits": [{ "oldText": "alpha", "newText": "ALPHA" }] })),
                Some(&signal),
                None,
            )
            .await
        })
    };
    first_write_started.notified().await;
    controller.abort();

    let second_args = utils::tools::object(json!({ "path": file, "edits": [{ "oldText": "beta", "newText": "BETA" }] }));
    let second = tool.execute("call-2", &second_args, None, None);
    assert!(!resolves_within(async { second_write_started.notified().await }, 20).await);
    finish_first_write.notify_one();
    let first_result = first_task.await.unwrap();
    assert_eq!(first_result.unwrap_err().message(), "Operation aborted");
    second.await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&file).await.unwrap(),
        "ALPHA\nBETA\n"
    );
}
