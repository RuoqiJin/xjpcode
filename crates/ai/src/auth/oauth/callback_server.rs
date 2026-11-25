//! 对应上游 `packages/ai/src/auth/oauth/callback-server.ts`。
//!
//! 浏览器登录的回调：在本机起一个只收一条回调的 HTTP 服务，把授权码交给登录流程的 `complete`，
//! 浏览器上显示成功或失败那一页。端口被占用时直接报错（由调用方决定退回粘贴链接）。
//!
//! 与上游不同的：HTTP 服务用 `tokio` 的 TCP 自己写（上游用 node:http），只实现这一条需要的部分
//! （读请求行、回一段 HTML）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::auth::types::{AuthPrompt, ModelsError, ModelsErrorCode, ProviderAuthInteraction};
use crate::types::BoxFuture;
use crate::utils::abort::{AbortController, AbortSignal};
use crate::utils::oauth_page::{oauth_error_html, oauth_success_html};

type CallbackSender<T> = Arc<Mutex<Option<oneshot::Sender<Result<Option<T>, ModelsError>>>>>;

/// pi: `OAuthCallbackServerOptions<T>`
pub struct OAuthCallbackServerOptions<T: Send + 'static> {
    pub provider_name: String,
    pub host: String,
    pub port: u16,
    pub path: String,
    /// `redirectUri` 里的主机名与监听地址不同时用它（例如 `localhost`）。
    pub redirect_host: Option<String>,
    pub state: Option<String>,
    /// 浏览器那一页发出之前把授权码换成结果，这样页面上能显示失败原因。
    /// 想稍后再换就给 `Ok(code)`。
    pub complete: Arc<dyn Fn(String) -> BoxFuture<'static, Result<T, ModelsError>> + Send + Sync>,
    pub signal: Option<AbortSignal>,
    pub timeout_ms: Option<u64>,
}

struct CallbackState {
    settled: AtomicBool,
    claimed: AtomicBool,
    shutdown: AbortController,
}

/// pi: `OAuthCallbackServer<T>`
pub struct OAuthCallbackServer<T: Send + 'static> {
    pub redirect_uri: String,
    wait: Option<oneshot::Receiver<Result<Option<T>, ModelsError>>>,
    state: Arc<CallbackState>,
    sender: CallbackSender<T>,
}

/// `OAuthCallbackServer::cancel` 的独立句柄。
pub struct OAuthCallbackCancel<T: Send + 'static> {
    state: Arc<CallbackState>,
    sender: CallbackSender<T>,
}

impl<T: Send + 'static> OAuthCallbackCancel<T> {
    /// 还没被处理时按「改成手动输入」收场（监听继续开着）。
    pub fn cancel(&self) {
        if !self.state.claimed.load(Ordering::SeqCst) {
            finish(&self.state, &self.sender, Ok(None));
        }
    }

    /// 关掉监听（上游的 `close()`；调用方的 `finally` 里调）。
    pub fn close(&self) {
        finish(
            &self.state,
            &self.sender,
            Err(ModelsError::login("OAuth callback server closed")),
        );
        self.state.shutdown.abort();
    }
}

/// 收场：交出结果，但监听还开着（`cancel()` 之后迟到的回调要能收到 409，照上游）。
fn finish<T>(state: &CallbackState, sender: &CallbackSender<T>, result: Result<Option<T>, ModelsError>) {
    if state.settled.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(sender) = sender.lock().expect("callback sender").take() {
        let _ = sender.send(result);
    }
}

impl<T: Send + 'static> OAuthCallbackServer<T> {
    /// pi: `server.wait()`——等到浏览器回调、`cancel()`（`None`）、或失败。
    pub async fn wait(&mut self) -> Result<Option<T>, ModelsError> {
        self.wait
            .take()
            .expect("wait called twice")
            .await
            .unwrap_or_else(|_| Err(ModelsError::login("OAuth callback server closed")))
    }

    /// 一个可以在别处调用的取消句柄（与 `wait` 同时持有不冲突）。
    pub fn cancel_handle(&self) -> OAuthCallbackCancel<T> {
        OAuthCallbackCancel {
            state: Arc::clone(&self.state),
            sender: Arc::clone(&self.sender),
        }
    }

    /// pi: `server.cancel()`——还没被处理时按「改成手动输入」收场。
    pub fn cancel(&self) {
        if !self.state.claimed.load(Ordering::SeqCst) {
            finish(&self.state, &self.sender, Ok(None));
        }
    }

    /// pi: `server.close()`
    pub fn close(&self) {
        self.cancel_handle().close();
    }
}

