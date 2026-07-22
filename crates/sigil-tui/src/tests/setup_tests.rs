use super::*;
use crate::test_env::EnvScope;
use sigil_runtime::DEFAULT_SETUP_API_KEY_ENV;

#[test]
fn setup_field_navigation_wraps() {
    assert_eq!(SetupField::Provider.next(), SetupField::Model);
    assert_eq!(SetupField::Save.next(), SetupField::Provider);
    assert_eq!(SetupField::Provider.previous(), SetupField::Save);
}

#[test]
fn setup_field_index_and_labels_cover_all_values() {
    assert_eq!(SetupField::from_index(0), Some(SetupField::Provider));
    assert_eq!(SetupField::from_index(1), Some(SetupField::Model));
    assert_eq!(SetupField::from_index(2), Some(SetupField::ApiKey));
    assert_eq!(SetupField::from_index(3), Some(SetupField::Save));
    assert_eq!(SetupField::from_index(4), None);

    assert_eq!(SetupField::Provider.label(), "provider");
    assert_eq!(SetupField::Model.label(), "model");
    assert_eq!(SetupField::ApiKey.label(), "api_key");
    assert_eq!(SetupField::Save.label(), "save");
}

#[test]
fn setup_state_masks_api_key() {
    let mut state = SetupState::new(PathBuf::from("/tmp/sigil.toml"), None);

    assert_eq!(state.masked_api_key(), "<empty>");

    state.api_key = "secret".to_owned();
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
    state.api_key = "deepseek-key".to_owned();

    state.cycle_provider();
    assert_eq!(state.provider_name, "openai_compat");
    assert_eq!(state.model, "gpt-4.1");
    assert!(state.api_key.is_empty());

    state.model = "openai-custom".to_owned();
    state.api_key = "openai-key".to_owned();
    for _ in 0..4 {
        state.cycle_provider();
    }

    assert_eq!(state.provider_name, "deepseek");
    assert_eq!(state.model, "deepseek-custom");
    assert_eq!(state.api_key, "deepseek-key");

    state.cycle_provider();
    assert_eq!(state.model, "openai-custom");
    assert_eq!(state.api_key, "openai-key");
}

#[test]
fn setup_auth_summary_prefers_pending_plaintext_key() {
    let mut state = SetupState::new(PathBuf::from("/tmp/sigil.toml"), None);

    state.api_key = "  secret  ".to_owned();

    assert_eq!(state.auth_summary(), "plaintext api_key pending save");
}

#[test]
fn setup_auth_summary_reports_env_key_when_present() {
    let _guard = crate::test_env::lock();
    let _env = EnvScope::set(DEFAULT_SETUP_API_KEY_ENV, "secret");
    let state = SetupState::new(PathBuf::from("/tmp/sigil.toml"), None);

    assert_eq!(
        state.auth_summary(),
        format!("env {DEFAULT_SETUP_API_KEY_ENV}")
    );
}
