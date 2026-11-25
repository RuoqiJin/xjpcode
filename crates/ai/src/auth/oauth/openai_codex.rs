//! 对应上游 `packages/ai/src/auth/oauth/openai-codex.ts`。
//!
//! GPT 订阅（ChatGPT Plus/Pro）的登录：浏览器登录在本机 1455 端口收回调，端口被占就改成
//! 贴回链接；另有设备码登录（无头机器）。令牌里的 `chatgpt_account_id` 会随凭证存下来。
//!
//! 与上游不同的：HTTP 用 `reqwest`（上游用全局 `fetch`），随机数用 `getrandom`，
//! 取消用 crate 自己的 `AbortSignal`。

use std::sync::Arc;

use base64::Engine;

use crate::auth::oauth::callback_server::{
    start_oauth_callback_server, wait_for_callback_or_manual_input, CallbackOrManual,
    OAuthCallbackServerOptions,
};
use crate::auth::oauth::device_code::{
    poll_oauth_device_code_flow, OAuthDeviceCodePollOptions, OAuthDeviceCodePollResult,
};
use crate::auth::oauth::pkce::{generate_pkce, random_hex};
use crate::auth::types::{
    AuthPrompt, AuthPromptOption, AuthEvent, LoginOptions, ModelsError, ModelsErrorCode,
    OAuthAuth, OAuthCredential, ProviderAuthInteraction,
};
use crate::types::BoxFuture;
use crate::utils::abort::AbortSignal;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEFAULT_AUTH_BASE_URL: &str = "https://auth.openai.com";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";

/// 测试用的地址覆盖（对着本机假接口验收时用）；正式运行不设这个变量，就是 OpenAI 的地址。
fn auth_base_url() -> String {
    std::env::var("XJPCODE_OPENAI_AUTH_BASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim_end_matches('/').to_string())
        .unwrap_or_else(|| DEFAULT_AUTH_BASE_URL.to_string())
}

fn authorize_url() -> String {
    format!("{}/oauth/authorize", auth_base_url())
}

fn token_url() -> String {
    format!("{}/oauth/token", auth_base_url())
}

fn device_user_code_url() -> String {
    format!("{}/api/accounts/deviceauth/usercode", auth_base_url())
}

fn device_token_url() -> String {
    format!("{}/api/accounts/deviceauth/token", auth_base_url())
}

fn device_verification_uri() -> String {
    format!("{}/codex/device", auth_base_url())
}

fn device_redirect_uri() -> String {
    format!("{}/deviceauth/callback", auth_base_url())
}
const DEVICE_CODE_TIMEOUT_SECONDS: f64 = 15.0 * 60.0;
const OPENAI_CODEX_BROWSER_LOGIN_METHOD: &str = "browser";
const OPENAI_CODEX_DEVICE_CODE_LOGIN_METHOD: &str = "device_code";
const SCOPE: &str = "openid profile email offline_access";
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";

#[derive(Debug, Clone, PartialEq, Eq)]
struct OAuthToken {
    access: String,
    refresh: String,
    expires: i64,
}

#[derive(Debug, Clone, PartialEq)]
struct DeviceAuthInfo {
    device_auth_id: String,
    user_code: String,
    interval_seconds: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeviceTokenSuccess {
    authorization_code: String,
    code_verifier: String,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

fn get_callback_host() -> String {
    std::env::var("PI_OAUTH_CALLBACK_HOST")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

fn login_error(message: impl Into<String>) -> ModelsError {
    ModelsError::new(ModelsErrorCode::Auth, message)
}

/// pi: `parseAuthorizationInput`
///
/// 用户可能贴回整条回调链接、`code#state`、`code=…&state=…`，或者只贴授权码。
pub fn parse_authorization_input(input: &str) -> (Option<String>, Option<String>) {
    let value = input.trim();
    if value.is_empty() {
        return (None, None);
    }
    if let Ok(url) = url::Url::parse(value) {
        let code = url
            .query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned());
        let state = url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned());
        return (code, state);
    }
    if let Some((code, state)) = value.split_once('#') {
        return (Some(code.to_string()), Some(state.to_string()));
    }
    if value.contains("code=") {
        let params = url::form_urlencoded::parse(value.as_bytes())
            .into_owned()
            .collect::<std::collections::BTreeMap<_, _>>();
        return (params.get("code").cloned(), params.get("state").cloned());
    }
    (Some(value.to_string()), None)
}

fn decode_jwt_payload(token: &str) -> Option<serde_json::Value> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(payload))
        .ok()?;
    serde_json::from_slice(&decoded).ok()
}