async fn send_page(stream: &mut tokio::net::TcpStream, status: u16, html: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        502 => "Bad Gateway",
        _ => "Error",
    };
    let body = html.as_bytes();
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/html; charset=utf-8\r\ncache-control: no-store\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    // 写失败一般意味着浏览器已经断开；页面没送到不影响登录结果的送达。
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body).await;
    let _ = stream.flush().await;
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Option<(String, String)> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if buffer.len() > 64 * 1024 {
            return None;
        }
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let text = String::from_utf8_lossy(&buffer);
    let mut parts = text.lines().next()?.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    Some((method, target))
}

#[allow(clippy::too_many_arguments)]
async fn handle_connection<T: Send + 'static>(
    mut stream: tokio::net::TcpStream,
    path: String,
    expected_state: Option<String>,
    provider_name: String,
    complete: Arc<dyn Fn(String) -> BoxFuture<'static, Result<T, ModelsError>> + Send + Sync>,
    state: Arc<CallbackState>,
    sender: CallbackSender<T>,
) {
    let Some((method, target)) = read_request(&mut stream).await else {
        return;
    };
    let url = url::Url::parse("http://localhost")
        .ok()
        .and_then(|base| base.join(&target).ok());
    let pathname = url.as_ref().map(|url| url.path().to_string());
    if method != "GET" || pathname.as_deref() != Some(path.as_str()) {
        send_page(
            &mut stream,
            404,
            &oauth_error_html("Callback route not found.", None),
        )
        .await;
        return;
    }
    let params: std::collections::BTreeMap<String, String> = url
        .as_ref()
        .map(|url| {
            url.query_pairs()
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    if let Some(expected) = &expected_state {
        if params.get("state") != Some(expected) {
            send_page(&mut stream, 400, &oauth_error_html("State mismatch.", None)).await;
            return;
        }
    }
    if state.claimed.load(Ordering::SeqCst) || state.settled.load(Ordering::SeqCst) {
        send_page(
            &mut stream,
            409,
            &oauth_error_html("This sign-in has already been handled.", None),
        )
        .await;
        return;
    }
    if let Some(error) = params.get("error") {
        let description = params
            .get("error_description")
            .cloned()
            .unwrap_or_else(|| error.clone());
        send_page(
            &mut stream,
            400,
            &oauth_error_html(
                &format!("{provider_name} authorization failed."),
                Some(&description),
            ),
        )
        .await;
        finish(
            &state,
            &sender,
            Err(ModelsError::login(format!(
                "{provider_name} authorization failed: {description}"
            ))),
        );
        return;
    }
    let Some(code) = params.get("code").cloned() else {
        send_page(
            &mut stream,
            400,
            &oauth_error_html("Missing authorization code.", None),
        )
        .await;
        return;
    };
    if state.claimed.swap(true, Ordering::SeqCst) {
        send_page(
            &mut stream,
            409,
            &oauth_error_html("This sign-in has already been handled.", None),
        )
        .await;
        return;
    }
    match complete(code).await {
        Ok(value) => {
            send_page(
                &mut stream,
                200,
                &oauth_success_html(&format!(
                    "Signed in to {provider_name}. You may now close this page."
                )),
            )
            .await;
            finish(&state, &sender, Ok(Some(value)));
        }
        Err(error) => {
            send_page(
                &mut stream,
                502,
                &oauth_error_html(
                    &format!("{provider_name} sign-in failed."),
                    Some(&error.message),
                ),
            )
            .await;
            finish(&state, &sender, Err(error));
        }
    }
}

/// pi: `startOAuthCallbackServer`
pub async fn start_oauth_callback_server<T: Send + 'static>(
    options: OAuthCallbackServerOptions<T>,
) -> Result<OAuthCallbackServer<T>, ModelsError> {
    let OAuthCallbackServerOptions {
        provider_name,
        host,
        port,
        path,
        redirect_host,
        state: expected_state,
        complete,
        signal,
        timeout_ms,
    } = options;
    if signal.as_ref().is_some_and(|signal| signal.aborted()) {
        return Err(ModelsError::login("Login cancelled"));
    }

    let listener = TcpListener::bind((host.as_str(), port)).await.map_err(|error| {
        ModelsError::new(
            ModelsErrorCode::Auth,
            "OAuth callback server did not bind to TCP",
        )
        .with_cause(error.to_string())
    })?;
    let address = listener.local_addr().map_err(|error| {
        ModelsError::new(
            ModelsErrorCode::Auth,
            "OAuth callback server did not bind to TCP",
        )
        .with_cause(error.to_string())
    })?;

    let (tx, rx) = oneshot::channel();
    let sender: CallbackSender<T> = Arc::new(Mutex::new(Some(tx)));
    let state = Arc::new(CallbackState {
        settled: AtomicBool::new(false),
        claimed: AtomicBool::new(false),
        shutdown: AbortController::new(),
    });

    // 监听循环：只收一条回调，收场时取消这个循环（已接下的连接不受影响）。
    {
        let shutdown = state.shutdown.clone();
        let path = path.clone();
        let provider_name = provider_name.clone();
        let complete = Arc::clone(&complete);
        let state = Arc::clone(&state);
        let sender = Arc::clone(&sender);
        let shutdown_signal = shutdown.signal();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown_signal.cancelled() => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break };
                        let path = path.clone();
                        let provider_name = provider_name.clone();
                        let complete = Arc::clone(&complete);
                        let state = Arc::clone(&state);
                        let sender = Arc::clone(&sender);
                        let expected_state = expected_state.clone();
                        tokio::spawn(async move {
                            handle_connection(stream, path, expected_state, provider_name, complete, state, sender)
                                .await;
                        });
                    }
                }
            }
        });
    }

    if let Some(signal) = signal {
        let state = Arc::clone(&state);
        let sender = Arc::clone(&sender);
        tokio::spawn(async move {
            signal.cancelled().await;
            finish(&state, &sender, Err(ModelsError::login("Login cancelled")));
        });
    }
    if let Some(timeout_ms) = timeout_ms {
        let state = Arc::clone(&state);
        let sender = Arc::clone(&sender);
        let provider_name = provider_name.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)).await;
            finish(
                &state,
                &sender,
                Err(ModelsError::login(format!(
                    "{provider_name} sign-in timed out"
                ))),
            );
        });
    }

    let redirect_host = redirect_host.unwrap_or_else(|| host.clone());
    let rendered_host = if redirect_host.contains(':') {
        format!("[{redirect_host}]")
    } else {
        redirect_host
    };
    Ok(OAuthCallbackServer {
        redirect_uri: format!("http://{rendered_host}:{}{path}", address.port()),
        wait: Some(rx),
        state,
        sender,
    })
}

