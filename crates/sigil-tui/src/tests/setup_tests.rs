use super::*;
use std::{
    ffi::OsString,
    sync::{Mutex, OnceLock},
};

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[test]
fn setup_field_navigation_wraps() {
    assert_eq!(SetupField::TrustCurrentFolder.next(), SetupField::Model);
    assert_eq!(SetupField::Save.next(), SetupField::TrustCurrentFolder);
    assert_eq!(SetupField::TrustCurrentFolder.previous(), SetupField::Save);
}

#[test]
fn setup_field_index_and_labels_cover_all_values() {
    assert_eq!(
        SetupField::from_index(0),
        Some(SetupField::TrustCurrentFolder)
    );
    assert_eq!(SetupField::from_index(1), Some(SetupField::Model));
    assert_eq!(SetupField::from_index(2), Some(SetupField::ApiKey));
    assert_eq!(SetupField::from_index(3), Some(SetupField::Save));
    assert_eq!(SetupField::from_index(4), None);

    assert_eq!(
        SetupField::TrustCurrentFolder.label(),
        "trust_current_folder"
    );
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
fn setup_state_starts_on_trust_field_and_keeps_startup_error() {
    let state = SetupState::new(
        PathBuf::from("/tmp/sigil.toml"),
        Some("failed to load config".to_owned()),
    );

    assert_eq!(state.config_path, PathBuf::from("/tmp/sigil.toml"));
    assert_eq!(state.selected_field, SetupField::TrustCurrentFolder);
    assert_eq!(state.model, "deepseek-v4-flash");
    assert!(!state.trusted_current_folder);
    assert_eq!(
        state.startup_error.as_deref(),
        Some("failed to load config")
    );
}

#[test]
fn setup_auth_summary_prefers_pending_plaintext_key() {
    let mut state = SetupState::new(PathBuf::from("/tmp/sigil.toml"), None);

    state.api_key = "  secret  ".to_owned();

    assert_eq!(state.auth_summary(), "plaintext api_key pending save");
}

#[test]
fn setup_auth_summary_reports_env_key_when_present() {
    let _guard = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env lock should not be poisoned");
    let _env = EnvScope::set(DEFAULT_SETUP_API_KEY_ENV, "secret");
    let state = SetupState::new(PathBuf::from("/tmp/sigil.toml"), None);

    assert_eq!(
        state.auth_summary(),
        format!("env {DEFAULT_SETUP_API_KEY_ENV}")
    );
}

struct EnvScope {
    name: &'static str,
    saved: Option<OsString>,
}

impl EnvScope {
    fn set(name: &'static str, value: &str) -> Self {
        let saved = std::env::var_os(name);
        // SAFETY: this test serializes process-wide env mutation with ENV_LOCK.
        unsafe { std::env::set_var(name, value) };
        Self { name, saved }
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        match self.saved.take() {
            Some(value) => {
                // SAFETY: this test serializes process-wide env mutation with ENV_LOCK.
                unsafe { std::env::set_var(self.name, value) };
            }
            None => {
                // SAFETY: this test serializes process-wide env mutation with ENV_LOCK.
                unsafe { std::env::remove_var(self.name) };
            }
        }
    }
}
