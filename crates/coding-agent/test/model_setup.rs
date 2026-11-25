//! `ModelSetup` 的登记、缺省值、钥匙解析与请求分派。
//!
//! pi 的对应实现在 `core/model-runtime.ts` / `core/model-resolver.ts`（这一轮没整份搬，
//! 见 `PORTING.md` 对账表）；这里测的是 xjpcode 自己装配的那一层。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use xjpcode_ai::auth::{
    ApiKeyCredential, AuthEvent, AuthInteraction, AuthOperationOptions, AuthPrompt, AuthType,
    Credential, CredentialStore, InMemoryCredentialStore, ModelsError, OAuthCredential,
};
use xjpcode_ai::providers::model_schema::ModelThinkingLevel;
use xjpcode_ai::types::BoxFuture;
use xjpcode_ai::{AbortSignal, AssistantMessageEvent, StopReason};
use xjpcode_coding_agent::core::model_setup::{
    build_models, resolve_model, ModelSetup, DEEPSEEK_PROVIDER, OPENAI_CODEX_PROVIDER,
};
use xjpcode_coding_agent::core::settings_manager::Settings;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn in_memory() -> Arc<dyn CredentialStore> {
    Arc::new(InMemoryCredentialStore::new())
}

fn codex_credential() -> Credential {
    Credential::OAuth(OAuthCredential {
        access: "codex-access".to_string(),
        refresh: "codex-refresh".to_string(),
        expires: 4_102_444_800_000,
        extra: BTreeMap::from([(
            "accountId".to_string(),
            serde_json::json!("acc_test"),
        )]),
    })
}

/// 登录时把 `answer` 当作唯一一次提问的回答。
struct AnswerInteraction {
    answer: String,
    events: Mutex<Vec<AuthEvent>>,
}

impl AuthInteraction for AnswerInteraction {
    fn signal(&self) -> Option<AbortSignal> {
        Some(AbortSignal::never())
    }

    fn prompt<'a>(
        &'a self,
        _prompt: AuthPrompt,
        _signal: Option<AbortSignal>,
    ) -> BoxFuture<'a, Result<String, ModelsError>> {
        Box::pin(async move { Ok(self.answer.clone()) })
    }

    fn notify(&self, event: AuthEvent) {
        self.events.lock().unwrap().push(event);
    }
}

#[test]
fn registers_the_two_providers_and_the_installed_codex_catalog() {
    let models = build_models(in_memory());
    let ids: Vec<String> = models
        .get_models(None)
        .into_iter()
        .map(|model| format!("{}/{}", model.provider, model.id))
        .collect();
    assert_eq!(ids.len(), 12);
    assert!(ids.contains(&"deepseek/deepseek-flash".to_string()));
    assert!(ids.contains(&"deepseek/deepseek-v4-pro".to_string()));
    assert!(ids.contains(&"openai-codex/gpt-6.2-astra".to_string()));
    assert_eq!(models.get_models(Some(OPENAI_CODEX_PROVIDER)).len(), 10);
    assert!(models
        .get_model(OPENAI_CODEX_PROVIDER, "gpt-6.2-astra")
        .is_some());
}

#[test]
fn the_default_model_stays_deepseek_flash_and_settings_can_override_it() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let setup = ModelSetup::load(None, in_memory(), &Settings::in_memory()).unwrap();
    assert_eq!(setup.model().provider, DEEPSEEK_PROVIDER);
    assert_eq!(setup.model().id, "deepseek-flash");
    assert!(!setup.model_explicit());

    let mut settings = Settings::in_memory();
    settings.default_provider = Some(DEEPSEEK_PROVIDER.to_string());
    settings.default_model = Some("deepseek-v4-pro".to_string());
    let setup = ModelSetup::load(None, in_memory(), &settings).unwrap();
    assert_eq!(setup.model().id, "deepseek-v4-pro");
    // 设置里的模型不在登记表里就退回缺省。
    settings.default_provider = Some("some-other-provider".to_string());
    settings.default_model = Some("their-model".to_string());
    let setup = ModelSetup::load(None, in_memory(), &settings).unwrap();
    assert_eq!(setup.model().id, "deepseek-flash");
}

