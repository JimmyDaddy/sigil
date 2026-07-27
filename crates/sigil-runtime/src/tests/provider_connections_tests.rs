use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::anyhow;
use async_trait::async_trait;
use serde_json::json;
use sigil_kernel::{CONFIG_VERSION_V2, ConnectionId, ModelRef, RootConfig, SecretString};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

#[derive(Clone, Default)]
struct FakeCredentialStore {
    records: Arc<Mutex<BTreeMap<CredentialId, ProviderCredentialRecord>>>,
    fail_load: Arc<Mutex<bool>>,
    fail_delete: Arc<Mutex<bool>>,
    fail_store_before_write: Arc<Mutex<bool>>,
    fail_store_after_write: Arc<Mutex<bool>>,
    fail_store_on_call: Arc<Mutex<Option<usize>>>,
    store_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl ProviderCredentialStore for FakeCredentialStore {
    async fn load(
        &self,
        credential_id: &CredentialId,
    ) -> Result<Option<ProviderCredentialRecord>, ProviderCredentialError> {
        if *self.fail_load.lock().expect("load flag lock") {
            return Err(ProviderCredentialError::new(
                ProviderCredentialErrorCode::CredentialStoreUnavailable,
                "injected load failure",
            ));
        }
        Ok(self
            .records
            .lock()
            .expect("records lock")
            .get(credential_id)
            .cloned())
    }

    async fn store(
        &self,
        record: &ProviderCredentialRecord,
    ) -> Result<(), ProviderCredentialError> {
        let call = self.store_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if *self
            .fail_store_before_write
            .lock()
            .expect("pre-write store failure flag lock")
        {
            return Err(ProviderCredentialError::new(
                ProviderCredentialErrorCode::CredentialStoreUnavailable,
                "injected pre-write store failure",
            ));
        }
        self.records
            .lock()
            .expect("records lock")
            .insert(record.credential_id.clone(), record.clone());
        if *self
            .fail_store_after_write
            .lock()
            .expect("store failure flag lock")
            || self
                .fail_store_on_call
                .lock()
                .expect("store call failure lock")
                .is_some_and(|target| target == call)
        {
            return Err(ProviderCredentialError::new(
                ProviderCredentialErrorCode::CredentialStoreRejected,
                "injected post-write store failure",
            ));
        }
        Ok(())
    }

    async fn delete(&self, credential_id: &CredentialId) -> Result<bool, ProviderCredentialError> {
        if *self.fail_delete.lock().expect("delete flag lock") {
            return Err(ProviderCredentialError::new(
                ProviderCredentialErrorCode::CredentialStoreRejected,
                "injected delete failure",
            ));
        }
        Ok(self
            .records
            .lock()
            .expect("records lock")
            .remove(credential_id)
            .is_some())
    }
}

#[derive(Default)]
struct MapEnvironment(BTreeMap<String, String>);

impl CredentialEnvironment for MapEnvironment {
    fn read(&self, name: &str) -> Option<SecretString> {
        self.0.get(name).cloned().map(SecretString::new)
    }
}

#[derive(Clone)]
struct FakePublisher {
    outcome: Result<ConfigPublishOutcome, &'static str>,
    published: Arc<Mutex<Option<RootConfig>>>,
}

impl FakePublisher {
    fn published(outcome: ConfigPublishOutcome) -> Self {
        Self {
            outcome: Ok(outcome),
            published: Arc::new(Mutex::new(None)),
        }
    }

