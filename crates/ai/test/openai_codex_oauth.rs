//! 对应上游 `packages/ai/test/openai-codex-oauth.test.ts` 的 HTTP 部分。
//!
//! 上游用 `vi.stubGlobal("fetch")` 换成假的 fetch；这里起一台本机 HTTP 服务，并把
//! `XJPCODE_OPENAI_AUTH_BASE_URL` 指过去（这是测试钩子，正式运行不设）。
//! 覆盖：设备码登录、令牌续期、授权码输入的各种形状、未知登录方式。

use std::sync::{Arc, Mutex};

use base64::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use xjpcode_ai::auth::oauth::openai_codex::{openai_codex_oauth, parse_authorization_input};
use xjpcode_ai::auth::{
    AuthEvent, AuthInteraction, AuthPrompt, ModelsError, OAuthCredential, ProviderAuthInteraction,
};
use xjpcode_ai::types::BoxFuture;
use xjpcode_ai::{AbortController, AbortSignal};

/// 同一份进程环境变量 + 固定端口：这些测试排着跑。
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn access_token(account_id: &str) -> String {
    let header = base64::engine::general_purpose::STANDARD.encode("{\"alg\":\"none\"}");
    let payload = base64::engine::general_purpose::STANDARD.encode(
        serde_json::json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": account_id }
        })
        .to_string(),
    );
    format!("{header}.{payload}.signature")
}

#[derive(Debug, Clone, Default)]
struct Recorded {
    path: String,
    body: String,
}

struct FakeAuthServer {
    base_url: String,
    controller: AbortController,
}

impl FakeAuthServer {
    async fn start(respond: impl Fn(&Recorded, usize) -> (u16, String) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let records = Arc::new(Mutex::new(Vec::new()));
        let records_for_task = Arc::clone(&records);
        let controller = AbortController::new();
        let shutdown = controller.signal();
        let respond = Arc::new(respond);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else { break };
                        let records = Arc::clone(&records_for_task);
                        let respond = Arc::clone(&respond);
                        tokio::spawn(async move {
                            let mut buffer = Vec::new();
                            let mut chunk = [0u8; 4096];
                            loop {
                                if buffer.windows(4).any(|w| w == b"\r\n\r\n") { break; }
                                let read = stream.read(&mut chunk).await.unwrap_or(0);
                                if read == 0 { break; }
                                buffer.extend_from_slice(&chunk[..read]);
                            }
                            let text = String::from_utf8_lossy(&buffer).to_string();
                            let mut parts = text.lines().next().unwrap_or("").split_whitespace();
                            let _method = parts.next().unwrap_or("");
                            let path = parts.next().unwrap_or("").to_string();
                            let headers_len = text.find("\r\n\r\n").map(|i| i + 4).unwrap_or(text.len());
                            let content_length = text
                                .lines()
                                .find_map(|line| {
                                    let lower = line.to_ascii_lowercase();
                                    lower.strip_prefix("content-length:").map(|value| value.trim().parse::<usize>().unwrap_or(0))
                                })
                                .unwrap_or(0);
                            let mut body = buffer[headers_len.min(buffer.len())..].to_vec();
                            while body.len() < content_length {
                                let read = stream.read(&mut chunk).await.unwrap_or(0);
                                if read == 0 { break; }
                                body.extend_from_slice(&chunk[..read]);
                            }
                            let record = Recorded { path, body: String::from_utf8_lossy(&body).to_string() };
                            let index = {
                                let mut records = records.lock().unwrap();
                                records.push(record.clone());
                                records.len() - 1
                            };
                            let (status, body) = respond(&record, index);
                            let response = format!(
                                "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = stream.write_all(response.as_bytes()).await;
                        });
                    }
                }
            }
        });
        Self {
            base_url: format!("http://127.0.0.1:{port}"),
            controller,
        }
    }

}

impl Drop for FakeAuthServer {
    fn drop(&mut self) {
        self.controller.abort();
    }
}

/// 按剧本回答的设备码交互：第一次选设备码，记下设备码事件。
struct DeviceLoginInteraction {
    events: Mutex<Vec<AuthEvent>>,
}

impl AuthInteraction for DeviceLoginInteraction {
    fn signal(&self) -> Option<AbortSignal> {
        Some(AbortSignal::never())
    }

    fn prompt<'a>(
        &'a self,
        prompt: AuthPrompt,
        _signal: Option<AbortSignal>,
    ) -> BoxFuture<'a, Result<String, ModelsError>> {
        Box::pin(async move {
            match prompt {
                AuthPrompt::Select { options, .. } => Ok(options
                    .iter()
                    .find(|option| option.id == "device_code")
                    .expect("device code option is offered")
                    .id
                    .clone()),
                other => Err(ModelsError::login(format!(
                    "Unexpected prompt: {}",
                    other.message()
                ))),
            }
        })
    }

    fn notify(&self, event: AuthEvent) {
        self.events.lock().unwrap().push(event);
    }
}

impl ProviderAuthInteraction for DeviceLoginInteraction {
    fn provider_signal(&self) -> AbortSignal {
        AbortSignal::never()
    }
}

