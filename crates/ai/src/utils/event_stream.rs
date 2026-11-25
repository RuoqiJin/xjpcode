//! 对应上游 `packages/ai/src/utils/event-stream.ts`。
//!
//! 上游是 `AsyncIterable` + Promise：生产者 `push`/`end`（同步调用），消费者 `for await`，
//! 最终结果由 `result()` 的 Promise 给出。Rust 侧保持同一条异步链：`push`/`end` 同步、
//! `next().await`、`result().await`，因此后面搬循环时每个 `await` 都能一一对上。
//!
//! 投递顺序与上游一致：`push` 先给最早注册的等待者，其次排队；`end` 唤醒所有等待者。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::oneshot;

use crate::types::{AssistantMessage, AssistantMessageEvent};

/// 一次投递给等待中消费者的载荷。
enum Delivery<T> {
    Event(T),
    End,
}

struct StreamState<T, R> {
    queue: VecDeque<T>,
    /// 等待者按注册顺序排队；每个等待者拿到一个事件或一个结束信号。
    waiting: VecDeque<oneshot::Sender<Delivery<T>>>,
    result_waiters: Vec<oneshot::Sender<()>>,
    done: bool,
    result: Option<R>,
}

struct Inner<T, R> {
    state: Mutex<StreamState<T, R>>,
    is_complete: Box<dyn Fn(&T) -> bool + Send + Sync>,
    extract_result: Box<dyn Fn(&T) -> R + Send + Sync>,
}

/// 通用事件通道。
///
/// 是一个可克隆的句柄：克隆只增加引用，事件队列与结果仍只有一份，因此生产者和
/// 多个消费者可以各自持有它并跨任务移动。
pub struct EventStream<T, R> {
    inner: Arc<Inner<T, R>>,
}

impl<T, R> Clone for EventStream<T, R> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T, R> EventStream<T, R> {
    pub fn new(
        is_complete: impl Fn(&T) -> bool + Send + Sync + 'static,
        extract_result: impl Fn(&T) -> R + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(StreamState {
                    queue: VecDeque::new(),
                    waiting: VecDeque::new(),
                    result_waiters: Vec::new(),
                    done: false,
                    result: None,
                }),
                is_complete: Box::new(is_complete),
                extract_result: Box::new(extract_result),
            }),
        }
    }

    /// 推入一个事件。与上游一样是同步调用；完成之后到达的事件被直接丢弃。
    pub fn push(&self, event: T) {
        let mut state = self.inner.state.lock().expect("stream state");
        if state.done {
            return;
        }

        if (self.inner.is_complete)(&event) {
            state.done = true;
            let result = (self.inner.extract_result)(&event);
            Self::resolve(&mut state, result);
        }

        let mut delivery = Delivery::Event(event);
        loop {
            let Some(waiter) = state.waiting.pop_front() else {
                if let Delivery::Event(event) = delivery {
                    state.queue.push_back(event);
                }
                return;
            };
            match waiter.send(delivery) {
                Ok(()) => return,
                // 等待者的 future 已被丢弃（例如任务被取消）：把事件交给下一个等待者，
                // 而不是让它消失。
                Err(returned) => delivery = returned,
            }
        }
    }

    /// 结束流。给了结果就同时敲定最终结果；不给则与上游一样，`result()` 永不返回。
    pub fn end(&self, result: Option<R>) {
        let mut state = self.inner.state.lock().expect("stream state");
        state.done = true;
        if let Some(result) = result {
            Self::resolve(&mut state, result);
        }
        let waiters: Vec<_> = state.waiting.drain(..).collect();
        drop(state);
        for waiter in waiters {
            let _ = waiter.send(Delivery::End);
        }
    }

    /// 等待最终结果落定，返回它的副本。
    pub async fn result(&self) -> R
    where
        R: Clone,
    {
        loop {
            let receiver = {
                let mut state = self.inner.state.lock().expect("stream state");
                if let Some(result) = &state.result {
                    return result.clone();
                }
                let (sender, receiver) = oneshot::channel();
                state.result_waiters.push(sender);
                receiver
            };
            // 唤醒后回到循环顶部重新取锁读结果：值只存一份，等待者只收信号。
            let _ = receiver.await;
        }
    }

    pub fn is_done(&self) -> bool {
        self.inner.state.lock().expect("stream state").done
    }

    /// 当前注册在流上的等待者数量（上游私有的 `waiting.length`）。
    ///
    /// 取消掉的 `next()` future 只有在下一次 `push`/`end` 触及时才会被剔除，因此这个
    /// 值可能短暂偏大。
    pub fn pending_consumers(&self) -> usize {
        self.inner.state.lock().expect("stream state").waiting.len()
    }

    /// 新建一个消费者。每次 `next().await` 在其等待期间占一个位置，顺序与注册顺序一致。
    pub fn consumer(&self) -> Consumer<T, R> {
        Consumer {
            stream: self.clone(),
        }
    }

    fn resolve(state: &mut StreamState<T, R>, result: R) {
        state.result = Some(result);
        for waiter in state.result_waiters.drain(..) {
            let _ = waiter.send(());
        }
    }
}