    fn failed() -> Self {
        Self {
            outcome: Err("injected publish failure"),
            published: Arc::new(Mutex::new(None)),
        }
    }
}

impl ProviderConfigPublisher for FakePublisher {
    fn publish(
        &self,
        _path: &Path,
        config: &RootConfig,
        _lock: &sigil_kernel::ConfigUpdateLockGuard,
    ) -> Result<ConfigPublishOutcome, anyhow::Error> {
        if self.outcome.is_ok() {
            *self.published.lock().expect("published lock") = Some(config.clone());
        }
        self.outcome.clone().map_err(|message| anyhow!(message))
    }
}

struct AlternateConfigPublisher {
    replacement: RootConfig,
}

impl ProviderConfigPublisher for AlternateConfigPublisher {
    fn publish(
        &self,
        path: &Path,
        _config: &RootConfig,
        _lock: &sigil_kernel::ConfigUpdateLockGuard,
    ) -> Result<ConfigPublishOutcome, anyhow::Error> {
        std::fs::write(path, toml::to_string_pretty(&self.replacement)?)?;
        Ok(ConfigPublishOutcome::PublishedVisibilityUncertain {
            recovery_path: None,
        })
    }
}

fn legacy_root(provider: &str, model: &str, body: &str) -> RootConfig {
    toml::from_str(&format!(
        r#"
[agent]
provider = "{provider}"
model = "{model}"

[providers.{provider}]
{body}
"#
    ))
    .expect("legacy config should parse")
}

fn unused_config_path() -> PathBuf {
    std::env::temp_dir().join(format!("sigil-provider-test-{}.toml", uuid::Uuid::new_v4()))
}

fn deepseek_connection() -> ProviderConnectionConfig {
    provider_connection_template(
        ProviderFamily::DeepSeek,
        ProviderProtocol::DeepSeek,
        ConnectionId::new("deepseek-default").expect("connection id"),
        "DeepSeek",
    )
    .expect("connection template")
    .0
}

async fn spawn_catalog_server(
    status: u16,
    body: &'static str,
    delay: Duration,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("catalog listener");
    let address = listener.local_addr().expect("catalog address");
    let count = Arc::new(AtomicUsize::new(0));
    let count_for_task = Arc::clone(&count);
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            count_for_task.fetch_add(1, Ordering::SeqCst);
            let mut request = vec![0_u8; 8192];
            let _ = stream.read(&mut request).await;
            tokio::time::sleep(delay).await;
            let reason = match status {
                200 => "OK",
                401 => "Unauthorized",
                404 => "Not Found",
                429 => "Too Many Requests",
                _ => "Error",
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    (format!("http://{address}/v1"), count, task)
}

async fn spawn_catalog_sequence_server(
    responses: Vec<(u16, &'static str, Duration)>,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("catalog sequence listener");
    let address = listener.local_addr().expect("catalog sequence address");
    let count = Arc::new(AtomicUsize::new(0));
    let count_for_task = Arc::clone(&count);
    let task = tokio::spawn(async move {
        for (status, body, delay) in responses {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            count_for_task.fetch_add(1, Ordering::SeqCst);
            let mut request = vec![0_u8; 8192];
            let _ = stream.read(&mut request).await;
            tokio::time::sleep(delay).await;
            let reason = match status {
                200 => "OK",
                401 => "Unauthorized",
                404 => "Not Found",
                429 => "Too Many Requests",
                _ => "Error",
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    (format!("http://{address}/v1"), count, task)
}

fn local_catalog_root(base_url: String, configured_model: &str) -> RootConfig {
    let mut connection = provider_connection_template(
        ProviderFamily::Custom,
        ProviderProtocol::OpenAiChatCompletions,
        ConnectionId::new("local").expect("connection id"),
        "Local",
    )
    .expect("connection template")
    .0;
    connection.base_url = base_url;
    let endpoint = url::Url::parse(&connection.base_url).expect("test endpoint URL");
    let loopback_http = endpoint.scheme() == "http"
        && endpoint
            .host_str()
            .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "::1"));
    connection.credential = if loopback_http {
        CredentialRefConfig::None
    } else {
        CredentialRefConfig::Environment {
            name: "SIGIL_OPENAI_COMPATIBLE_API_KEY".to_owned(),
        }
    };
    materialize_v2_root_config(
        &legacy_root(
            "openai_compat",
            configured_model,
            r#"base_url = "https://placeholder.invalid/v1""#,
        ),
        &BTreeMap::from([(connection.id.clone(), connection)]),
        &ModelRef::new(
            ConnectionId::new("local").expect("connection id"),
            configured_model,
        )
        .expect("model ref"),
    )
    .expect("local V2 config")
}

#[test]
fn recent_models_keep_compound_identity_order_limit_and_filter_deleted_connections() {
    let temp = tempfile::tempdir().expect("temporary state root");
    let root = local_catalog_root("http://127.0.0.1:11434/v1".to_owned(), "configured-model");
    for index in 0..25 {
        record_recent_model_ref(
            temp.path(),
            &root,
            &ModelRef::new(
                ConnectionId::new("local").expect("connection id"),
                format!("model-{index:02}"),
            )
            .expect("model ref"),
        )
        .expect("recent model should publish");
    }
    let models = load_recent_model_refs(temp.path(), &root);
    assert_eq!(models.len(), 20);
    assert_eq!(models[0].connection_id.as_str(), "local");
    assert_eq!(models[0].model_id, "model-24");
    assert_eq!(models[19].model_id, "model-05");

    let loaded = load_provider_connections(&root);
    let local = loaded
        .connections
        .get(&ConnectionId::new("local").expect("connection id"))
        .expect("local connection")
        .config
        .clone();
    let mut removed_later = local.clone();
    removed_later.id = ConnectionId::new("removed-later").expect("connection id");
    removed_later.label = "Removed later".to_owned();
    let expanded = materialize_v2_root_config(
        &root,
        &BTreeMap::from([
            (local.id.clone(), local),
            (removed_later.id.clone(), removed_later.clone()),
        ]),
        &ModelRef::new(removed_later.id.clone(), "temporary-model").expect("temporary model ref"),
    )
    .expect("expanded config");
    record_recent_model_ref(
        temp.path(),
        &expanded,
        &ModelRef::new(removed_later.id, "temporary-model").expect("temporary model ref"),
    )
    .expect("temporary connection recent should publish");

    let filtered = load_recent_model_refs(temp.path(), &root);
    assert!(
        filtered
            .iter()
            .all(|model_ref| model_ref.connection_id.as_str() == "local")
    );
}

#[test]
fn recent_models_reject_malformed_or_unsafe_state_and_use_private_permissions() {
    let temp = tempfile::tempdir().expect("temporary state root");
    let root = local_catalog_root("http://127.0.0.1:11434/v1".to_owned(), "configured-model");
    let model_ref = ModelRef::new(
        ConnectionId::new("local").expect("connection id"),
        "configured-model",
    )
    .expect("model ref");
    record_recent_model_ref(temp.path(), &root, &model_ref).expect("recent model should publish");
    let path = recent_models_path(temp.path());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            std::fs::metadata(&path)
                .expect("recent file metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().expect("recent parent"))
                .expect("recent parent metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    std::fs::write(&path, b"{malformed").expect("malformed state fixture");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("restore private mode");
    }
    assert!(load_recent_model_refs(temp.path(), &root).is_empty());

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        std::fs::remove_file(&path).expect("remove malformed state");
        let outside = temp.path().join("outside.json");
        std::fs::write(&outside, b"outside").expect("outside fixture");
        symlink(&outside, &path).expect("state symlink");
        assert!(record_recent_model_ref(temp.path(), &root, &model_ref).is_err());
        assert_eq!(
            std::fs::read_to_string(&outside).expect("outside remains readable"),
            "outside"
        );
    }
}

#[test]
fn connection_config_rejects_unknown_fields_and_unsafe_auth_or_endpoint() {
    let id = ConnectionId::new("custom-local").expect("connection id");
    let unknown = json!({
        "label": "Local",
        "provider": "custom",
        "protocol": "chat_completions",
        "base_url": "http://127.0.0.1:11434/v1",
        "credential": {"source": "none"},
        "options": {},
        "unknown": true
    });
    assert!(ProviderConnectionConfig::from_raw(id.clone(), unknown).is_err());

    let plaintext_option = json!({
        "label": "Local",
        "provider": "custom",
        "protocol": "chat_completions",
        "base_url": "http://127.0.0.1:11434/v1",
        "credential": {"source": "none"},
        "options": {"api_key": "must-never-be-admitted"}
    });
    let error = ProviderConnectionConfig::from_raw(id.clone(), plaintext_option)
        .expect_err("credential-like options must be rejected");
    assert!(format!("{error:#}").contains("reserved or credential-like"));
    assert!(!format!("{error:?}").contains("must-never-be-admitted"));

    for options in [
        json!({"headers": {"Authorization": "Bearer nested-secret"}}),
        json!({"routes": [{"password": "nested-secret"}]}),
        json!({"proxy": {"token": "nested-secret"}}),
        json!({"transport": {"apiKey": "nested-secret"}}),
        json!({"transport": [{"accessToken": "nested-secret"}]}),
        json!({"client": {"clientSecret": "nested-secret"}}),
    ] {
        let nested_plaintext = json!({
            "label": "Local",
            "provider": "custom",
            "protocol": "chat_completions",
            "base_url": "http://127.0.0.1:11434/v1",
            "credential": {"source": "none"},
            "options": options
        });
        let error = ProviderConnectionConfig::from_raw(id.clone(), nested_plaintext)
            .expect_err("nested credential-like options must be rejected");
        assert!(format!("{error:#}").contains("reserved or credential-like"));
        assert!(!format!("{error:?}").contains("nested-secret"));
    }

    for options in [
        json!({"private_key": "schema-secret"}),
        json!({"headers": {"x_custom": "Bearer schema-secret"}}),
    ] {
        let schema_escape = json!({
            "label": "Local",
            "provider": "custom",
            "protocol": "chat_completions",
            "base_url": "http://127.0.0.1:11434/v1",
            "credential": {"source": "none"},
            "options": options
        });
        let error = ProviderConnectionConfig::from_raw(id.clone(), schema_escape)
            .expect_err("provider-owned exact schema must reject unknown option containers");
        assert!(format!("{error:#}").contains("invalid provider-specific connection options"));
        assert!(!format!("{error:?}").contains("schema-secret"));
    }

    let wrong_env = json!({
        "label": "Custom",
        "provider": "custom",
        "protocol": "chat_completions",
        "base_url": "https://gateway.example/v1",
        "credential": {"source": "environment", "name": "HOME"},
        "options": {}
    });
    assert!(ProviderConnectionConfig::from_raw(id.clone(), wrong_env).is_err());

    let credentialed_http = json!({
        "label": "Custom",
        "provider": "custom",
        "protocol": "chat_completions",
        "base_url": "http://127.0.0.1:11434/v1",
        "credential": {
            "source": "environment",
            "name": "SIGIL_OPENAI_COMPATIBLE_API_KEY"
        },
        "options": {}
    });
    assert!(ProviderConnectionConfig::from_raw(id, credentialed_http).is_err());

    let remote_unauthenticated_https = json!({
        "label": "Remote no-auth",
        "provider": "custom",
        "protocol": "chat_completions",
        "base_url": "https://gateway.example/v1",
        "credential": {"source": "none"},
        "options": {}
    });
    let error = ProviderConnectionConfig::from_raw(
        ConnectionId::new("remote-no-auth").expect("connection id"),
        remote_unauthenticated_https,
    )
    .expect_err("unauthenticated HTTPS must still be loopback-only");
    assert!(format!("{error:#}").contains("loopback"));
}

#[test]
fn v2_loader_keeps_valid_connections_when_a_sibling_is_malformed() {
    let root: RootConfig = toml::from_str(
        r#"
config_version = 2

[agent]
connection = "openai-personal"
model = "gpt-4.1"

[connections.openai-personal]
label = "OpenAI"
provider = "openai"
protocol = "responses"
base_url = "https://api.openai.com/v1"
credential = { source = "environment", name = "SIGIL_OPENAI_RESPONSES_API_KEY" }

[connections.broken]
label = "Broken"
provider = "custom"
protocol = "chat_completions"
base_url = "not-a-url"
credential = { source = "none" }
"#,
    )
    .expect("raw V2 config should deserialize");

    let loaded = load_provider_connections(&root);
    assert_eq!(loaded.mode, ConfigMode::V2);
    assert_eq!(loaded.connections.len(), 1);
    assert_eq!(loaded.issues.len(), 1);
    assert_eq!(loaded.issues[0].connection_id.as_deref(), Some("broken"));
    assert_eq!(
        loaded
            .default_model
            .as_ref()
            .expect("default model")
            .connection_id
            .as_str(),
        "openai-personal"
    );
}

#[test]
fn deepseek_v2_wire_name_matches_documented_provider_name() {
    let root: RootConfig = toml::from_str(
        r#"
config_version = 2

[agent]
connection = "deepseek-default"
model = "deepseek-v4-flash"

[connections.deepseek-default]
label = "DeepSeek"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }
"#,
    )
    .expect("documented DeepSeek V2 config should deserialize");

    let loaded = load_provider_connections(&root);
    assert_eq!(loaded.mode, ConfigMode::V2);
    assert!(loaded.issues.is_empty(), "{:?}", loaded.issues);
    assert_eq!(
        loaded
            .default_model
            .as_ref()
            .expect("default model")
            .connection_id
            .as_str(),
        "deepseek-default"
    );
    assert_eq!(
        loaded
            .connections
            .values()
            .next()
            .expect("DeepSeek connection")
            .config
            .provider,
        ProviderFamily::DeepSeek
    );
}

#[test]
fn deepseek_secondary_endpoints_inherit_credentialed_https_policy() {
    let id = ConnectionId::new("deepseek-default").expect("connection id");
    let (mut connection, _) = provider_connection_template(
        ProviderFamily::DeepSeek,
        ProviderProtocol::DeepSeek,
        id,
        "DeepSeek",
    )
    .expect("DeepSeek template");
    connection.options["beta_base_url"] = json!("http://attacker.example");

    let error = connection
        .validate()
        .expect_err("credentialed secondary endpoint must require HTTPS");
    assert!(format!("{error:#}").contains("beta_base_url"));
    assert!(format!("{error:#}").contains("require https"));

    connection.options["beta_base_url"] = json!("https://api.deepseek.com/beta");
    connection.options["anthropic_base_url"] = json!("http://127.0.0.1:8080");
    let error = connection
        .validate()
        .expect_err("DeepSeek loopback secondary endpoint still carries credentials");
    assert!(format!("{error:#}").contains("anthropic_base_url"));
}

#[test]
fn legacy_projection_is_deterministic_and_preserves_exact_model() {
    for (provider, model, body, expected_id, expected_family, expected_protocol) in [
        (
            "deepseek",
            "deepseek-custom-model",
            r#"base_url = "https://api.deepseek.com""#,
            "deepseek-default",
            ProviderFamily::DeepSeek,
            ProviderProtocol::DeepSeek,
        ),
        (
            "openai_compat",
            "deployment/blue",
            r#"base_url = "https://gateway.example/v1""#,
            "openai-compatible-default",
            ProviderFamily::Custom,
            ProviderProtocol::OpenAiChatCompletions,
        ),
        (
            "openai_responses",
            "gpt-account-model",
            r#"base_url = "https://api.openai.com/v1/""#,
            "openai-default",
            ProviderFamily::OpenAi,
            ProviderProtocol::OpenAiResponses,
        ),
        (
            "openai_responses",
            "custom-response-model",
            r#"base_url = "https://responses.example/v1""#,
            "openai-responses-default",
            ProviderFamily::Custom,
            ProviderProtocol::OpenAiResponses,
        ),
        (
            "anthropic",
            "claude-private",
            r#"base_url = "https://api.anthropic.com""#,
            "anthropic-default",
            ProviderFamily::Anthropic,
            ProviderProtocol::AnthropicMessages,
        ),
        (
            "gemini",
            "models/gemini-private",
            r#"base_url = "https://generativelanguage.googleapis.com/v1beta""#,
            "gemini-default",
            ProviderFamily::Gemini,
            ProviderProtocol::GeminiGenerateContent,
        ),
    ] {
        let loaded = load_provider_connections(&legacy_root(provider, model, body));
        assert!(loaded.issues.is_empty(), "{provider}: {:?}", loaded.issues);
        let model_ref = loaded.default_model.expect("default model");
        assert_eq!(model_ref.connection_id.as_str(), expected_id);
        assert_eq!(model_ref.model_id, model);
        let connection = loaded
            .connections
            .get(&model_ref.connection_id)
            .expect("projected connection");
        assert_eq!(connection.config.provider, expected_family);
        assert_eq!(connection.config.protocol, expected_protocol);
    }
}

#[test]
fn legacy_projection_preserves_all_provider_blocks_and_migrates_role_routes() {
    let mut root = legacy_root(
        "deepseek",
        "deepseek-private",
        r#"base_url = "https://api.deepseek.com""#,
    );
    root.providers.insert(
        "anthropic".to_owned(),
        json!({
            "base_url": "https://api.anthropic.com",
            "max_tokens": 2048
        }),
    );
    root.task.planner.provider = Some("anthropic".to_owned());
    let loaded = load_provider_connections(&root);
    assert!(loaded.issues.is_empty(), "{:?}", loaded.issues);
    assert_eq!(loaded.connections.len(), 2);
    let connections = loaded
        .connections
        .into_iter()
        .map(|(id, loaded)| (id, loaded.config))
        .collect::<BTreeMap<_, _>>();
    let migrated = materialize_v2_root_config(
        &root,
        &connections,
        &loaded.default_model.expect("default model"),
    )
    .expect("all legacy providers should migrate");

    assert!(migrated.providers.is_empty());
    assert_eq!(migrated.connections.len(), 2);
    assert!(migrated.task.planner.provider.is_none());
    assert_eq!(
        migrated
            .task
            .planner
            .connection
            .as_ref()
            .expect("role connection")
            .as_str(),
        "anthropic-default"
    );
    assert_eq!(
        migrated.task.planner.model.as_deref(),
        Some("claude-sonnet-4-5")
    );
}

#[test]
fn legacy_projection_freezes_template_defaults_before_applying_overrides() {
    let deepseek = load_provider_connections(&legacy_root(
        "deepseek",
        "deepseek-v4-flash",
        r#"base_url = "https://api.deepseek.com""#,
    ));
    let deepseek_options = deepseek
        .default_connection()
        .expect("deepseek connection")
        .config
        .options
        .as_object()
        .expect("deepseek options");
    assert_eq!(
        deepseek_options.get("strict_tools_mode"),
        Some(&json!("auto"))
    );
    assert_eq!(
        deepseek_options.get("fim_model"),
        Some(&json!("deepseek-v4-pro"))
    );

    let anthropic = load_provider_connections(&legacy_root(
        "anthropic",
        "claude-custom",
        r#"
base_url = "https://api.anthropic.com"
max_tokens = 2048
"#,
    ));
    let anthropic_options = anthropic
        .default_connection()
        .expect("anthropic connection")
        .config
        .options
        .as_object()
        .expect("anthropic options");
    assert_eq!(
        anthropic_options.get("anthropic_version"),
        Some(&json!("2023-06-01"))
    );
    assert_eq!(anthropic_options.get("max_tokens"), Some(&json!(2048)));
}

#[test]
fn legacy_plaintext_is_redacted_and_requires_explicit_migration() {
    let secret = "legacy-secret-canary";
    let root = legacy_root(
        "deepseek",
        "deepseek-v4-flash",
        &format!(
            r#"base_url = "https://api.deepseek.com"
api_key = "{secret}""#
        ),
    );
    let loaded = load_provider_connections(&root);
    assert!(loaded.migration_required());
    assert!(!format!("{root:?}").contains(secret));
    assert!(!format!("{loaded:?}").contains(secret));
}

#[test]
fn legacy_migration_plan_moves_all_inline_keys_and_preserves_environment_connections() {
    let secret = "legacy-plan-secret-canary";
    let root: RootConfig = toml::from_str(&format!(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-private"

[providers.deepseek]
base_url = "https://private.deepseek.example/v1"
api_key = "{secret}"
strict_tools_mode = "auto"

[providers.anthropic]
base_url = "https://api.anthropic.com"
"#
    ))
    .expect("multi-provider legacy config");

    let preview = legacy_connection_migration_preview(&root)
        .expect("legacy migration preview should prepare");
    assert_eq!(preview.connection_count, 2);
    assert_eq!(preview.inline_credential_count, 1);
    assert_eq!(preview.environment_reference_count, 1);
    assert_eq!(
        preview.default_model,
        ModelRef::new(
            ConnectionId::new("deepseek-default").expect("connection id"),
            "deepseek-private",
        )
        .expect("model ref")
    );
    let loaded = load_provider_connections(&root);
    let deepseek = loaded
        .connections
        .get(&ConnectionId::new("deepseek-default").expect("connection id"))
        .expect("projected DeepSeek connection");
    assert_eq!(
        deepseek.config.base_url,
        "https://private.deepseek.example/v1"
    );
    let anthropic = loaded
        .connections
        .get(&ConnectionId::new("anthropic-default").expect("connection id"))
        .expect("projected Anthropic connection");
    assert!(matches!(
        &anthropic.config.credential,
        CredentialRefConfig::Environment { name }
            if name == "SIGIL_ANTHROPIC_API_KEY"
    ));
    assert!(!format!("{preview:?}").contains(secret));
}

#[test]
fn legacy_migration_plan_rejects_v2_and_malformed_legacy_configs() {
    let v2 = materialize_v2_root_config(
        &legacy_root(
            "deepseek",
            "deepseek-v4-flash",
            r#"base_url = "https://api.deepseek.com""#,
        ),
        &BTreeMap::from([(deepseek_connection().id.clone(), deepseek_connection())]),
        &ModelRef::new(
            ConnectionId::new("deepseek-default").expect("connection id"),
            "deepseek-v4-flash",
        )
        .expect("model ref"),
    )
    .expect("V2 config");
    assert!(matches!(
        legacy_connection_migration_preview(&v2),
        Err(LegacyConnectionMigrationError::NotLegacy)
    ));

    let mut malformed = legacy_root(
        "deepseek",
        "deepseek-v4-flash",
        r#"base_url = "https://api.deepseek.com""#,
    );
    malformed
        .providers
        .insert("broken".to_owned(), json!("not-an-object"));
    assert!(matches!(
        legacy_connection_migration_preview(&malformed),
        Err(LegacyConnectionMigrationError::InvalidConfig)
    ));
}

#[tokio::test]
async fn legacy_migration_transaction_uses_exact_source_and_disk_truth() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let path = temp.path().join("sigil.toml");
    let source = br#"
[storage]
credential_store = "file"

[agent]
provider = "deepseek"
model = "deepseek-private"

[providers.deepseek]
base_url = "https://private.deepseek.example/v1"
api_key = "legacy-transaction-secret"

[providers.anthropic]
base_url = "https://api.anthropic.com"
"#;
    std::fs::write(&path, source).expect("legacy source should write");
    let store = FakeCredentialStore::default();
    let outcome = migrate_legacy_provider_config(&path, source, &store, &RootConfigPublisher)
        .await
        .expect("legacy migration should publish");
    assert_eq!(
        outcome.status,
        LegacyConnectionMigrationPublishStatus::Published
    );
    assert_eq!(outcome.connection_count, 2);
    assert_eq!(outcome.inline_credential_count, 1);
    assert_eq!(outcome.environment_reference_count, 1);
    assert_eq!(store.records.lock().expect("records lock").len(), 1);
    let persisted = std::fs::read_to_string(&path).expect("migrated config should read");
    assert!(persisted.contains("config_version = 2"));
    assert!(persisted.contains("deepseek-private"));
    assert!(persisted.contains("https://private.deepseek.example/v1"));
    assert!(!persisted.contains("legacy-transaction-secret"));

    std::fs::write(&path, source).expect("legacy source should restore");
    let changed = [source.as_slice(), b"\n# concurrent comment\n"].concat();
    std::fs::write(&path, &changed).expect("concurrent source should write");
    let records_before = store.records.lock().expect("records lock").len();
    assert!(matches!(
        migrate_legacy_provider_config(&path, source, &store, &RootConfigPublisher).await,
        Err(LegacyConnectionMigrationTransactionError::Stale)
    ));
    assert_eq!(
        store.records.lock().expect("records lock").len(),
        records_before
    );
    assert_eq!(
        std::fs::read(&path).expect("stale source should remain"),
        changed
    );

    std::fs::remove_file(&path).expect("stale source should be removable");
    assert!(matches!(
        migrate_legacy_provider_config(&path, source, &store, &RootConfigPublisher).await,
        Err(LegacyConnectionMigrationTransactionError::Stale)
    ));
    assert_eq!(
        store.records.lock().expect("records lock").len(),
        records_before
    );
}

#[tokio::test]
async fn legacy_environment_only_migration_writes_no_credential_record() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let path = temp.path().join("sigil.toml");
    let source = br#"
[agent]
provider = "anthropic"
model = "claude-private"

[providers.anthropic]
base_url = "https://api.anthropic.com"
"#;
    std::fs::write(&path, source).expect("legacy source should write");
    let store = FakeCredentialStore::default();

    let outcome = migrate_legacy_provider_config(&path, source, &store, &RootConfigPublisher)
        .await
        .expect("environment-only migration should publish");

    assert_eq!(outcome.inline_credential_count, 0);
    assert_eq!(outcome.environment_reference_count, 1);
    assert!(store.records.lock().expect("records lock").is_empty());
    let persisted = std::fs::read_to_string(path).expect("migrated config should read");
    assert!(persisted.contains("source = \"environment\""));
    assert!(persisted.contains("SIGIL_ANTHROPIC_API_KEY"));
    assert!(persisted.contains("claude-private"));
}

#[tokio::test]
async fn legacy_visibility_uncertainty_rolls_back_when_old_source_is_still_live() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let path = temp.path().join("sigil.toml");
    let source = br#"
[agent]
provider = "deepseek"
model = "deepseek-private"

[providers.deepseek]
base_url = "https://api.deepseek.com"
api_key = "visibility-secret-canary"
"#;
    std::fs::write(&path, source).expect("legacy source should write");
    let store = FakeCredentialStore::default();
    let publisher = FakePublisher::published(ConfigPublishOutcome::PublishedVisibilityUncertain {
        recovery_path: Some(temp.path().join("sigil.previous")),
    });

    let result = migrate_legacy_provider_config(&path, source, &store, &publisher).await;

    assert!(matches!(
        result,
        Err(LegacyConnectionMigrationTransactionError::NotPublished {
            rollback_incomplete: false
        })
    ));
    assert!(store.records.lock().expect("records lock").is_empty());
    assert_eq!(
        std::fs::read(&path).expect("old source should remain"),
        source
    );
}

#[tokio::test]
async fn legacy_incomplete_rollback_persists_a_restart_safe_recovery_block() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let path = temp.path().join("sigil.toml");
    let source = br#"
[agent]
provider = "deepseek"
model = "deepseek-private"

[providers.deepseek]
base_url = "https://api.deepseek.com"
api_key = "rollback-marker-secret"
"#;
    std::fs::write(&path, source).expect("legacy source should write");
    let store = FakeCredentialStore::default();
    *store.fail_delete.lock().expect("delete flag") = true;
    let publisher = FakePublisher::published(ConfigPublishOutcome::PublishedVisibilityUncertain {
        recovery_path: Some(temp.path().join("sigil.previous")),
    });

    assert!(matches!(
        migrate_legacy_provider_config(&path, source, &store, &publisher).await,
        Err(LegacyConnectionMigrationTransactionError::NotPublished {
            rollback_incomplete: true
        })
    ));
    assert_eq!(
        legacy_migration_recovery_state(&path).expect("recovery marker should read"),
        Some(LegacyMigrationRecoveryState::RollbackIncomplete)
    );
    let orphaned_credential_id = store
        .records
        .lock()
        .expect("records lock")
        .keys()
        .next()
        .cloned()
        .expect("rollback failure should retain one orphan");
    let marker_path = temp
        .path()
        .join("sigil.toml.provider-migration-recovery-v1");
    let marker = std::fs::read_to_string(&marker_path).expect("recovery marker should read");
    assert!(marker.contains(&orphaned_credential_id.to_string()));
    assert!(!marker.contains("rollback-marker-secret"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&marker_path)
                .expect("recovery marker metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert!(matches!(
        migrate_legacy_provider_config(&path, source, &store, &RootConfigPublisher).await,
        Err(
            LegacyConnectionMigrationTransactionError::RecoveryRequired {
                state: LegacyMigrationRecoveryState::RollbackIncomplete
            }
        )
    ));
    assert_eq!(store.records.lock().expect("records lock").len(), 1);

    let legacy = RootConfig::parse_persisted(
        std::str::from_utf8(source).expect("legacy source should be UTF-8"),
    )
    .expect("legacy source should parse");
    let loaded = load_provider_connections(&legacy);
    let default_model = loaded.default_model.expect("legacy default model");
    let mut repaired_connections = loaded
        .connections
        .into_iter()
        .map(|(id, loaded)| (id, loaded.config))
        .collect::<BTreeMap<_, _>>();
    repaired_connections
        .get_mut(&default_model.connection_id)
        .expect("default connection should exist")
        .credential = CredentialRefConfig::Environment {
        name: "SIGIL_API_KEY".to_owned(),
    };
    let repaired = materialize_v2_root_config(&legacy, &repaired_connections, &default_model)
        .expect("healthy V2 repair should materialize");
    let repaired_source = toml::to_string(&repaired).expect("healthy V2 should serialize");
    std::fs::write(&path, &repaired_source).expect("healthy V2 repair should write");
    let environment = MapEnvironment(BTreeMap::from([(
        "SIGIL_API_KEY".to_owned(),
        "repaired-environment-secret".to_owned(),
    )]));
    let healthy_inventory = connection_inventory_offline(&repaired, &environment);
    *store.fail_delete.lock().expect("delete flag") = false;
    assert!(
        super::persistence::clear_legacy_migration_recovery_if_healthy(
            &path,
            repaired_source.as_bytes(),
            &repaired,
            &healthy_inventory,
            super::persistence::RecoveryCredentialCleanupStore::Injected(&store),
        )
        .await
        .expect("healthy V2 recheck should clean tracked orphan")
    );
    assert!(store.records.lock().expect("records lock").is_empty());
    assert!(!marker_path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn legacy_migration_publishes_a_recovery_guard_before_storing_credentials() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("temporary directory");
    let path = temp.path().join("sigil.toml");
    let lock_path = temp.path().join(".sigil.toml.update.lock");
    let source = br#"
[agent]
provider = "deepseek"
model = "deepseek-private"

[providers.deepseek]
base_url = "https://api.deepseek.com"
api_key = "write-ahead-secret"
"#;
    std::fs::write(&path, source).expect("legacy source should write");
    std::fs::write(&lock_path, "").expect("config update lock should preexist");
    std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600))
        .expect("config update lock should be private");
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o500))
        .expect("config parent should become read-only");
    let store = FakeCredentialStore::default();

    let result = migrate_legacy_provider_config(&path, source, &store, &RootConfigPublisher).await;
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))
        .expect("config parent permissions should restore");

    assert!(
        matches!(
            result,
            Err(LegacyConnectionMigrationTransactionError::Save {
                source: ConnectionSaveError::CredentialStoreWrite { .. }
            })
        ),
        "unexpected migration result: {result:?}"
    );
    assert_eq!(store.store_calls.load(Ordering::SeqCst), 0);
    assert!(store.records.lock().expect("records lock").is_empty());
    assert!(
        !temp
            .path()
            .join("sigil.toml.provider-migration-recovery-v1")
            .exists()
    );
}

