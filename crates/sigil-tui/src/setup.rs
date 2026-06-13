use std::{env, path::PathBuf};

use sigil_provider_deepseek::SIGIL_API_KEY_ENV;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetupField {
    TrustCurrentFolder,
    Model,
    ApiKey,
    Save,
}

impl SetupField {
    const ORDER: [Self; 4] = [
        Self::TrustCurrentFolder,
        Self::Model,
        Self::ApiKey,
        Self::Save,
    ];

    pub(crate) fn next(self) -> Self {
        let index = Self::ORDER
            .iter()
            .position(|field| *field == self)
            .expect("setup field must exist in the ordered list");
        Self::ORDER[(index + 1) % Self::ORDER.len()]
    }

    pub(crate) fn previous(self) -> Self {
        let index = Self::ORDER
            .iter()
            .position(|field| *field == self)
            .expect("setup field must exist in the ordered list");
        if index == 0 {
            *Self::ORDER.last().expect("setup fields are non-empty")
        } else {
            Self::ORDER[index - 1]
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SetupState {
    pub(crate) config_path: PathBuf,
    pub(crate) selected_field: SetupField,
    pub(crate) model: String,
    pub(crate) api_key: String,
    pub(crate) trusted_current_folder: bool,
    pub(crate) startup_error: Option<String>,
}

impl SetupState {
    pub(crate) fn new(config_path: PathBuf, startup_error: Option<String>) -> Self {
        Self {
            config_path,
            selected_field: SetupField::TrustCurrentFolder,
            model: "deepseek-v4-flash".to_owned(),
            api_key: String::new(),
            trusted_current_folder: false,
            startup_error,
        }
    }

    pub(crate) fn masked_api_key(&self) -> String {
        if self.api_key.is_empty() {
            "<empty>".to_owned()
        } else {
            "*".repeat(self.api_key.chars().count().max(8))
        }
    }

    pub(crate) fn auth_summary(&self) -> String {
        if !self.api_key.trim().is_empty() {
            return "plaintext api_key pending save".to_owned();
        }
        if env::var(SIGIL_API_KEY_ENV).is_ok() {
            return format!("env {SIGIL_API_KEY_ENV}");
        }

        "missing".to_owned()
    }
}

#[cfg(test)]
#[path = "tests/setup_tests.rs"]
mod tests;