fn get_account_id(access_token: &str) -> Option<String> {
    let payload = decode_jwt_payload(access_token)?;
    let account_id = payload
        .get(JWT_CLAIM_PATH)?
        .get("chatgpt_account_id")?
        .as_str()?;
    (!account_id.is_empty()).then(|| account_id.to_string())
}

fn credentials_from_token(token: OAuthToken) -> Result<OAuthCredential, ModelsError> {
    let account_id = get_account_id(&token.access)
        .ok_or_else(|| login_error("Failed to extract accountId from token"))?;
    let mut credential = OAuthCredential {
        access: token.access,
        refresh: token.refresh,
        expires: token.expires,
        extra: Default::default(),
    };
    credential.set_account_id(account_id);
    Ok(credential)
}

fn read_token_response(status: u16, text: &str, operation: &str) -> Result<OAuthToken, ModelsError> {
    if !(200..300).contains(&status) {
        return Err(login_error(format!(
            "OpenAI Codex token {operation} failed ({status}): {}",
            if text.is_empty() { "" } else { text }
        )));
    }
    let json: serde_json::Value = serde_json::from_str(text).map_err(|error| {
        login_error(format!(
            "OpenAI Codex token {operation} response missing fields: {error}"
        ))
    })?;
    let access = json.get("access_token").and_then(|value| value.as_str());
    let refresh = json.get("refresh_token").and_then(|value| value.as_str());
    let expires_in = json.get("expires_in").and_then(|value| value.as_f64());
    match (access, refresh, expires_in) {
        (Some(access), Some(refresh), Some(expires_in)) => Ok(OAuthToken {
            access: access.to_string(),
            refresh: refresh.to_string(),
            expires: now_ms() + (expires_in * 1000.0) as i64,
        }),
        _ => Err(login_error(format!(
            "OpenAI Codex token {operation} response missing fields: {}",
            serde_json::to_string(&json).unwrap_or_default()
        ))),
    }
}

/// 发请求，登录取消时立刻返回「Login cancelled」。
async fn send_with_cancellation(
    request: reqwest::RequestBuilder,
    signal: &AbortSignal,
) -> Result<reqwest::Response, ModelsError> {
    tokio::select! {
        response = request.send() => response.map_err(|error| {
            if signal.aborted() {
                login_error("Login cancelled")
            } else {
                login_error(error.to_string())
            }
        }),
        _ = signal.cancelled() => Err(login_error("Login cancelled")),
    }
}

async fn exchange_authorization_code(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    signal: &AbortSignal,
) -> Result<OAuthToken, ModelsError> {
    let client = reqwest::Client::new();
    let request = client
        .post(token_url())
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
        ]);
    let response = send_with_cancellation(request, signal).await?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    read_token_response(status, &text, "exchange")
}

async fn refresh_access_token(
    refresh_token: &str,
    signal: &AbortSignal,
) -> Result<OAuthToken, ModelsError> {
    let client = reqwest::Client::new();
    let request = client
        .post(token_url())
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CLIENT_ID),
        ]);
    let response = tokio::select! {
        response = request.send() => response.map_err(|error| {
            login_error(format!("OpenAI Codex token refresh error: {error}"))
        })?,
        _ = signal.cancelled() => return Err(login_error("Login cancelled")),
    };
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    read_token_response(status, &text, "refresh")
}

async fn start_device_auth(signal: &AbortSignal) -> Result<DeviceAuthInfo, ModelsError> {
    let client = reqwest::Client::new();
    let request = client
        .post(device_user_code_url())
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "client_id": CLIENT_ID }));
    let response = send_with_cancellation(request, signal).await?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        if status == 404 {
            return Err(login_error(
                "OpenAI Codex device code login is not enabled for this server. Use browser login or verify the server URL.",
            ));
        }
        let body = response.text().await.unwrap_or_default();
        return Err(login_error(format!(
            "OpenAI Codex device code request failed with status {status}{}",
            if body.is_empty() {
                String::new()
            } else {
                format!(": {body}")
            }
        )));
    }
    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|error| login_error(format!("Invalid OpenAI Codex device code response: {error}")))?;
    let device_auth_id = json.get("device_auth_id").and_then(|value| value.as_str());
    let user_code = json.get("user_code").and_then(|value| value.as_str());
    let interval_seconds = match json.get("interval") {
        Some(serde_json::Value::String(value)) => value.trim().parse::<f64>().ok(),
        Some(value) => value.as_f64(),
        None => None,
    };
    match (device_auth_id, user_code, interval_seconds) {
        (Some(device_auth_id), Some(user_code), Some(interval_seconds))
            if interval_seconds.is_finite() && interval_seconds >= 0.0 =>
        {
            Ok(DeviceAuthInfo {
                device_auth_id: device_auth_id.to_string(),
                user_code: user_code.to_string(),
                interval_seconds,
            })
        }
        _ => Err(login_error(format!(
            "Invalid OpenAI Codex device code response: {}",
            serde_json::to_string(&json).unwrap_or_default()
        ))),
    }
}