#[tokio::test]
async fn recovery_recheck_preserves_tracked_credentials_referenced_by_healthy_v2() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let path = temp.path().join("sigil.toml");
    let marker_path = temp
        .path()
        .join("sigil.toml.provider-migration-recovery-v1");
    let store = FakeCredentialStore::default();
    let credential_id = CredentialId::random();
    store
        .store(&ProviderCredentialRecord::new(
            credential_id.clone(),
            &PreparedCredential::api_key(ProviderFamily::DeepSeek, "published-secret"),
        ))
        .await
        .expect("published credential should write");
    let mut connection = deepseek_connection();
    connection.credential = CredentialRefConfig::Stored {
        id: credential_id.clone(),
    };
    let default_model =
        ModelRef::new(connection.id.clone(), "deepseek-private").expect("default model");
    let root = materialize_v2_root_config(
        &legacy_root(
            "deepseek",
            "deepseek-private",
            r#"base_url = "https://api.deepseek.com""#,
        ),
        &BTreeMap::from([(connection.id.clone(), connection)]),
        &default_model,
    )
    .expect("V2 config should materialize");
    let source = toml::to_string(&root).expect("V2 config should serialize");
    std::fs::write(&path, &source).expect("V2 config should write");
    std::fs::write(
        &marker_path,
        format!(
            "sigil-provider-migration-recovery-v2\nrollback_incomplete\norphan={credential_id}\n"
        ),
    )
    .expect("write-ahead recovery record should write");
    let inventory = connection_inventory(&root, &store, &MapEnvironment::default()).await;

    assert!(
        super::persistence::clear_legacy_migration_recovery_if_healthy(
            &path,
            source.as_bytes(),
            &root,
            &inventory,
            super::persistence::RecoveryCredentialCleanupStore::Injected(&store),
        )
        .await
        .expect("healthy V2 recheck should resolve write-ahead guard")
    );
    assert!(
        store
            .records
            .lock()
            .expect("records lock")
            .contains_key(&credential_id)
    );
    assert!(!marker_path.exists());
}