#[tokio::test]
async fn logs_in_with_the_device_code_flow_and_keeps_the_account_id() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let token = access_token("account-123");
    let token_for_server = token.clone();
    let server = FakeAuthServer::start(move |record, index| {
        if record.path.ends_with("/api/accounts/deviceauth/usercode") {
            assert!(record.body.contains("\"client_id\":\"app_EMoamEEZ73f0CkXaXp7hrann\""));
            (
                200,
                serde_json::json!({
                    "device_auth_id": "device-auth-id",
                    "user_code": "ABCD-1234",
                    "interval": "1",
                })
                .to_string(),
            )
        } else if record.path.ends_with("/api/accounts/deviceauth/token") {
            assert!(record.body.contains("device-auth-id"));
            assert!(record.body.contains("ABCD-1234"));
            if index <= 1 {
                (
                    403,
                    serde_json::json!({
                        "error": {
                            "message": "Device authorization is pending. Please try again.",
                            "code": "deviceauth_authorization_pending",
                        }
                    })
                    .to_string(),
                )
            } else {
                (
                    200,
                    serde_json::json!({
                        "authorization_code": "oauth-code",
                        "code_challenge": "device-code-challenge",
                        "code_verifier": "device-code-verifier",
                    })
                    .to_string(),
                )
            }
        } else if record.path.ends_with("/oauth/token") {
            assert!(record.body.contains("grant_type=authorization_code"));
            assert!(record.body.contains("redirect_uri="));
            assert!(record.body.contains("%2Fdeviceauth%2Fcallback"));
            assert!(record.body.contains("code_verifier=device-code-verifier"));
            (
                200,
                serde_json::json!({
                    "access_token": token_for_server,
                    "refresh_token": "refresh-token",
                    "expires_in": 3600,
                })
                .to_string(),
            )
        } else {
            (404, "{}".to_string())
        }
    })
    .await;
    std::env::set_var("XJPCODE_OPENAI_AUTH_BASE_URL", &server.base_url);

    let oauth = openai_codex_oauth();
    let login_interaction = Arc::new(DeviceLoginInteraction {
        events: Mutex::new(Vec::new()),
    });
    let credential = (oauth.login)(Arc::clone(&login_interaction) as Arc<dyn ProviderAuthInteraction>, None)
        .await
        .unwrap();
    assert_eq!(credential.access, token);
    assert_eq!(credential.refresh, "refresh-token");
    assert_eq!(credential.account_id(), Some("account-123"));
    assert!(credential.expires > 0);
    let events = login_interaction.events.lock().unwrap();
    assert!(matches!(
        &events[0],
        AuthEvent::DeviceCode {
            user_code,
            verification_uri,
            interval_seconds: Some(1),
            expires_in_seconds: Some(900),
            ..
        } if user_code == "ABCD-1234"
            && verification_uri.ends_with("/codex/device")
    ));
    drop(events);

    std::env::remove_var("XJPCODE_OPENAI_AUTH_BASE_URL");
}

#[tokio::test]
async fn refreshes_the_token_and_extracts_the_new_account_id() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let token = access_token("account-456");
    let token_for_server = token.clone();
    let server = FakeAuthServer::start(move |record, _| {
        assert!(record.path.ends_with("/oauth/token"));
        assert!(record.body.contains("grant_type=refresh_token"));
        assert!(record.body.contains("refresh_token=old-refresh"));
        (
            200,
            serde_json::json!({
                "access_token": token_for_server,
                "refresh_token": "new-refresh",
                "expires_in": 60,
            })
            .to_string(),
        )
    })
    .await;
    std::env::set_var("XJPCODE_OPENAI_AUTH_BASE_URL", &server.base_url);
    let oauth = openai_codex_oauth();
    let credential = OAuthCredential {
        access: "old-access".to_string(),
        refresh: "old-refresh".to_string(),
        expires: 0,
        extra: Default::default(),
    };
    let refreshed = (oauth.refresh)(credential, AbortSignal::never()).await.unwrap();
    assert_eq!(refreshed.access, token);
    assert_eq!(refreshed.refresh, "new-refresh");
    assert_eq!(refreshed.account_id(), Some("account-456"));
    std::env::remove_var("XJPCODE_OPENAI_AUTH_BASE_URL");
}

#[tokio::test]
async fn reports_token_exchange_failures_with_status_and_body() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let server = FakeAuthServer::start(|_, _| {
        (400, serde_json::json!({"error": "invalid_grant"}).to_string())
    })
    .await;
    std::env::set_var("XJPCODE_OPENAI_AUTH_BASE_URL", &server.base_url);
    let oauth = openai_codex_oauth();
    let credential = OAuthCredential {
        access: "old".to_string(),
        refresh: "bad".to_string(),
        expires: 0,
        extra: Default::default(),
    };
    let error = (oauth.refresh)(credential, AbortSignal::never())
        .await
        .unwrap_err();
    assert!(error.message.contains("OpenAI Codex token refresh failed (400)"));
    assert!(error.message.contains("invalid_grant"));
    std::env::remove_var("XJPCODE_OPENAI_AUTH_BASE_URL");
}

#[test]
fn parses_pasted_authorization_input_in_all_its_shapes() {
    let (code, state) = parse_authorization_input(
        "http://localhost:1455/auth/callback?code=abc&state=def",
    );
    assert_eq!(code.as_deref(), Some("abc"));
    assert_eq!(state.as_deref(), Some("def"));
    let (code, state) = parse_authorization_input("abc#def");
    assert_eq!(code.as_deref(), Some("abc"));
    assert_eq!(state.as_deref(), Some("def"));
    let (code, state) = parse_authorization_input("code=abc&state=def");
    assert_eq!(code.as_deref(), Some("abc"));
    assert_eq!(state.as_deref(), Some("def"));
    let (code, state) = parse_authorization_input("just-a-code");
    assert_eq!(code.as_deref(), Some("just-a-code"));
    assert_eq!(state, None);
    assert_eq!(parse_authorization_input("   "), (None, None));
}