async fn poll_device_auth(
    device: &DeviceAuthInfo,
    signal: &AbortSignal,
) -> Result<DeviceTokenSuccess, ModelsError> {
    let client = reqwest::Client::new();
    let device = device.clone();
    let poll_signal = signal.clone();
    poll_oauth_device_code_flow(OAuthDeviceCodePollOptions {
        interval_seconds: Some(device.interval_seconds),
        expires_in_seconds: Some(DEVICE_CODE_TIMEOUT_SECONDS),
        wait_before_first_poll: false,
        signal: signal.clone(),
        poll: Arc::new(move || {
            let client = client.clone();
            let device = device.clone();
            let signal = poll_signal.clone();
            Box::pin(async move {
                let request = client
                    .post(device_token_url())
                    .header("Content-Type", "application/json")
                    .json(&serde_json::json!({
                        "device_auth_id": device.device_auth_id,
                        "user_code": device.user_code,
                    }));
                let response = send_with_cancellation(request, &signal).await?;
                let status = response.status().as_u16();
                if (200..300).contains(&status) {
                    let json: serde_json::Value = response.json().await.map_err(|error| {
                        login_error(format!("Invalid OpenAI Codex device auth token response: {error}"))
                    })?;
                    let authorization_code =
                        json.get("authorization_code").and_then(|value| value.as_str());
                    let code_verifier = json.get("code_verifier").and_then(|value| value.as_str());
                    return match (authorization_code, code_verifier) {
                        (Some(authorization_code), Some(code_verifier)) => {
                            Ok(OAuthDeviceCodePollResult::Complete(DeviceTokenSuccess {
                                authorization_code: authorization_code.to_string(),
                                code_verifier: code_verifier.to_string(),
                            }))
                        }
                        _ => Ok(OAuthDeviceCodePollResult::Failed {
                            message: format!(
                                "Invalid OpenAI Codex device auth token response: {}",
                                serde_json::to_string(&json).unwrap_or_default()
                            ),
                        }),
                    };
                }
                if status == 403 || status == 404 {
                    return Ok(OAuthDeviceCodePollResult::Pending);
                }
                let body = response.text().await.unwrap_or_default();
                let error_code = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|json| {
                        let error = json.get("error")?;
                        match error {
                            serde_json::Value::String(value) => Some(value.clone()),
                            serde_json::Value::Object(map) => {
                                map.get("code").and_then(|value| value.as_str()).map(str::to_string)
                            }
                            _ => None,
                        }
                    });
                match error_code.as_deref() {
                    Some("deviceauth_authorization_pending") => {
                        Ok(OAuthDeviceCodePollResult::Pending)
                    }
                    Some("slow_down") => Ok(OAuthDeviceCodePollResult::SlowDown {
                        interval_seconds: None,
                    }),
                    _ => Ok(OAuthDeviceCodePollResult::Failed {
                        message: format!(
                            "OpenAI Codex device auth failed with status {status}{}",
                            if body.is_empty() {
                                String::new()
                            } else {
                                format!(": {body}")
                            }
                        ),
                    }),
                }
            }) as BoxFuture<'static, Result<OAuthDeviceCodePollResult<DeviceTokenSuccess>, ModelsError>>
        }),
    })
    .await
}

async fn create_authorization_flow(
    originator: &str,
) -> Result<(String, String, String), ModelsError> {
    let pkce = generate_pkce()?;
    let state = random_hex(16)?;
    let mut url = url::Url::parse(&authorize_url()).expect("the authorize URL is valid");
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("response_type", "code");
        query.append_pair("client_id", CLIENT_ID);
        query.append_pair("redirect_uri", REDIRECT_URI);
        query.append_pair("scope", SCOPE);
        query.append_pair("code_challenge", &pkce.challenge);
        query.append_pair("code_challenge_method", "S256");
        query.append_pair("state", &state);
        query.append_pair("id_token_add_organizations", "true");
        query.append_pair("codex_cli_simplified_flow", "true");
        query.append_pair("originator", originator);
    }
    Ok((pkce.verifier, state, url.to_string()))
}

async fn exchange_authorization_code_for_credentials(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    signal: &AbortSignal,
) -> Result<OAuthCredential, ModelsError> {
    credentials_from_token(exchange_authorization_code(code, verifier, redirect_uri, signal).await?)
}