#[tokio::test]
async fn legacy_visibility_reconciliation_requires_the_complete_target_config() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let path = temp.path().join("sigil.toml");
    let source = br#"
[agent]
provider = "deepseek"
model = "deepseek-private"

[providers.deepseek]
base_url = "https://private.deepseek.example/v1"
api_key = "target-binding-secret"

[providers.anthropic]
base_url = "https://api.anthropic.com"
"#;
    std::fs::write(&path, source).expect("legacy source should write");
    let legacy = RootConfig::parse_persisted(
        std::str::from_utf8(source).expect("legacy source should be UTF-8"),
    )
    .expect("legacy source should parse");
    let loaded = load_provider_connections(&legacy);
    let deepseek_id = ConnectionId::new("deepseek-default").expect("connection id");
    let deepseek = loaded
        .connections
        .get(&deepseek_id)
        .expect("DeepSeek projection")
        .config
        .clone();
    let alternate = materialize_v2_root_config(
        &legacy,
        &BTreeMap::from([(deepseek_id.clone(), deepseek)]),
        &ModelRef::new(deepseek_id, "deepseek-private").expect("model ref"),
    )
    .expect("alternate V2 should materialize");
    let store = FakeCredentialStore::default();

    let result = migrate_legacy_provider_config(
        &path,
        source,
        &store,
        &AlternateConfigPublisher {
            replacement: alternate,
        },
    )
    .await;

    assert!(matches!(
        result,
        Err(LegacyConnectionMigrationTransactionError::ReconcileRequired)
    ));
    assert_eq!(store.records.lock().expect("records lock").len(), 1);
    let disk = RootConfig::load_persisted(&path).expect("alternate V2 should remain inspectable");
    assert_eq!(
        load_provider_connections(&disk).connections.len(),
        1,
        "an incomplete same-default V2 must not be accepted as the migration target"
    );
    assert_eq!(
        legacy_migration_recovery_state(&path).expect("recovery marker should read"),
        Some(LegacyMigrationRecoveryState::ReconcileRequired)
    );
    assert!(matches!(
        migrate_legacy_provider_config(&path, source, &store, &RootConfigPublisher).await,
        Err(
            LegacyConnectionMigrationTransactionError::RecoveryRequired {
                state: LegacyMigrationRecoveryState::ReconcileRequired
            }
        )
    ));

    let default_model = load_provider_connections(&disk)
        .default_model
        .expect("alternate config should retain a default");
    let healthy_inventory = ConnectionInventory {
        mode: ConfigMode::V2,
        default_model: Some(default_model.clone()),
        entries: vec![ConnectionInventoryEntry {
            id: default_model.connection_id.clone(),
            label: "DeepSeek".to_owned(),
            provider_label: "DeepSeek".to_owned(),
            protocol_label: "DeepSeek".to_owned(),
            endpoint_display: "private.deepseek.example".to_owned(),
            credential_source: CredentialSourceView::Stored,
            readiness: ConnectionReadiness::Ready,
            default_model: Some(default_model),
            issue: None,
        }],
        issues: Vec::new(),
    };
    assert!(
        !super::persistence::clear_legacy_migration_recovery_if_healthy(
            &path,
            source,
            &disk,
            &healthy_inventory,
            super::persistence::RecoveryCredentialCleanupStore::Injected(&store),
        )
        .await
        .expect("stale recheck should remain blocked")
    );
    assert_eq!(
        legacy_migration_recovery_state(&path).expect("stale recheck marker should read"),
        Some(LegacyMigrationRecoveryState::ReconcileRequired)
    );
    assert!(
        super::persistence::clear_legacy_migration_recovery_if_healthy(
            &path,
            &std::fs::read(&path).expect("live source should read"),
            &disk,
            &healthy_inventory,
            super::persistence::RecoveryCredentialCleanupStore::Injected(&store),
        )
        .await
        .expect("healthy explicit recheck should clear recovery")
    );
    assert_eq!(
        legacy_migration_recovery_state(&path).expect("cleared marker should read"),
        None
    );
}

#[tokio::test]
async fn legacy_store_failure_before_write_is_not_reported_as_an_orphan() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let path = temp.path().join("sigil.toml");
    let source = br#"
[agent]
provider = "deepseek"
model = "deepseek-private"

[providers.deepseek]
base_url = "https://api.deepseek.com"
api_key = "pre-write-secret"
"#;
    std::fs::write(&path, source).expect("legacy source should write");
    let store = FakeCredentialStore::default();
    *store
        .fail_store_before_write
        .lock()
        .expect("pre-write flag") = true;

    let result = migrate_legacy_provider_config(&path, source, &store, &RootConfigPublisher).await;

    assert!(matches!(
        result,
        Err(LegacyConnectionMigrationTransactionError::Save {
            source: ConnectionSaveError::CredentialStoreWrite {
                orphaned_credential: false,
                ..
            }
        })
    ));
    assert!(store.records.lock().expect("records lock").is_empty());
    assert_eq!(
        std::fs::read(&path).expect("legacy source should remain"),
        source
    );
}

#[tokio::test]
async fn multi_inline_legacy_failure_rolls_back_every_created_record() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let path = temp.path().join("sigil.toml");
    let source = br#"
[agent]
provider = "deepseek"
model = "deepseek-private"

[providers.deepseek]
base_url = "https://api.deepseek.com"
api_key = "first-inline-secret"

[providers.anthropic]
base_url = "https://api.anthropic.com"
api_key = "second-inline-secret"
"#;
    std::fs::write(&path, source).expect("legacy source should write");
    let store = FakeCredentialStore::default();
    *store.fail_store_on_call.lock().expect("store call failure") = Some(2);

    let result = migrate_legacy_provider_config(&path, source, &store, &RootConfigPublisher).await;

    assert!(matches!(
        result,
        Err(LegacyConnectionMigrationTransactionError::Save {
            source: ConnectionSaveError::CredentialStoreWrite {
                orphaned_credential: false,
                ..
            }
        })
    ));
    assert!(store.records.lock().expect("records lock").is_empty());
    assert_eq!(store.store_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        std::fs::read(&path).expect("legacy source should remain"),
        source
    );
}

#[tokio::test]
async fn credential_resolution_is_exact_and_redacted() {
    let connection = deepseek_connection();
    let store = FakeCredentialStore::default();
    let mut environment = MapEnvironment::default();
    environment
        .0
        .insert("SIGIL_API_KEY".to_owned(), "environment-secret".to_owned());
    let resolved = resolve_connection_credential(
        &connection,
        &LoadedCredentialRef::Config(CredentialRefConfig::Environment {
            name: "SIGIL_API_KEY".to_owned(),
        }),
        &store,
        &environment,
    )
    .await
    .expect("environment credential should resolve");
    assert_eq!(
        resolved
            .secret
            .as_ref()
            .expect("resolved secret")
            .expose_secret(),
        "environment-secret"
    );
    assert!(!format!("{resolved:?}").contains("environment-secret"));
}

#[tokio::test]
async fn v2_provider_builder_uses_the_exact_connection_and_injected_environment() {
    let mut connection = provider_connection_template(
        ProviderFamily::OpenAi,
        ProviderProtocol::OpenAiResponses,
        ConnectionId::new("openai-personal").expect("connection id"),
        "OpenAI",
    )
    .expect("connection template")
    .0;
    connection.base_url = "https://api.openai.com/v1".to_owned();
    let mut root = materialize_v2_root_config(
        &legacy_root(
            "openai_responses",
            "legacy-model",
            r#"base_url = "https://api.openai.com/v1""#,
        ),
        &BTreeMap::from([(connection.id.clone(), connection)]),
        &ModelRef::new(
            ConnectionId::new("openai-personal").expect("connection id"),
            "account/deployment:model",
        )
        .expect("model ref"),
    )
    .expect("V2 config");
    root.connections.insert(
        "broken-unused".to_owned(),
        json!({
            "label": "Broken unused sibling",
            "provider": "custom",
            "protocol": "chat_completions",
            "base_url": "not-a-url",
            "credential": {"source": "none"},
            "options": {}
        }),
    );
    let mut environment = MapEnvironment::default();
    environment.0.insert(
        "SIGIL_OPENAI_RESPONSES_API_KEY".to_owned(),
        "exact-environment-secret".to_owned(),
    );

    let provider = crate::build_provider_with_credentials(
        &root,
        &FakeCredentialStore::default(),
        &environment,
    )
    .await
    .expect("V2 provider should build");
    assert_eq!(provider.name(), "openai_responses");
}

