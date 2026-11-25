//! 对应上游 `packages/ai/test/oauth-callback-server.test.ts`。
//!
//! 只连本机：起服务、发 HTTP、看页面与 `wait()` 的结果。与上游一样逐个顺序跑
//! （`#[tokio::test]` 默认每个测试一个运行时，端口不会互相抢）。

use std::sync::{Arc, Mutex};

use xjpcode_ai::auth::oauth::callback_server::{
    start_oauth_callback_server, wait_for_callback_or_manual_input, CallbackOrManual,
    OAuthCallbackServerOptions,
};
use xjpcode_ai::auth::{
    AuthEvent, AuthInteraction, AuthPrompt, ModelsError, ProviderAuthInteraction,
};
use xjpcode_ai::types::BoxFuture;
use xjpcode_ai::AbortSignal;

async fn start<T: Send + 'static>(
    provider_name: &str,
    port: u16,
    state: Option<&str>,
    redirect_host: Option<&str>,
    complete: Arc<dyn Fn(String) -> BoxFuture<'static, Result<T, ModelsError>> + Send + Sync>,
) -> Result<xjpcode_ai::auth::oauth::callback_server::OAuthCallbackServer<T>, ModelsError> {
    start_oauth_callback_server(OAuthCallbackServerOptions {
        provider_name: provider_name.to_string(),
        host: "127.0.0.1".to_string(),
        port,
        path: "/callback".to_string(),
        redirect_host: redirect_host.map(str::to_string),
        state: state.map(str::to_string),
        complete,
        signal: None,
        timeout_ms: None,
    })
    .await
}

fn echo_complete() -> Arc<dyn Fn(String) -> BoxFuture<'static, Result<String, ModelsError>> + Send + Sync> {
    Arc::new(|code: String| Box::pin(async move { Ok(format!("completed:{code}")) }))
}

fn callback_url(redirect_uri: &str, params: &[(&str, &str)]) -> String {
    let mut url = url::Url::parse(redirect_uri).unwrap();
    for (key, value) in params {
        url.query_pairs_mut().append_pair(key, value);
    }
    url.to_string()
}

struct Page {
    status: u16,
    content_type: Option<String>,
    body: String,
}

async fn page(url: String) -> Page {
    let response = reqwest::Client::new().get(url).send().await.unwrap();
    Page {
        status: response.status().as_u16(),
        content_type: response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        body: response.text().await.unwrap(),
    }
}