#[test]
fn thinking_defaults_follow_pi() {
    let settings = Settings::in_memory();
    let setup = ModelSetup::load(None, in_memory(), &settings).unwrap();
    assert_eq!(
        setup.default_thinking_level(&settings),
        ModelThinkingLevel::Medium
    );
    let mut settings = Settings::in_memory();
    settings.default_thinking_level = Some("max".to_string());
    assert_eq!(
        setup.default_thinking_level(&settings),
        ModelThinkingLevel::Max
    );
}

#[test]
fn resolves_provider_slash_id_and_bare_ids() {
    let models = build_models(in_memory());
    assert_eq!(
        resolve_model(&models, "openai-codex/gpt-6.2-astra")
            .unwrap()
            .id,
        "gpt-6.2-astra"
    );
    assert_eq!(resolve_model(&models, "deepseek-v4-pro").unwrap().provider, DEEPSEEK_PROVIDER);
    let error = resolve_model(&models, "nope").unwrap_err();
    assert!(error.contains("unknown model nope"));
    assert!(error.contains("deepseek/deepseek-flash"));
    let error = resolve_model(&models, "nope/gpt").unwrap_err();
    assert!(error.contains("unknown model nope/gpt"));
}

#[tokio::test]
async fn resolves_the_deepseek_key_from_the_environment() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    std::env::set_var("DEEPSEEK_API_KEY", "env-key");
    let setup = ModelSetup::load(None, in_memory(), &Settings::in_memory()).unwrap();
    let key = (setup.get_api_key())("deepseek").await;
    assert_eq!(key.as_deref(), Some("env-key"));
    let check = setup.models().check_auth(DEEPSEEK_PROVIDER).await.unwrap();
    assert_eq!(check.unwrap().r#type, AuthType::ApiKey);
    std::env::remove_var("DEEPSEEK_API_KEY");
}

#[tokio::test]
async fn codex_oauth_credentials_drive_auth_and_availability() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    std::env::remove_var("DEEPSEEK_API_KEY");
    let store = InMemoryCredentialStore::new();
    store
        .modify(
            OPENAI_CODEX_PROVIDER,
            Box::new(|_| {
                Box::pin(async { Ok(Some(codex_credential())) })
            }),
            AuthOperationOptions::default(),
        )
        .await
        .unwrap();
    let setup = ModelSetup::load(
        Some("openai-codex/gpt-6.2-astra"),
        Arc::new(store),
        &Settings::in_memory(),
    )
    .unwrap();
    assert_eq!(
        (setup.get_api_key())(OPENAI_CODEX_PROVIDER).await.as_deref(),
        Some("codex-access")
    );
    assert!(setup.models().is_using_subscription(OPENAI_CODEX_PROVIDER).await);
    let available = setup.models().get_available(None).await.unwrap();
    assert_eq!(available.len(), 10);
    assert!(available.iter().all(|model| model.provider == OPENAI_CODEX_PROVIDER));
}

#[tokio::test]
async fn logging_in_stores_the_key_and_makes_it_effective() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    std::env::remove_var("DEEPSEEK_API_KEY");
    let store = Arc::new(InMemoryCredentialStore::new());
    let setup = ModelSetup::load(
        None,
        Arc::clone(&store) as Arc<dyn CredentialStore>,
        &Settings::in_memory(),
    )
    .unwrap();
    assert_eq!((setup.get_api_key())("deepseek").await, None);

    let interaction = Arc::new(AnswerInteraction {
        answer: "sk-from-login".to_string(),
        events: Mutex::new(Vec::new()),
    });
    let credential = setup
        .models()
        .login(DEEPSEEK_PROVIDER, AuthType::ApiKey, interaction, None)
        .await
        .unwrap();
    match &credential {
        Credential::ApiKey(api_key) => assert_eq!(api_key.key.as_deref(), Some("sk-from-login")),
        other => panic!("unexpected credential {other:?}"),
    }
    assert_eq!(
        (setup.get_api_key())("deepseek").await.as_deref(),
        Some("sk-from-login")
    );
    let stored = store.peek(DEEPSEEK_PROVIDER).unwrap();
    match stored {
        Credential::ApiKey(ApiKeyCredential { key, .. }) => {
            assert_eq!(key.as_deref(), Some("sk-from-login"))
        }
        other => panic!("unexpected stored credential {other:?}"),
    }

    setup.models().logout(DEEPSEEK_PROVIDER).await.unwrap();
    assert_eq!((setup.get_api_key())("deepseek").await, None);
}