/// pi: `waitForCallbackOrManualInput` 的结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackOrManual<T> {
    Callback(T),
    Manual(String),
}

/// pi: `waitForCallbackOrManualInput`
///
/// 浏览器回调与「粘贴授权码」同时等：谁先到用谁；手动输入先到时关掉回调服务。
pub async fn wait_for_callback_or_manual_input<T: Send + 'static>(
    interaction: &dyn ProviderAuthInteraction,
    callback: Option<OAuthCallbackServer<T>>,
    message: &str,
    placeholder: &str,
) -> Result<CallbackOrManual<T>, ModelsError> {
    let manual_controller = AbortController::new();
    let prompt = AuthPrompt::ManualCode {
        message: message.to_string(),
        placeholder: Some(placeholder.to_string()),
    };
    let manual = interaction.prompt(prompt, Some(manual_controller.signal()));

    let Some(mut callback) = callback else {
        let input = manual.await?;
        return Ok(CallbackOrManual::Manual(input));
    };

    let control = callback.cancel_handle();
    let result = {
        tokio::pin!(manual);
        let wait = callback.wait();
        tokio::pin!(wait);
        tokio::select! {
            value = &mut wait => {
                match value {
                    Ok(Some(value)) => {
                        manual_controller.abort();
                        Ok(CallbackOrManual::Callback(value))
                    }
                    // 浏览器回调被取消：等用户把链接/授权码贴回来（上游也是这个顺序）。
                    Ok(None) => {
                        let input = manual.await?;
                        Ok(CallbackOrManual::Manual(input))
                    }
                    Err(error) => {
                        manual_controller.abort();
                        Err(error)
                    }
                }
            }
            input = &mut manual => {
                control.cancel();
                Ok(CallbackOrManual::Manual(input?))
            }
        }
    };
    control.close();
    result
}
