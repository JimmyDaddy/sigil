use super::*;
use crate::test_env::EnvScope;
use sigil_runtime::DEFAULT_SETUP_API_KEY_ENV;

#[test]
fn setup_field_navigation_wraps_for_standard_and_custom_flows() {
    assert_eq!(SetupField::Provider.next(false), SetupField::ApiKey);
    assert_eq!(SetupField::Save.next(false), SetupField::Provider);
    assert_eq!(SetupField::Provider.previous(false), SetupField::Save);
    assert_eq!(SetupField::Provider.next(true), SetupField::Protocol);
    assert_eq!(SetupField::Protocol.next(true), SetupField::Endpoint);
}

#[test]
fn setup_field_index_and_labels_cover_standard_and_custom_values() {
    assert_eq!(SetupField::from_index(0, false), Some(SetupField::Provider));
    assert_eq!(SetupField::from_index(1, false), Some(SetupField::ApiKey));
    assert_eq!(SetupField::from_index(2, false), Some(SetupField::Model));
    assert_eq!(
        SetupField::from_index(3, false),
        Some(SetupField::ContextWindow)
    );
    assert_eq!(SetupField::from_index(4, false), Some(SetupField::Save));
    assert_eq!(SetupField::from_index(5, false), None);
    assert_eq!(SetupField::from_index(1, true), Some(SetupField::Protocol));
    assert_eq!(SetupField::from_index(2, true), Some(SetupField::Endpoint));
    assert_eq!(
        SetupField::from_index(5, true),
        Some(SetupField::ContextWindow)
    );
    assert_eq!(SetupField::from_index(6, true), Some(SetupField::Save));

    assert_eq!(SetupField::Provider.label(), "provider");
    assert_eq!(SetupField::Protocol.label(), "protocol");
    assert_eq!(SetupField::Endpoint.label(), "endpoint");
    assert_eq!(SetupField::ApiKey.label(), "authentication");
    assert_eq!(SetupField::Model.label(), "model");
    assert_eq!(SetupField::ContextWindow.label(), "context window");
    assert_eq!(SetupField::Save.label(), "review");
}

#[test]
fn setup_state_masks_staged_secure_store_secret() {
    let mut state = SetupState::new(PathBuf::from("/tmp/sigil.toml"), None);

    assert_eq!(state.masked_api_key(), "<not staged>");

    state.api_key = SecretString::new("secret");
    assert_eq!(state.masked_api_key(), "********");
}

#[test]
fn setup_state_starts_on_provider_field_and_keeps_startup_error() {
    let state = SetupState::new(
        PathBuf::from("/tmp/sigil.toml"),
        Some("failed to load config".to_owned()),
    );

    assert_eq!(state.config_path, PathBuf::from("/tmp/sigil.toml"));
    assert_eq!(state.selected_field, SetupField::Provider);
    assert_eq!(state.provider_name, "deepseek");
    assert_eq!(state.model, "deepseek-v4-flash");
    assert_eq!(
        state.startup_error.as_deref(),
        Some("failed to load config")
    );
}

#[test]
fn setup_provider_cycle_uses_provider_defaults_and_restores_drafts() {
    let mut state = SetupState::new(PathBuf::from("/tmp/sigil.toml"), None);
    state.model = "deepseek-custom".to_owned();
    state.context_window_tokens = "262144".to_owned();
    state.api_key = SecretString::new("deepseek-key");
    state.credential_source = SetupCredentialSource::SecureStore;

    state.cycle_provider();
    assert_eq!(state.provider_name, "openai_responses");
    assert_eq!(state.model, "gpt-4.1");
    assert!(state.context_window_tokens.is_empty());
    assert!(state.api_key.is_empty());

    state.model = "openai-custom".to_owned();
    state.api_key = SecretString::new("openai-key");
    state.credential_source = SetupCredentialSource::SecureStore;
    for _ in 0..4 {
        state.cycle_provider();
    }

    assert_eq!(state.provider_name, "deepseek");
    assert_eq!(state.model, "deepseek-custom");
    assert_eq!(state.context_window_tokens, "262144");
    assert_eq!(state.api_key, "deepseek-key");

    state.cycle_provider();
    assert_eq!(state.model, "openai-custom");
    assert_eq!(state.api_key, "openai-key");
}

#[test]
fn setup_auth_summary_reports_staged_secure_store_without_plaintext() {
    let mut state = SetupState::new(PathBuf::from("/tmp/sigil.toml"), None);
    state.credential_source = SetupCredentialSource::SecureStore;
    state.api_key = SecretString::new("  secret  ");

    let summary = state.auth_summary();
    assert_eq!(summary, "protected store · credential staged in memory");
    assert!(!summary.contains("secret"));
}

#[test]
fn setup_auth_summary_reports_detected_env_reference_without_value() {
    let _guard = crate::test_env::lock();
    let _env = EnvScope::set(DEFAULT_SETUP_API_KEY_ENV, "secret");
    let state = SetupState::new(PathBuf::from("/tmp/sigil.toml"), None);

    assert_eq!(
        state.auth_summary(),
        format!("environment {DEFAULT_SETUP_API_KEY_ENV} detected")
    );
    assert!(!state.auth_summary().contains("secret"));
}

#[test]
fn custom_protocol_switch_detects_the_protocol_specific_environment() {
    let _guard = crate::test_env::lock();
    let _responses = EnvScope::set("SIGIL_OPENAI_RESPONSES_API_KEY", "responses-secret");
    let _chat = EnvScope::unset("SIGIL_OPENAI_COMPATIBLE_API_KEY");
    let mut state = SetupState::new(PathBuf::from("/tmp/sigil.toml"), None);
    for _ in 0..4 {
        state.cycle_provider();
    }

    assert!(state.is_custom());
    assert_eq!(state.protocol, ProviderProtocol::OpenAiChatCompletions);
    assert_eq!(state.credential_source, SetupCredentialSource::SecureStore);

    state.cycle_protocol();

    assert_eq!(state.protocol, ProviderProtocol::OpenAiResponses);
    assert_eq!(
        state.api_key_env_name(),
        Some("SIGIL_OPENAI_RESPONSES_API_KEY")
    );
    assert_eq!(state.credential_source, SetupCredentialSource::Environment);

    state.cycle_protocol();
    assert_eq!(state.protocol, ProviderProtocol::OpenAiChatCompletions);
    assert_eq!(state.credential_source, SetupCredentialSource::SecureStore);
}