#[tokio::test]
async fn the_stream_function_reports_which_credential_problem_it_hit() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    std::env::remove_var("DEEPSEEK_API_KEY");
    let setup = ModelSetup::load(None, in_memory(), &Settings::in_memory()).unwrap();
    let model = setup.model().clone();
    let stream = (setup.stream_fn())(
        model,
        xjpcode_ai::TranscriptContext { messages: vec![] },
        xjpcode_agent::StreamFnOptions {
            options: Default::default(),
            signal: None,
        },
    );
    let events = stream.consumer();
    let mut events = events;
    let first = tokio::time::timeout(std::time::Duration::from_secs(1), events.next())
        .await
        .unwrap()
        .unwrap();
    match first {
        AssistantMessageEvent::Error { error, .. } => {
            assert_eq!(error.stop_reason, StopReason::Error);
            assert!(error
                .error_message
                .as_deref()
                .unwrap_or_default()
                .contains("No credentials for deepseek"));
        }
        other => panic!("unexpected first event {other:?}"),
    }
}

#[test]
fn provider_labels_come_from_the_registry() {
    let models = build_models(in_memory());
    assert_eq!(
        xjpcode_coding_agent::core::model_setup::provider_label(&models, DEEPSEEK_PROVIDER),
        "DeepSeek"
    );
    assert_eq!(
        xjpcode_coding_agent::core::model_setup::provider_label(&models, OPENAI_CODEX_PROVIDER),
        "OpenAI Codex"
    );
}

#[test]
fn restoring_a_session_redeclares_tools_and_the_changed_prompt_sections() {
    use xjpcode_ai::{SystemContent, SystemMessage, SystemRole};
    use xjpcode_coding_agent::core::model_setup::restored_prompt_message;
    use xjpcode_coding_agent::core::system_prompt::BuildSystemPromptOptions;
    use xjpcode_coding_agent::core::tools::read::create_read_tool;
    use xjpcode_coding_agent::ReadToolOptions;

    let tools: Vec<Arc<dyn xjpcode_agent::AgentTool>> = vec![Arc::new(create_read_tool(
        &std::path::PathBuf::from("/work"),
        ReadToolOptions::default(),
    ))];
    let declaration = tools[0].declaration();
    let recorded = xjpcode_agent::AgentMessage::System(SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text(String::new()),
        sections: Some(
            [("cwd".to_string(), Some("/work".to_string()))]
                .into_iter()
                .collect(),
        ),
        tools_added: Some(vec![declaration.clone()]),
        tools_removed: None,
        timestamp: 1,
    });
    let prompt_options = BuildSystemPromptOptions {
        cwd: "/work".to_string(),
        selected_tools: Some(vec!["read".to_string()]),
        ..Default::default()
    };
    let patch = restored_prompt_message(&[recorded], &tools, &prompt_options)
        .expect("a restored session with declared tools gets a declaration");
    assert_eq!(
        patch
            .tools_added
            .as_ref()
            .map(|tools| tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>()),
        Some(vec!["read"])
    );
    // 记录里只有 cwd，当前资源重算后还有别的段落，所以差分里会出现新段落。
    assert!(patch
        .sections
        .as_ref()
        .is_some_and(|sections| !sections.is_empty()));

    // 记录里没声明过工具：不补。
    let no_tools = xjpcode_agent::AgentMessage::System(SystemMessage {
        role: SystemRole::System,
        content: SystemContent::Text("hello".to_string()),
        sections: None,
        tools_added: None,
        tools_removed: None,
        timestamp: 1,
    });
    assert!(restored_prompt_message(&[no_tools], &tools, &prompt_options).is_none());
}