/// 一个事件消费者。同一个消费者上不得并发调用 `next()`。
pub struct Consumer<T, R> {
    stream: EventStream<T, R>,
}

impl<T, R> Consumer<T, R> {
    /// 取下一个事件；流已结束且队列为空时返回 `None`。
    pub async fn next(&mut self) -> Option<T> {
        let receiver = {
            let mut state = self.stream.inner.state.lock().expect("stream state");
            if let Some(event) = state.queue.pop_front() {
                return Some(event);
            }
            if state.done {
                return None;
            }
            let (sender, receiver) = oneshot::channel();
            state.waiting.push_back(sender);
            receiver
        };

        match receiver.await {
            Ok(Delivery::Event(event)) => Some(event),
            // 发送端消失只可能是流本身被丢弃，此时也没有更多事件了。
            Ok(Delivery::End) | Err(_) => None,
        }
    }
}

/// 流式时钟。上游用 `performance.now()` 的单调读数。
fn monotonic_ms(started_at: Instant) -> u64 {
    (started_at.elapsed().as_secs_f64() * 1000.0).round() as u64
}

fn wall_clock_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// 一次 assistant 响应的流。它在给最终消息打上 `durationMs` 的同时记录计时起点。
///
/// 计时规则与上游一致：只给本流看着开始的响应计时（消息已有 `durationMs` 或
/// `timestamp` 早于本流创建时间时不覆盖），因此转发一条在别处开始的响应
/// （例如稍后取回的 deferred 结果）不会破坏原始测量。
#[derive(Clone)]
pub struct AssistantMessageEventStream {
    inner: EventStream<AssistantMessageEvent, AssistantMessage>,
    started_at_ms: u64,
    started_at: Instant,
}

impl Default for AssistantMessageEventStream {
    fn default() -> Self {
        Self::new()
    }
}

impl AssistantMessageEventStream {
    pub fn new() -> Self {
        Self {
            inner: EventStream::new(
                |event| {
                    matches!(
                        event,
                        AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
                    )
                },
                |event| match event {
                    AssistantMessageEvent::Done { message, .. } => message.clone(),
                    AssistantMessageEvent::Error { error, .. } => error.clone(),
                    _ => panic!("Unexpected event type for final result"),
                },
            ),
            started_at_ms: wall_clock_ms(),
            started_at: Instant::now(),
        }
    }

    pub fn push(&self, mut event: AssistantMessageEvent) {
        match &mut event {
            AssistantMessageEvent::Done { message, .. } => self.time(message),
            AssistantMessageEvent::Error { error, .. } => self.time(error),
            _ => {}
        }
        self.inner.push(event);
    }

    pub fn end(&self, mut result: Option<AssistantMessage>) {
        if let Some(message) = result.as_mut() {
            self.time(message);
        }
        self.inner.end(result);
    }

    pub async fn result(&self) -> AssistantMessage {
        self.inner.result().await
    }

    pub fn is_done(&self) -> bool {
        self.inner.is_done()
    }

    pub fn pending_consumers(&self) -> usize {
        self.inner.pending_consumers()
    }

    pub fn consumer(&self) -> Consumer<AssistantMessageEvent, AssistantMessage> {
        self.inner.consumer()
    }

    fn time(&self, message: &mut AssistantMessage) {
        if self.inner.is_done()
            || message.duration_ms.is_some()
            || message.timestamp < self.started_at_ms
        {
            return;
        }
        message.duration_ms = Some(monotonic_ms(self.started_at));
    }
}

/// 供扩展使用的构造入口，与上游 `createAssistantMessageEventStream()` 对应。
pub fn create_assistant_message_event_stream() -> AssistantMessageEventStream {
    AssistantMessageEventStream::new()
}