async fn login_device_code(
    interaction: &dyn ProviderAuthInteraction,
) -> Result<OAuthCredential, ModelsError> {
    let signal = interaction.provider_signal();
    let device = start_device_auth(&signal).await?;
    interaction.notify(AuthEvent::DeviceCode {
        user_code: device.user_code.clone(),
        verification_uri: device_verification_uri(),
        open_browser_on_confirm: true,
        copy_code_on_confirm: true,
        interval_seconds: Some(device.interval_seconds as u64),
        expires_in_seconds: Some(DEVICE_CODE_TIMEOUT_SECONDS as u64),
    });
    let code = poll_device_auth(&device, &signal).await?;
    exchange_authorization_code_for_credentials(
        &code.authorization_code,
        &code.code_verifier,
        &device_redirect_uri(),
        &signal,
    )
    .await
}

async fn login_browser(
    interaction: &dyn ProviderAuthInteraction,
    options: Option<LoginOptions>,
) -> Result<OAuthCredential, ModelsError> {
    let signal = interaction.provider_signal();
    let originator = options
        .as_ref()
        .map(|options| options.agent_name_or_default().to_string())
        .unwrap_or_else(|| "pi".to_string());
    let (verifier, state, url) = create_authorization_flow(&originator).await?;
    // 1455 端口和 Codex CLI 共用；被占了就退回贴链接。
    let callback = start_oauth_callback_server(OAuthCallbackServerOptions {
        provider_name: "OpenAI".to_string(),
        host: get_callback_host(),
        port: 1455,
        path: "/auth/callback".to_string(),
        redirect_host: None,
        state: Some(state.clone()),
        complete: Arc::new(|code: String| {
            Box::pin(async move { Ok(code) })
                as BoxFuture<'static, Result<String, ModelsError>>
        }),
        signal: Some(signal.clone()),
        timeout_ms: None,
    })
    .await
    .ok();

    interaction.notify(AuthEvent::AuthUrl {
        url,
        instructions: Some("A browser window should open. Complete login to finish.".to_string()),
    });

    let result = wait_for_callback_or_manual_input(
        interaction,
        callback,
        "Complete login in your browser, or paste the authorization code / redirect URL here:",
        REDIRECT_URI,
    )
    .await?;
    let code = match result {
        CallbackOrManual::Callback(code) => code,
        CallbackOrManual::Manual(input) => {
            let (code, returned_state) = parse_authorization_input(&input);
            if let Some(returned_state) = returned_state {
                if returned_state != state {
                    return Err(login_error("State mismatch"));
                }
            }
            code.ok_or_else(|| login_error("Missing authorization code"))?
        }
    };
    exchange_authorization_code_for_credentials(&code, &verifier, REDIRECT_URI, &signal).await
}

/// pi: `openaiCodexOAuth`
pub fn openai_codex_oauth() -> OAuthAuth {
    OAuthAuth {
        name: "OpenAI (ChatGPT Plus/Pro)".to_string(),
        is_subscription: true,
        login_label: None,
        login: Arc::new(|interaction: Arc<dyn ProviderAuthInteraction>, options: Option<LoginOptions>| {
            Box::pin(async move {
                let method = interaction
                    .prompt(
                        AuthPrompt::Select {
                            message: "Select OpenAI Codex login method:".to_string(),
                            options: vec![
                                AuthPromptOption {
                                    id: OPENAI_CODEX_BROWSER_LOGIN_METHOD.to_string(),
                                    label: "Browser login (default)".to_string(),
                                    description: None,
                                },
                                AuthPromptOption {
                                    id: OPENAI_CODEX_DEVICE_CODE_LOGIN_METHOD.to_string(),
                                    label: "Device code login (headless)".to_string(),
                                    description: None,
                                },
                            ],
                        },
                        None,
                    )
                    .await?;
                match method.as_str() {
                    OPENAI_CODEX_DEVICE_CODE_LOGIN_METHOD => login_device_code(interaction.as_ref()).await,
                    OPENAI_CODEX_BROWSER_LOGIN_METHOD => login_browser(interaction.as_ref(), options).await,
                    other => Err(login_error(format!(
                        "Unknown OpenAI Codex login method: {other}"
                    ))),
                }
            }) as BoxFuture<'static, Result<OAuthCredential, ModelsError>>
        }),
        refresh: Arc::new(
            |credential: OAuthCredential, signal: AbortSignal| {
                Box::pin(async move {
                    credentials_from_token(
                        refresh_access_token(&credential.refresh, &signal).await?,
                    )
                }) as BoxFuture<'static, Result<OAuthCredential, ModelsError>>
            },
        ),
        to_auth: Arc::new(|credential: OAuthCredential| {
            Box::pin(async move {
                Ok(crate::auth::types::ModelAuth {
                    api_key: Some(credential.access),
                    ..Default::default()
                })
            }) as BoxFuture<'static, Result<crate::auth::types::ModelAuth, ModelsError>>
        }),
    }
}