#[tokio::test]
async fn ignores_stray_requests_and_resolves_with_the_completed_code() {
    let server = start("Example", 0, Some("expected-state"), None, echo_complete())
        .await
        .unwrap();
    assert!(server
        .redirect_uri
        .starts_with("http://127.0.0.1:"));
    assert!(server.redirect_uri.ends_with("/callback"));

    let redirect_uri = server.redirect_uri.clone();
    let wrong_path = page(
        url::Url::parse(&redirect_uri)
            .unwrap()
            .join("/other")
            .unwrap()
            .to_string(),
    )
    .await;
    assert_eq!(wrong_path.status, 404);

    let wrong_state = page(callback_url(&redirect_uri, &[("code", "c"), ("state", "other")])).await;
    assert_eq!(wrong_state.status, 400);
    assert_eq!(
        wrong_state.content_type.as_deref(),
        Some("text/html; charset=utf-8")
    );
    assert!(wrong_state.body.contains("State mismatch."));

    let post = reqwest::Client::new()
        .post(callback_url(
            &redirect_uri,
            &[("code", "c"), ("state", "expected-state")],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(post.status().as_u16(), 404);

    let missing = page(callback_url(&redirect_uri, &[("state", "expected-state")])).await;
    assert_eq!(missing.status, 400);

    let success = page(callback_url(
        &redirect_uri,
        &[("code", "the-code"), ("state", "expected-state")],
    ))
    .await;
    assert_eq!(success.status, 200);
    assert!(success.body.contains("Authentication successful"));
    assert!(success.body.contains("Signed in to Example."));
    assert!(success.body.contains("fill=\"#F09082\""));
    assert!(success.body.contains("fill=\"#4D9ABF\""));
    assert!(success.body.contains("fill=\"#F1BE58\""));

    let mut server = server;
    assert_eq!(
        server.wait().await.unwrap(),
        Some("completed:the-code".to_string())
    );
}

#[tokio::test]
async fn uses_the_redirect_host_and_skips_the_state_check_when_none_is_expected() {
    let mut server = start("Example", 0, None, Some("localhost"), echo_complete())
        .await
        .unwrap();
    assert!(server.redirect_uri.starts_with("http://localhost:"));
    let url = callback_url(&server.redirect_uri, &[("code", "no-state")]);
    assert_eq!(page(url).await.status, 200);
    assert_eq!(
        server.wait().await.unwrap(),
        Some("completed:no-state".to_string())
    );
}

#[tokio::test]
async fn shows_completion_failures_on_the_page_and_rejects_the_wait() {
    let mut server = start::<String>(
        "Example",
        0,
        Some("expected-state"),
        None,
        Arc::new(|_code: String| {
            Box::pin(async {
                Err(ModelsError::login("token exchange failed"))
            })
        }),
    )
    .await
    .unwrap();
    let failure = page(callback_url(
        &server.redirect_uri,
        &[("code", "c"), ("state", "expected-state")],
    ))
    .await;
    assert_eq!(failure.status, 502);
    assert!(failure.body.contains("Example sign-in failed."));
    assert!(failure.body.contains("token exchange failed"));
    let error = server.wait().await.unwrap_err();
    assert_eq!(error.message, "token exchange failed");
}

#[tokio::test]
async fn rejects_the_wait_when_the_provider_redirects_with_an_error() {
    let mut server = start("Example", 0, Some("expected-state"), None, echo_complete())
        .await
        .unwrap();
    let failure = page(callback_url(
        &server.redirect_uri,
        &[
            ("error", "access_denied"),
            ("error_description", "User denied access"),
            ("state", "expected-state"),
        ],
    ))
    .await;
    assert_eq!(failure.status, 400);
    assert!(failure.body.contains("User denied access"));
    let error = server.wait().await.unwrap_err();
    assert_eq!(
        error.message,
        "Example authorization failed: User denied access"
    );
}

#[tokio::test]
async fn completes_only_the_first_callback() {
    let finish: Arc<Mutex<Option<tokio::sync::oneshot::Sender<String>>>> =
        Arc::new(Mutex::new(None));
    let finish_for_complete = Arc::clone(&finish);
    let mut server = start(
        "Example",
        0,
        Some("expected-state"),
        None,
        Arc::new(move |_code: String| {
            let finish = Arc::clone(&finish_for_complete);
            Box::pin(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                *finish.lock().unwrap() = Some(tx);
                Ok(rx.await.unwrap_or_default())
            })
        }),
    )
    .await
    .unwrap();
    let url = callback_url(
        &server.redirect_uri,
        &[("code", "c"), ("state", "expected-state")],
    );
    let first = tokio::spawn(page(url.clone()));
    for _ in 0..50 {
        if finish.lock().unwrap().is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(finish.lock().unwrap().is_some());
    let second = page(url).await;
    assert_eq!(second.status, 409);
    server.cancel();
    finish.lock().unwrap().take().unwrap().send("done".to_string()).unwrap();
    assert_eq!(first.await.unwrap().status, 200);
    assert_eq!(server.wait().await.unwrap(), Some("done".to_string()));
}

#[tokio::test]
async fn resolves_with_none_after_cancel_and_rejects_late_callbacks() {
    let mut server = start("Example", 0, Some("expected-state"), None, echo_complete())
        .await
        .unwrap();
    let cancel = server.cancel_handle();
    cancel.cancel();
    assert_eq!(server.wait().await.unwrap(), None);

    let server = start("Example", 0, Some("expected-state"), None, echo_complete())
        .await
        .unwrap();
    server.cancel();
    let late = page(callback_url(
        &server.redirect_uri,
        &[("code", "c"), ("state", "expected-state")],
    ))
    .await;
    assert_eq!(late.status, 409);
}

#[tokio::test]
async fn rejects_the_wait_on_abort_and_on_timeout() {
    let controller = xjpcode_ai::AbortController::new();
    let server = start_oauth_callback_server(OAuthCallbackServerOptions {
        provider_name: "Example".to_string(),
        host: "127.0.0.1".to_string(),
        port: 0,
        path: "/callback".to_string(),
        redirect_host: None,
        state: None,
        complete: echo_complete(),
        signal: Some(controller.signal()),
        timeout_ms: None,
    })
    .await
    .unwrap();
    controller.abort();
    let mut server = server;
    assert_eq!(server.wait().await.unwrap_err().message, "Login cancelled");

    let mut timed_out = start_oauth_callback_server(OAuthCallbackServerOptions {
        provider_name: "Example".to_string(),
        host: "127.0.0.1".to_string(),
        port: 0,
        path: "/callback".to_string(),
        redirect_host: None,
        state: None,
        complete: echo_complete(),
        signal: None,
        timeout_ms: Some(10),
    })
    .await
    .unwrap();
    assert_eq!(
        timed_out.wait().await.unwrap_err().message,
        "Example sign-in timed out"
    );

    let already = xjpcode_ai::AbortController::new();
    already.abort();
    let error = start_oauth_callback_server(OAuthCallbackServerOptions {
        provider_name: "Example".to_string(),
        host: "127.0.0.1".to_string(),
        port: 0,
        path: "/callback".to_string(),
        redirect_host: None,
        state: None,
        complete: echo_complete(),
        signal: Some(already.signal()),
        timeout_ms: None,
    })
    .await
    .err()
    .unwrap();
    assert_eq!(error.message, "Login cancelled");
}

#[tokio::test]
async fn fails_instead_of_picking_another_port_when_the_requested_port_is_taken() {
    let blocker = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = blocker.local_addr().unwrap().port();
    let error = start("Example", port, None, None, echo_complete())
        .await
        .err()
        .unwrap();
    assert!(error.message.contains("did not bind"));
    drop(blocker);
}

/// 只回答一次提问的交互；用来让「浏览器先到、手动输入被取消」这条路过一遍。
struct PendingManualPrompt {
    signal: AbortSignal,
    prompt_signal: Arc<Mutex<Option<AbortSignal>>>,
}

impl AuthInteraction for PendingManualPrompt {
    fn signal(&self) -> Option<AbortSignal> {
        Some(self.signal.clone())
    }

    fn prompt<'a>(
        &'a self,
        _prompt: AuthPrompt,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'a, Result<String, ModelsError>> {
        if let Some(signal) = &signal {
            *self.prompt_signal.lock().unwrap() = Some(signal.clone());
        }
        let own = self.signal.clone();
        let prompt_signal = signal.unwrap_or_else(AbortSignal::never);
        Box::pin(async move {
            tokio::select! {
                _ = prompt_signal.cancelled() => {
                    Err(ModelsError::login("prompt aborted"))
                }
                _ = own.cancelled() => Err(ModelsError::login("Login cancelled")),
            }
        })
    }

    fn notify(&self, _event: AuthEvent) {}
}

impl ProviderAuthInteraction for PendingManualPrompt {
    fn provider_signal(&self) -> AbortSignal {
        self.signal.clone()
    }
}

#[tokio::test]
async fn returns_the_browser_callback_and_aborts_the_manual_prompt() {
    let server = start("Example", 0, None, None, Arc::new(|code: String| {
        Box::pin(async move { Ok(code) })
    }))
    .await
    .unwrap();
    let prompt_signal: Arc<Mutex<Option<AbortSignal>>> = Arc::new(Mutex::new(None));
    let interaction = PendingManualPrompt {
        signal: AbortSignal::never(),
        prompt_signal: Arc::clone(&prompt_signal),
    };
    let redirect_uri = server.redirect_uri.clone();
    let result = tokio::spawn(async move {
        wait_for_callback_or_manual_input(
            &interaction,
            Some(server),
            "paste",
            "placeholder",
        )
        .await
    });
    for _ in 0..50 {
        if prompt_signal.lock().unwrap().is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let _ = reqwest::Client::new()
        .get(callback_url(&redirect_uri, &[("code", "from-browser")]))
        .send()
        .await
        .unwrap();
    assert_eq!(
        result.await.unwrap().unwrap(),
        CallbackOrManual::Callback("from-browser".to_string())
    );
    assert!(prompt_signal
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .aborted());
}