#[tokio::test]
async fn malformed_sibling_identity_cannot_collide_with_default_or_inject_terminal_text() {
    let mut root = local_catalog_root("http://127.0.0.1:11434/v1".to_owned(), "local-model");
    let valid_id = "a".repeat(64);
    let raw = root.connections.remove("local").expect("local connection");
    root.connections.insert(valid_id.clone(), raw.clone());
    root.agent.connection =
        Some(ConnectionId::new(valid_id.clone()).expect("maximum-length connection id"));
    root.connections.insert(format!("{valid_id}x\u{202e}"), raw);

    let loaded = load_provider_connections(&root);
    let issue = loaded.issues.first().expect("malformed sibling issue");
    let issue_identity = issue
        .connection_id
        .as_deref()
        .expect("malformed sibling identity");
    assert!(issue_identity.starts_with("!invalid-"));
    assert!(!issue_identity.contains('\u{202e}'));
    assert_ne!(issue_identity, valid_id);

    let provider = crate::build_provider_with_credentials(
        &root,
        &FakeCredentialStore::default(),
        &MapEnvironment::default(),
    )
    .await
    .expect("malformed sibling must not block the exact valid default");
    assert_eq!(provider.name(), "openai_compat");
}

#[tokio::test]
async fn explicit_repair_save_can_remove_a_malformed_v2_sibling() {
    let mut root = local_catalog_root("http://127.0.0.1:11434/v1".to_owned(), "local-model");
    root.connections.insert(
        "broken-unused".to_owned(),
        json!({
            "label": "Broken unused sibling",
            "provider": "custom",
            "protocol": "chat_completions",
            "base_url": "not-a-url",
            "credential": {"source": "none"}
        }),
    );
    let loaded = load_provider_connections(&root);
    assert!(!loaded.issues.is_empty());
    let default_model = loaded.default_model.expect("valid default model");
    let connections = loaded
        .connections
        .into_iter()
        .map(|(id, connection)| (id, connection.config))
        .collect::<BTreeMap<_, _>>();

    let outcome = save_connection_config(
        &root,
        &unused_config_path(),
        ConnectionSaveDraft {
            connections,
            default_model,
            credential_updates: Vec::new(),
            confirmed_legacy_environment: Default::default(),
        },
        &FakeCredentialStore::default(),
        &FakePublisher::published(ConfigPublishOutcome::Published),
    )
    .await
    .expect("explicit repair should publish a fully valid replacement");

    assert!(
        !outcome
            .root_config
            .connections
            .contains_key("broken-unused")
    );
    assert!(
        load_provider_connections(&outcome.root_config)
            .issues
            .is_empty()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn synchronous_v2_builder_rejects_async_runtime_without_blocking() {
    let root = local_catalog_root("http://127.0.0.1:11434/v1".to_owned(), "local-model");
    let error = crate::build_provider(&root)
        .err()
        .expect("sync compatibility entry must reject async callers");
    assert!(
        format!("{error:#}").contains("use build_provider_async"),
        "unexpected error: {error:#}"
    );
    let provider = crate::build_provider_async(&root)
        .await
        .expect("async callers should use the async builder");
    assert_eq!(provider.name(), "openai_compat");
}

#[tokio::test]
async fn exact_v2_provider_and_role_builders_remain_async_inside_runtime() {
    let root = local_catalog_root("http://127.0.0.1:11434/v1".to_owned(), "local-model");
    let alternate = ModelRef::new(
        ConnectionId::new("local").expect("connection id"),
        "other-model",
    )
    .expect("alternate model ref");
    let exact_sync_error = crate::build_provider_for_model_ref(&root, &alternate)
        .err()
        .expect("exact synchronous wrapper must reject async callers");
    assert!(
        format!("{exact_sync_error:#}").contains("use build_provider_for_model_ref_async"),
        "unexpected error: {exact_sync_error:#}"
    );
    let provider = crate::build_provider_for_model_ref_async(&root, &alternate)
        .await
        .expect("exact provider should build without a synchronous shim");
    assert_eq!(provider.name(), "openai_compat");

    let role_sync_error = crate::build_role_provider(&root, sigil_kernel::AgentRole::Planner)
        .err()
        .expect("role synchronous wrapper must reject async callers");
    assert!(
        format!("{role_sync_error:#}").contains("use build_role_provider_async"),
        "unexpected error: {role_sync_error:#}"
    );
    let role_provider = crate::build_role_provider_async(&root, sigil_kernel::AgentRole::Planner)
        .await
        .expect("task role provider should build inside the async runtime");
    assert_eq!(role_provider.name(), "openai_compat");
}

#[test]
fn resolved_route_allows_credential_rotation_but_rejects_semantic_drift() {
    let root = local_catalog_root("https://models.example.test/v1".to_owned(), "local-model");
    let (provider_name, route) =
        resolve_default_model_route(&root).expect("default route should resolve");
    assert_eq!(provider_name, "openai_compat");
    assert_eq!(
        validate_persisted_model_route(&root, &route).expect("unchanged route"),
        "openai_compat"
    );

    let loaded = load_provider_connections(&root);
    let mut rotated = loaded
        .connections
        .get(&route.model_ref.connection_id)
        .expect("connection")
        .config
        .clone();
    rotated.credential = CredentialRefConfig::SystemKeyring {
        id: CredentialId::random(),
    };
    let rotated_root = materialize_v2_root_config(
        &root,
        &BTreeMap::from([(rotated.id.clone(), rotated.clone())]),
        &route.model_ref,
    )
    .expect("rotated config");
    validate_persisted_model_route(&rotated_root, &route)
        .expect("credential identity and generation are not semantic route inputs");

    rotated.base_url = "https://alternate.example.test/v1".to_owned();
    let drifted_root = materialize_v2_root_config(
        &rotated_root,
        &BTreeMap::from([(rotated.id.clone(), rotated)]),
        &route.model_ref,
    )
    .expect("drifted config");
    assert!(matches!(
        validate_persisted_model_route(&drifted_root, &route),
        Err(ResolvedRouteError::SemanticDrift)
    ));
}

#[tokio::test]
async fn in_memory_mixed_and_future_schemas_fail_before_legacy_dispatch() {
    let mut mixed = local_catalog_root("http://127.0.0.1:11434/v1".to_owned(), "local-model");
    mixed.agent.provider = "deepseek".to_owned();
    mixed
        .providers
        .insert("deepseek".to_owned(), json!({"api_key": "must-not-run"}));
    let mixed_error = crate::build_provider_with_credentials(
        &mixed,
        &FakeCredentialStore::default(),
        &MapEnvironment::default(),
    )
    .await
    .err()
    .expect("mixed schema must fail closed");
    assert!(format!("{mixed_error:#}").contains("mixed_config_schema"));
    assert!(!format!("{mixed_error:#}").contains("must-not-run"));

    let mut future = mixed;
    future.agent.provider.clear();
    future.providers.clear();
    future.config_version = Some(99);
    let future_error = crate::build_provider_with_credentials(
        &future,
        &FakeCredentialStore::default(),
        &MapEnvironment::default(),
    )
    .await
    .err()
    .expect("future schema must fail closed");
    assert!(format!("{future_error:#}").contains("connection_config_invalid"));
    assert!(
        load_provider_connections(&future)
            .issues
            .iter()
            .any(|issue| issue.code == "unsupported_config_version")
    );
}

#[tokio::test]
async fn connection_inventory_is_secret_free_and_reports_each_connection() {
    let env_connection = deepseek_connection();
    let mut stored_connection = provider_connection_template(
        ProviderFamily::OpenAi,
        ProviderProtocol::OpenAiResponses,
        ConnectionId::new("openai-personal").expect("connection id"),
        "OpenAI personal",
    )
    .expect("connection template")
    .0;
    let missing_stored_id = CredentialId::random();
    stored_connection.credential = CredentialRefConfig::Stored {
        id: missing_stored_id.clone(),
    };
    let model_ref =
        ModelRef::new(env_connection.id.clone(), "deepseek-v4-flash").expect("model ref");
    let root = materialize_v2_root_config(
        &legacy_root(
            "deepseek",
            "deepseek-v4-flash",
            r#"base_url = "https://api.deepseek.com""#,
        ),
        &BTreeMap::from([
            (env_connection.id.clone(), env_connection),
            (stored_connection.id.clone(), stored_connection),
        ]),
        &model_ref,
    )
    .expect("V2 config");
    let inventory = connection_inventory(
        &root,
        &FakeCredentialStore::default(),
        &MapEnvironment::default(),
    )
    .await;

    assert_eq!(inventory.entries.len(), 2);
    assert!(inventory.entries.iter().any(|entry| {
        entry.id.as_str() == "deepseek-default"
            && entry.readiness == ConnectionReadiness::NeedsCredential
    }));
    assert!(inventory.entries.iter().any(|entry| {
        entry.id.as_str() == "openai-personal"
            && entry.credential_source == CredentialSourceView::Stored
            && entry.readiness == ConnectionReadiness::NeedsCredential
    }));
    let rendered = format!("{inventory:?}");
    assert!(!rendered.contains(&missing_stored_id.to_string()));
    assert!(!rendered.contains("api.deepseek.com"));
    assert!(!rendered.contains("api.openai.com"));
}

#[tokio::test]
async fn cancelled_connection_inventory_does_not_start_stored_credential_work() {
    let mut connection = deepseek_connection();
    connection.credential = CredentialRefConfig::Stored {
        id: CredentialId::random(),
    };
    let model_ref = ModelRef::new(connection.id.clone(), "deepseek-v4-flash")
        .expect("default model ref should parse");
    let root = materialize_v2_root_config(
        &legacy_root(
            "deepseek",
            "deepseek-v4-flash",
            r#"base_url = "https://api.deepseek.com""#,
        ),
        &BTreeMap::from([(connection.id.clone(), connection)]),
        &model_ref,
    )
    .expect("V2 config");
    let cancelled = AtomicBool::new(true);
    let inventory = connection_inventory_with_cancellation(
        &root,
        &FakeCredentialStore::default(),
        &MapEnvironment::default(),
        &cancelled,
    )
    .await;

    assert_eq!(inventory.entries.len(), 1);
    assert_eq!(
        inventory.entries[0].readiness,
        ConnectionReadiness::Unverified
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn catalog_is_connection_scoped_single_flight_and_uses_exact_offline_cache() {
    let _environment_guard = crate::test_env::lock();
    let (base_url, request_count, server) = spawn_catalog_server(
        200,
        r#"{"data":[{"id":"local-alpha"},{"id":"local-beta"},{"id":"local-alpha"}]}"#,
        Duration::from_millis(100),
    )
    .await;
    let root = local_catalog_root(base_url, "configured-only");
    let connection = load_provider_connections(&root)
        .default_connection()
        .expect("default connection")
        .config
        .clone();
    let fingerprint = connection_semantic_fingerprint(&connection);
    let cache_root = tempfile::tempdir().expect("cache root");
    let service = ProviderModelCatalogService::new(
        cache_root.path().to_path_buf(),
        Arc::new(FakeCredentialStore::default()),
        Arc::new(MapEnvironment::default()),
    )
    .expect("catalog service");
    let request = |request_id| ModelCatalogRequest {
        request_id,
        connection_id: ConnectionId::new("local").expect("connection id"),
        draft_revision: 7,
        connection_fingerprint: fingerprint.clone(),
        explicit_refresh: true,
    };

    let (first, second) = tokio::join!(
        service.models(&root, request(1)),
        service.models(&root, request(2))
    );
    assert_eq!(request_count.load(Ordering::SeqCst), 1);
    for result in [&first, &second] {
        assert!(matches!(
            result.state,
            ModelCatalogState::Remote | ModelCatalogState::CacheFresh
        ));
        assert!(result.entries.iter().any(|entry| {
            entry.model_ref.connection_id.as_str() == "local"
                && entry.model_ref.model_id == "local-alpha"
                && entry.provenance
                    == if result.state == ModelCatalogState::Remote {
                        ModelCatalogProvenance::Remote
                    } else {
                        ModelCatalogProvenance::Cache
                    }
        }));
        assert!(result.entries.iter().any(|entry| {
            entry.model_ref.model_id == "configured-only"
                && entry.availability == ModelAvailability::ConfiguredUnavailable
        }));
        assert!(
            result
                .entries
                .iter()
                .all(|entry| !entry.model_ref.model_id.starts_with("deepseek-"))
        );
    }

    server.abort();
    let restarted = ProviderModelCatalogService::new(
        cache_root.path().to_path_buf(),
        Arc::new(FakeCredentialStore::default()),
        Arc::new(MapEnvironment::default()),
    )
    .expect("restarted catalog service");
    let offline = restarted.models(&root, request(3)).await;
    assert_eq!(offline.state, ModelCatalogState::Offline);
    assert!(
        offline
            .entries
            .iter()
            .any(|entry| entry.provenance == ModelCatalogProvenance::Cache)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let cache_parent = cache_root
            .path()
            .join("provider-models")
            .join("v1")
            .join("local");
        assert_eq!(
            std::fs::metadata(&cache_parent)
                .expect("cache parent")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        std::fs::set_permissions(&cache_parent, std::fs::Permissions::from_mode(0o755))
            .expect("make cache parent non-private");
        let rejected_permissions = ProviderModelCatalogService::new(
            cache_root.path().to_path_buf(),
            Arc::new(FakeCredentialStore::default()),
            Arc::new(MapEnvironment::default()),
        )
        .expect("permission-check catalog service")
        .models(
            &root,
            ModelCatalogRequest {
                explicit_refresh: false,
                ..request(4)
            },
        )
        .await;
        assert_eq!(rejected_permissions.state, ModelCatalogState::Offline);
        assert!(
            rejected_permissions
                .entries
                .iter()
                .all(|entry| entry.provenance != ModelCatalogProvenance::Cache)
        );
        std::fs::set_permissions(&cache_parent, std::fs::Permissions::from_mode(0o700))
            .expect("restore private cache parent");

        let alias_parent = tempfile::tempdir().expect("cache alias parent");
        let alias = alias_parent.path().join("cache-link");
        std::os::unix::fs::symlink(cache_root.path(), &alias).expect("cache root symlink");
        let rejected_symlink = ProviderModelCatalogService::new(
            alias,
            Arc::new(FakeCredentialStore::default()),
            Arc::new(MapEnvironment::default()),
        )
        .expect("symlink-check catalog service")
        .models(
            &root,
            ModelCatalogRequest {
                explicit_refresh: false,
                ..request(5)
            },
        )
        .await;
        assert_eq!(rejected_symlink.state, ModelCatalogState::Offline);
        assert!(
            rejected_symlink
                .entries
                .iter()
                .all(|entry| entry.provenance != ModelCatalogProvenance::Cache)
        );
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn catalog_honors_standard_proxy_environment() {
    let (proxy_base_url, request_count, server) = spawn_catalog_server(
        200,
        r#"{"data":[{"id":"proxy-visible-model"}]}"#,
        Duration::ZERO,
    )
    .await;
    let proxy_url = proxy_base_url
        .strip_suffix("/v1")
        .expect("catalog fixture URL suffix");
    let _environment_guard = crate::test_env::lock();
    let _http_proxy = crate::test_env::EnvScope::set("HTTP_PROXY", proxy_url);
    let _http_proxy_lower = crate::test_env::EnvScope::set("http_proxy", proxy_url);
    let _https_proxy = crate::test_env::EnvScope::set("HTTPS_PROXY", proxy_url);
    let _https_proxy_lower = crate::test_env::EnvScope::set("https_proxy", proxy_url);
    let _no_proxy = crate::test_env::EnvScope::set("NO_PROXY", "");
    let _no_proxy_lower = crate::test_env::EnvScope::set("no_proxy", "");

    let root = local_catalog_root("http://127.0.0.1:9/v1".to_owned(), "configured-only");
    let connection = load_provider_connections(&root)
        .default_connection()
        .expect("default connection")
        .config
        .clone();
    let result = ProviderModelCatalogService::new(
        tempfile::tempdir().expect("cache root").keep(),
        Arc::new(FakeCredentialStore::default()),
        Arc::new(MapEnvironment::default()),
    )
    .expect("catalog service")
    .models(
        &root,
        ModelCatalogRequest {
            request_id: 1,
            connection_id: connection.id.clone(),
            draft_revision: 1,
            connection_fingerprint: connection_semantic_fingerprint(&connection),
            explicit_refresh: true,
        },
    )
    .await;

    assert_eq!(result.state, ModelCatalogState::Remote);
    assert_eq!(request_count.load(Ordering::SeqCst), 1);
    assert!(
        result
            .entries
            .iter()
            .any(|entry| entry.model_ref.model_id == "proxy-visible-model")
    );
    server.abort();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn catalog_single_flight_waiters_inherit_auth_failure_instead_of_stale_ready_cache() {
    let _environment_guard = crate::test_env::lock();
    let (base_url, request_count, server) = spawn_catalog_sequence_server(vec![
        (
            200,
            r#"{"data":[{"id":"private-deployment"}]}"#,
            Duration::ZERO,
        ),
        (401, r#"{"error":"expired"}"#, Duration::from_millis(100)),
    ])
    .await;
    let root = local_catalog_root(base_url, "private-deployment");
    let connection = load_provider_connections(&root)
        .default_connection()
        .expect("default connection")
        .config
        .clone();
    let fingerprint = connection_semantic_fingerprint(&connection);
    let service = ProviderModelCatalogService::new(
        tempfile::tempdir().expect("cache root").keep(),
        Arc::new(FakeCredentialStore::default()),
        Arc::new(MapEnvironment::default()),
    )
    .expect("catalog service");
    let request = |request_id| ModelCatalogRequest {
        request_id,
        connection_id: connection.id.clone(),
        draft_revision: 1,
        connection_fingerprint: fingerprint.clone(),
        explicit_refresh: true,
    };

    assert_eq!(
        service.models(&root, request(1)).await.state,
        ModelCatalogState::Remote
    );
    let (first, second) = tokio::join!(
        service.models(&root, request(2)),
        service.models(&root, request(3))
    );

    assert_eq!(request_count.load(Ordering::SeqCst), 2);
    assert_eq!(first.state, ModelCatalogState::AuthRejected);
    assert_eq!(second.state, ModelCatalogState::AuthRejected);
    assert!(first.entries.iter().any(|entry| {
        entry.model_ref.model_id == "private-deployment"
            && entry.provenance == ModelCatalogProvenance::Cache
            && entry.availability == ModelAvailability::Unverified
    }));
    assert_eq!(first.entries, second.entries);
    server.await.expect("catalog sequence server should finish");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn process_staged_catalog_cache_isolated_by_secret_account() {
    let _environment_guard = crate::test_env::lock();
    let (base_url, request_count, server) = spawn_catalog_server(
        200,
        r#"{"data":[{"id":"private-deployment"}]}"#,
        Duration::ZERO,
    )
    .await;
    let root = local_catalog_root(base_url, "private-deployment");
    let connection = load_provider_connections(&root)
        .default_connection()
        .expect("default connection")
        .config
        .clone();
    let fingerprint = connection_semantic_fingerprint(&connection);
    let cache_root = tempfile::tempdir().expect("cache root");
    let service = ProviderModelCatalogService::new(
        cache_root.path().to_path_buf(),
        Arc::new(FakeCredentialStore::default()),
        Arc::new(MapEnvironment::default()),
    )
    .expect("catalog service");
    let request = |request_id| ModelCatalogRequest {
        request_id,
        connection_id: connection.id.clone(),
        draft_revision: 1,
        connection_fingerprint: fingerprint.clone(),
        explicit_refresh: false,
    };

    let first = service
        .models_with_prepared_credential(
            &root,
            request(1),
            Some(&PreparedCredential::api_key(
                ProviderFamily::Custom,
                "account-a-secret",
            )),
        )
        .await;
    let second = service
        .models_with_prepared_credential(
            &root,
            request(2),
            Some(&PreparedCredential::api_key(
                ProviderFamily::Custom,
                "account-b-secret",
            )),
        )
        .await;

    assert_eq!(first.state, ModelCatalogState::Remote);
    assert_eq!(second.state, ModelCatalogState::Remote);
    assert_eq!(request_count.load(Ordering::SeqCst), 2);
    assert!(!cache_root.path().join("provider-models").exists());
    server.abort();
}

#[test]
fn catalog_pagination_rejects_repeated_empty_and_excessive_cursors() {
    assert!(super::catalog::admit_catalog_page(100).is_ok());
    assert!(super::catalog::admit_catalog_page(101).is_err());

    let mut seen = HashSet::new();
    assert_eq!(
        super::catalog::admit_catalog_next_cursor(&mut seen, Some("next".to_owned()))
            .expect("first cursor"),
        Some("next".to_owned())
    );
    assert!(super::catalog::admit_catalog_next_cursor(&mut seen, Some("next".to_owned())).is_err());
    assert!(super::catalog::admit_catalog_next_cursor(&mut seen, Some(String::new())).is_err());
}

fn write_catalog_cache_wire(
    cache_root: &Path,
    directory_connection_id: &str,
    fingerprint: &str,
    wire_connection_id: &str,
    entry_connection_id: &str,
    provenance: &str,
    stored_at_unix_secs: u64,
) -> PathBuf {
    let directory = cache_root
        .join("provider-models")
        .join("v1")
        .join(directory_connection_id);
    std::fs::create_dir_all(&directory).expect("cache directory should create");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut current = cache_root.to_path_buf();
        for component in ["provider-models", "v1", directory_connection_id] {
            std::fs::set_permissions(&current, std::fs::Permissions::from_mode(0o700))
                .expect("cache ancestor should be private");
            current.push(component);
        }
        std::fs::set_permissions(&current, std::fs::Permissions::from_mode(0o700))
            .expect("cache connection directory should be private");
    }
    let path = directory.join(format!("{fingerprint}.json"));
    let bytes = serde_json::to_vec(&json!({
        "version": 2,
        "connection_id": wire_connection_id,
        "fingerprint": fingerprint,
        "stored_at_unix_secs": stored_at_unix_secs,
        "entries": [{
            "model_ref": {
                "connection_id": entry_connection_id,
                "model_id": "private-deployment"
            },
            "display_name": "Private deployment",
            "availability": "available",
            "recommendation": "recommended",
            "provenance": provenance
        }]
    }))
    .expect("cache fixture should serialize");
    std::fs::write(&path, bytes).expect("cache fixture should write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("cache fixture should be private");
    }
    path
}

#[test]
fn persistent_catalog_cache_binds_exact_connection_fingerprint_and_remote_provenance() {
    let root = tempfile::tempdir().expect("cache root");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs();

    let valid = write_catalog_cache_wire(
        root.path(),
        "local",
        "valid",
        "local",
        "local",
        "remote",
        now,
    );
    let loaded = super::catalog_cache::load_catalog_cache(root.path(), "local", "valid")
        .expect("exact cache should load");
    assert_eq!(
        loaded.entries[0].recommendation,
        ModelRecommendation::Standard
    );
    assert!(valid.exists());

    let foreign = write_catalog_cache_wire(
        root.path(),
        "local",
        "foreign",
        "local",
        "another-connection",
        "remote",
        now,
    );
    assert!(super::catalog_cache::load_catalog_cache(root.path(), "local", "foreign").is_none());
    assert!(!foreign.exists());

    let forged = write_catalog_cache_wire(
        root.path(),
        "local",
        "forged",
        "local",
        "local",
        "bundled",
        now,
    );
    assert!(super::catalog_cache::load_catalog_cache(root.path(), "local", "forged").is_none());
    assert!(!forged.exists());
}

#[test]
fn persistent_catalog_cache_rejects_implausible_future_timestamps() {
    let root = tempfile::tempdir().expect("cache root");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs();
    let future = write_catalog_cache_wire(
        root.path(),
        "local",
        "future",
        "local",
        "local",
        "remote",
        now.saturating_add(10 * 60),
    );

    assert!(super::catalog_cache::load_catalog_cache(root.path(), "local", "future").is_none());
    assert!(!future.exists());
}

#[test]
fn catalog_cache_startup_sweep_removes_expired_unreferenced_fingerprints() {
    let root = tempfile::tempdir().expect("cache root");
    let expired = write_catalog_cache_wire(
        root.path(),
        "deleted-connection",
        "old-fingerprint",
        "deleted-connection",
        "deleted-connection",
        "remote",
        1,
    );

    super::catalog_cache::sweep_catalog_cache(root.path()).expect("cache sweep should succeed");
    assert!(!expired.exists());
}

#[cfg(unix)]
#[test]
fn catalog_cache_rejects_symlink_ancestor_without_mutating_the_target_tree() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let temp = tempfile::tempdir().expect("catalog ancestor root");
    let real_parent = temp.path().join("real-parent");
    let linked_parent = temp.path().join("linked-parent");
    let real_cache = real_parent.join("cache");
    std::fs::create_dir(&real_parent).expect("real parent should create");
    let expired = write_catalog_cache_wire(
        &real_cache,
        "external",
        "expired",
        "external",
        "external",
        "remote",
        1,
    );
    let v1 = real_cache.join("provider-models").join("v1");
    std::fs::set_permissions(&v1, std::fs::Permissions::from_mode(0o755))
        .expect("target cache mode should change");
    symlink(&real_parent, &linked_parent).expect("catalog parent symlink should create");
    let linked_cache = linked_parent.join("cache");

    assert!(super::catalog_cache::sweep_catalog_cache(&linked_cache).is_err());
    assert!(
        expired.exists(),
        "rejected sweep must not delete target files"
    );
    assert_eq!(
        std::fs::metadata(&v1)
            .expect("target cache metadata")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "rejected sweep must not chmod the target tree"
    );

    let model_ref = ModelRef::new(
        ConnectionId::new("new-connection").expect("connection id should parse"),
        "model",
    )
    .expect("model ref should parse");
    let entry = ModelCatalogEntry {
        model_ref,
        display_name: "Model".to_owned(),
        availability: ModelAvailability::Available,
        recommendation: ModelRecommendation::Standard,
        provenance: ModelCatalogProvenance::Remote,
    };
    assert!(
        super::catalog_cache::save_catalog_cache(
            &linked_cache,
            "new-connection",
            "fingerprint",
            &[entry],
        )
        .is_err()
    );
    assert!(
        !real_cache
            .join("provider-models/v1/new-connection/fingerprint.json")
            .exists(),
        "rejected save must not create through the linked ancestor"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn catalog_distinguishes_empty_auth_unsupported_and_malformed() {
    let _environment_guard = crate::test_env::lock();
    for (status, body, expected) in [
        (200, r#"{"data":[]}"#, ModelCatalogState::Empty),
        (
            401,
            r#"{"error":"secret body"}"#,
            ModelCatalogState::AuthRejected,
        ),
        (404, r#"{}"#, ModelCatalogState::Unsupported),
        (200, r#"{"data":[{}]}"#, ModelCatalogState::Malformed),
    ] {
        let (base_url, _, server) = spawn_catalog_server(status, body, Duration::ZERO).await;
        let root = local_catalog_root(base_url, "configured-only");
        let connection = load_provider_connections(&root)
            .default_connection()
            .expect("default connection")
            .config
            .clone();
        let result = ProviderModelCatalogService::new(
            tempfile::tempdir().expect("cache root").keep(),
            Arc::new(FakeCredentialStore::default()),
            Arc::new(MapEnvironment::default()),
        )
        .expect("catalog service")
        .models(
            &root,
            ModelCatalogRequest {
                request_id: status as u64,
                connection_id: connection.id.clone(),
                draft_revision: 1,
                connection_fingerprint: connection_semantic_fingerprint(&connection),
                explicit_refresh: true,
            },
        )
        .await;
        assert_eq!(result.state, expected);
        assert_eq!(
            result.manual_entry_allowed,
            matches!(
                expected,
                ModelCatalogState::Empty | ModelCatalogState::Unsupported
            )
        );
        server.abort();
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn staged_and_environment_catalogs_remain_process_memory_only() {
    let _environment_guard = crate::test_env::lock();
    let (base_url, request_count, server) = spawn_catalog_server(
        200,
        r#"{"data":[{"id":"private-deployment"}]}"#,
        Duration::ZERO,
    )
    .await;
    let root = local_catalog_root(base_url, "private-deployment");
    let loaded = load_provider_connections(&root);
    let connection = loaded.default_connection().expect("default connection");
    let temp = tempfile::tempdir().expect("cache root");
    let service = ProviderModelCatalogService::new(
        temp.path().to_path_buf(),
        Arc::new(FakeCredentialStore::default()),
        Arc::new(MapEnvironment::default()),
    )
    .expect("catalog service");
    let result = service
        .models_with_prepared_credential(
            &root,
            ModelCatalogRequest {
                request_id: 1,
                connection_id: connection.config.id.clone(),
                draft_revision: 7,
                connection_fingerprint: connection_semantic_fingerprint(&connection.config),
                explicit_refresh: true,
            },
            Some(&PreparedCredential::api_key(
                ProviderFamily::Custom,
                "staged-secret",
            )),
        )
        .await;

    assert_eq!(result.state, ModelCatalogState::Remote);
    assert!(
        result
            .entries
            .iter()
            .any(|entry| entry.model_ref.model_id == "private-deployment")
    );
    let cached = service
        .models_with_prepared_credential(
            &root,
            ModelCatalogRequest {
                request_id: 2,
                connection_id: connection.config.id.clone(),
                draft_revision: 7,
                connection_fingerprint: connection_semantic_fingerprint(&connection.config),
                explicit_refresh: false,
            },
            Some(&PreparedCredential::api_key(
                ProviderFamily::Custom,
                "staged-secret",
            )),
        )
        .await;
    assert_eq!(cached.state, ModelCatalogState::CacheFresh);
    assert_eq!(request_count.load(Ordering::SeqCst), 1);
    assert!(!temp.path().join("provider-models").exists());
    server.abort();
}

#[tokio::test]
async fn cow_migration_publishes_v2_then_deletes_no_longer_referenced_secret() {
    let root = legacy_root(
        "deepseek",
        "deepseek-private",
        r#"base_url = "https://api.deepseek.com"
api_key = "legacy-secret""#,
    );
    let loaded = load_provider_connections(&root);
    let default_model = loaded.default_model.clone().expect("default model");
    let connections = loaded
        .connections
        .into_iter()
        .map(|(id, loaded)| (id, loaded.config))
        .collect();
    let store = FakeCredentialStore::default();
    let publisher = FakePublisher::published(ConfigPublishOutcome::Published);
    let outcome = save_connection_config(
        &root,
        &unused_config_path(),
        ConnectionSaveDraft {
            connections,
            default_model,
            credential_updates: vec![ConnectionCredentialUpdate {
                connection_id: ConnectionId::new("deepseek-default").expect("connection id"),
                prepared: PreparedCredential::api_key(
                    ProviderFamily::DeepSeek,
                    "new-secret-canary",
                ),
            }],
            confirmed_legacy_environment: Default::default(),
        },
        &store,
        &publisher,
    )
    .await
    .expect("COW migration should succeed");

    assert_eq!(outcome.root_config.config_version, Some(CONFIG_VERSION_V2));
    assert!(outcome.root_config.providers.is_empty());
    let rendered =
        toml::to_string(&outcome.root_config).expect("V2 root config should serialize to TOML");
    assert!(!rendered.contains("legacy-secret"));
    assert!(!rendered.contains("new-secret-canary"));
    assert!(rendered.contains("source = \"stored\""));
    assert_eq!(store.records.lock().expect("records lock").len(), 1);
}

#[tokio::test]
async fn cow_migration_rejects_a_malformed_legacy_sibling_without_deleting_it() {
    let mut root = legacy_root(
        "deepseek",
        "deepseek-v4-flash",
        r#"base_url = "https://api.deepseek.com""#,
    );
    root.providers
        .insert("broken-sibling".to_owned(), json!("must-be-an-object"));
    let loaded = load_provider_connections(&root);
    assert!(!loaded.issues.is_empty());
    let result = save_connection_config(
        &root,
        &unused_config_path(),
        ConnectionSaveDraft {
            connections: loaded
                .connections
                .into_iter()
                .map(|(id, loaded)| (id, loaded.config))
                .collect(),
            default_model: loaded.default_model.expect("valid active default"),
            credential_updates: Vec::new(),
            confirmed_legacy_environment: Default::default(),
        },
        &FakeCredentialStore::default(),
        &FakePublisher::published(ConfigPublishOutcome::Published),
    )
    .await;
    assert!(matches!(
        result,
        Err(ConnectionSaveError::CurrentConfigInvalid)
    ));
    assert_eq!(
        root.providers.get("broken-sibling"),
        Some(&json!("must-be-an-object"))
    );
}

#[tokio::test]
async fn cow_not_published_removes_new_record_but_uncertain_publish_keeps_it() {
    for (publisher, expect_records, expect_error) in [
        (FakePublisher::failed(), 0, true),
        (
            FakePublisher::published(ConfigPublishOutcome::PublishedDurabilityUncertain),
            1,
            false,
        ),
        (
            FakePublisher::published(ConfigPublishOutcome::PublishedVisibilityUncertain {
                recovery_path: Some(PathBuf::from("sigil.previous")),
            }),
            1,
            false,
        ),
    ] {
        let root = legacy_root(
            "deepseek",
            "deepseek-v4-flash",
            r#"base_url = "https://api.deepseek.com""#,
        );
        let loaded = load_provider_connections(&root);
        let default_model = loaded.default_model.clone().expect("default model");
        let connections = loaded
            .connections
            .into_iter()
            .map(|(id, loaded)| (id, loaded.config))
            .collect();
        let store = FakeCredentialStore::default();
        let result = save_connection_config(
            &root,
            &unused_config_path(),
            ConnectionSaveDraft {
                connections,
                default_model,
                credential_updates: vec![ConnectionCredentialUpdate {
                    connection_id: ConnectionId::new("deepseek-default").expect("connection id"),
                    prepared: PreparedCredential::api_key(
                        ProviderFamily::DeepSeek,
                        "new-secret-canary",
                    ),
                }],
                confirmed_legacy_environment: Default::default(),
            },
            &store,
            &publisher,
        )
        .await;
        assert_eq!(result.is_err(), expect_error);
        assert_eq!(
            store.records.lock().expect("records lock").len(),
            expect_records
        );
    }
}

#[tokio::test]
async fn cow_surfaces_new_record_cleanup_failure_and_old_record_cleanup_warning() {
    let root = legacy_root(
        "deepseek",
        "deepseek-v4-flash",
        r#"base_url = "https://api.deepseek.com""#,
    );
    let loaded = load_provider_connections(&root);
    let default_model = loaded.default_model.clone().expect("default model");
    let connections = loaded
        .connections
        .into_iter()
        .map(|(id, loaded)| (id, loaded.config))
        .collect::<BTreeMap<_, _>>();
    let store = FakeCredentialStore::default();
    *store.fail_delete.lock().expect("delete flag") = true;
    let error = save_connection_config(
        &root,
        &unused_config_path(),
        ConnectionSaveDraft {
            connections: connections.clone(),
            default_model: default_model.clone(),
            credential_updates: vec![ConnectionCredentialUpdate {
                connection_id: ConnectionId::new("deepseek-default").expect("connection id"),
                prepared: PreparedCredential::api_key(ProviderFamily::DeepSeek, "new-secret"),
            }],
            confirmed_legacy_environment: Default::default(),
        },
        &store,
        &FakePublisher::failed(),
    )
    .await
    .expect_err("failed publish should report orphan cleanup");
    assert!(matches!(
        error,
        ConnectionSaveError::ConfigNotPublished {
            orphaned_credential: true,
            ..
        }
    ));

    *store.fail_delete.lock().expect("delete flag") = false;
    let old_id = CredentialId::random();
    let old_record = ProviderCredentialRecord::new(
        old_id.clone(),
        &PreparedCredential::api_key(ProviderFamily::DeepSeek, "old-secret"),
    );
    store.store(&old_record).await.expect("old record");
    let mut current_connection = connections
        .get(&ConnectionId::new("deepseek-default").expect("connection id"))
        .expect("projected connection")
        .clone();
    current_connection.credential = CredentialRefConfig::SystemKeyring { id: old_id.clone() };
    let current = materialize_v2_root_config(
        &root,
        &BTreeMap::from([(current_connection.id.clone(), current_connection.clone())]),
        &default_model,
    )
    .expect("current V2 config");
    *store.fail_delete.lock().expect("delete flag") = true;
    let outcome = save_connection_config(
        &current,
        &unused_config_path(),
        ConnectionSaveDraft {
            connections: BTreeMap::from([(current_connection.id.clone(), current_connection)]),
            default_model,
            credential_updates: vec![ConnectionCredentialUpdate {
                connection_id: ConnectionId::new("deepseek-default").expect("connection id"),
                prepared: PreparedCredential::api_key(ProviderFamily::DeepSeek, "replacement"),
            }],
            confirmed_legacy_environment: Default::default(),
        },
        &store,
        &FakePublisher::published(ConfigPublishOutcome::Published),
    )
    .await
    .expect("published config should retain cleanup warning");
    assert!(outcome.old_credential_cleanup_warning);
}

#[tokio::test]
async fn cow_post_write_store_error_attempts_orphan_cleanup() {
    let root = legacy_root(
        "deepseek",
        "deepseek-v4-flash",
        r#"base_url = "https://api.deepseek.com""#,
    );
    let loaded = load_provider_connections(&root);
    let store = FakeCredentialStore::default();
    *store
        .fail_store_after_write
        .lock()
        .expect("store failure flag") = true;

    let error = save_connection_config(
        &root,
        &unused_config_path(),
        ConnectionSaveDraft {
            connections: loaded
                .connections
                .into_iter()
                .map(|(id, loaded)| (id, loaded.config))
                .collect(),
            default_model: loaded.default_model.expect("default model"),
            credential_updates: vec![ConnectionCredentialUpdate {
                connection_id: ConnectionId::new("deepseek-default").expect("connection id"),
                prepared: PreparedCredential::api_key(
                    ProviderFamily::DeepSeek,
                    "post-write-secret",
                ),
            }],
            confirmed_legacy_environment: Default::default(),
        },
        &store,
        &FakePublisher::published(ConfigPublishOutcome::Published),
    )
    .await
    .expect_err("post-write failure must abort");

    assert!(matches!(
        error,
        ConnectionSaveError::CredentialStoreWrite {
            orphaned_credential: false,
            ..
        }
    ));
    assert!(store.records.lock().expect("records lock").is_empty());
}

#[tokio::test]
async fn cow_never_deletes_a_keyring_record_still_referenced_by_a_sibling() {
    let old_id = CredentialId::random();
    let store = FakeCredentialStore::default();
    store
        .store(&ProviderCredentialRecord::new(
            old_id.clone(),
            &PreparedCredential::api_key(ProviderFamily::DeepSeek, "shared-old-secret"),
        ))
        .await
        .expect("old record");
    let mut primary = deepseek_connection();
    primary.credential = CredentialRefConfig::SystemKeyring { id: old_id.clone() };
    let mut sibling = primary.clone();
    sibling.id = ConnectionId::new("deepseek-sibling").expect("connection id");
    sibling.label = "DeepSeek sibling".to_owned();
    let default_model =
        ModelRef::new(primary.id.clone(), "deepseek-v4-flash").expect("default model");
    let connections = BTreeMap::from([
        (primary.id.clone(), primary.clone()),
        (sibling.id.clone(), sibling.clone()),
    ]);
    let current = materialize_v2_root_config(
        &legacy_root(
            "deepseek",
            "deepseek-v4-flash",
            r#"base_url = "https://api.deepseek.com""#,
        ),
        &connections,
        &default_model,
    )
    .expect("current V2 config");

    let outcome = save_connection_config(
        &current,
        &unused_config_path(),
        ConnectionSaveDraft {
            connections,
            default_model,
            credential_updates: vec![ConnectionCredentialUpdate {
                connection_id: primary.id,
                prepared: PreparedCredential::api_key(
                    ProviderFamily::DeepSeek,
                    "replacement-secret",
                ),
            }],
            confirmed_legacy_environment: Default::default(),
        },
        &store,
        &FakePublisher::published(ConfigPublishOutcome::Published),
    )
    .await
    .expect("shared record update should publish");

    assert!(!outcome.old_credential_cleanup_warning);
    assert!(
        store
            .records
            .lock()
            .expect("records lock")
            .contains_key(&old_id)
    );
}

#[tokio::test]
async fn cow_rejects_a_stale_config_snapshot_before_writing_credentials() {
    let connection = deepseek_connection();
    let default_model =
        ModelRef::new(connection.id.clone(), "deepseek-v4-flash").expect("default model");
    let current = materialize_v2_root_config(
        &legacy_root(
            "deepseek",
            "deepseek-v4-flash",
            r#"base_url = "https://api.deepseek.com""#,
        ),
        &BTreeMap::from([(connection.id.clone(), connection.clone())]),
        &default_model,
    )
    .expect("current V2 config");
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("sigil.toml");
    current.save(&path).expect("current config should save");
    let mut concurrent = current.clone();
    concurrent.permission.mode = sigil_kernel::PermissionMode::ReadOnly;
    concurrent
        .save(&path)
        .expect("concurrent update should save");
    let store = FakeCredentialStore::default();

    let result = save_connection_config(
        &current,
        &path,
        ConnectionSaveDraft {
            connections: BTreeMap::from([(connection.id.clone(), connection.clone())]),
            default_model,
            credential_updates: vec![ConnectionCredentialUpdate {
                connection_id: connection.id,
                prepared: PreparedCredential::api_key(
                    ProviderFamily::DeepSeek,
                    "must-not-be-written",
                ),
            }],
            confirmed_legacy_environment: Default::default(),
        },
        &store,
        &FakePublisher::published(ConfigPublishOutcome::Published),
    )
    .await;

    assert!(matches!(
        result,
        Err(ConnectionSaveError::ConcurrentModification)
    ));
    assert!(store.records.lock().expect("records lock").is_empty());
}

#[tokio::test]
async fn cow_deletes_keyring_record_after_connection_removal_is_published() {
    let retired_id = CredentialId::random();
    let store = FakeCredentialStore::default();
    store
        .store(&ProviderCredentialRecord::new(
            retired_id.clone(),
            &PreparedCredential::api_key(ProviderFamily::DeepSeek, "retired-secret"),
        ))
        .await
        .expect("retired record");
    let mut retired = deepseek_connection();
    retired.id = ConnectionId::new("deepseek-retired").expect("connection id");
    retired.label = "DeepSeek retired".to_owned();
    retired.credential = CredentialRefConfig::SystemKeyring {
        id: retired_id.clone(),
    };
    let active = deepseek_connection();
    let default_model =
        ModelRef::new(active.id.clone(), "deepseek-v4-flash").expect("default model");
    let current_connections = BTreeMap::from([
        (active.id.clone(), active.clone()),
        (retired.id.clone(), retired),
    ]);
    let current = materialize_v2_root_config(
        &legacy_root(
            "deepseek",
            "deepseek-v4-flash",
            r#"base_url = "https://api.deepseek.com""#,
        ),
        &current_connections,
        &default_model,
    )
    .expect("current V2 config");

    let outcome = save_connection_config(
        &current,
        &unused_config_path(),
        ConnectionSaveDraft {
            connections: BTreeMap::from([(active.id.clone(), active)]),
            default_model,
            credential_updates: Vec::new(),
            confirmed_legacy_environment: Default::default(),
        },
        &store,
        &FakePublisher::published(ConfigPublishOutcome::Published),
    )
    .await
    .expect("connection removal should publish");

    assert!(!outcome.old_credential_cleanup_warning);
    assert!(
        !store
            .records
            .lock()
            .expect("records lock")
            .contains_key(&retired_id)
    );
}

#[tokio::test]
async fn cow_readback_failure_cleans_new_record_or_reports_orphan() {
    for cleanup_fails in [false, true] {
        let root = legacy_root(
            "deepseek",
            "deepseek-v4-flash",
            r#"base_url = "https://api.deepseek.com""#,
        );
        let loaded = load_provider_connections(&root);
        let store = FakeCredentialStore::default();
        *store.fail_load.lock().expect("load flag") = true;
        *store.fail_delete.lock().expect("delete flag") = cleanup_fails;
        let error = save_connection_config(
            &root,
            &unused_config_path(),
            ConnectionSaveDraft {
                connections: loaded
                    .connections
                    .into_iter()
                    .map(|(id, loaded)| (id, loaded.config))
                    .collect(),
                default_model: loaded.default_model.expect("default model"),
                credential_updates: vec![ConnectionCredentialUpdate {
                    connection_id: ConnectionId::new("deepseek-default").expect("connection id"),
                    prepared: PreparedCredential::api_key(
                        ProviderFamily::DeepSeek,
                        "readback-secret",
                    ),
                }],
                confirmed_legacy_environment: Default::default(),
            },
            &store,
            &FakePublisher::published(ConfigPublishOutcome::Published),
        )
        .await
        .expect_err("readback failure should abort");
        assert!(matches!(
            error,
            ConnectionSaveError::CredentialReadBackMismatch {
                orphaned_credential
            } if orphaned_credential == cleanup_fails
        ));
    }
}

#[test]
fn materialized_v2_config_contains_only_compound_default_and_connection_refs() {
    let connection = deepseek_connection();
    let id = connection.id.clone();
    let root = legacy_root(
        "deepseek",
        "deepseek-v4-flash",
        r#"base_url = "https://api.deepseek.com""#,
    );
    let model_ref = ModelRef::new(id.clone(), "deepseek-v4-pro").expect("model ref");
    let v2 = materialize_v2_root_config(
        &root,
        &BTreeMap::from([(id.clone(), connection)]),
        &model_ref,
    )
    .expect("V2 config should materialize");
    assert_eq!(v2.agent.connection, Some(id));
    assert!(v2.agent.provider.is_empty());
    assert!(v2.providers.is_empty());
    assert_eq!(v2.agent.model, "deepseek-v4-pro");
}
