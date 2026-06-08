use std::{env, path::PathBuf};

use termquill_provider_deepseek::TERMQUILL_API_KEY_ENV;

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
            return "inline api_key pending save".to_owned();
        }
        if env::var(TERMQUILL_API_KEY_ENV).is_ok() {
            return format!("env {TERMQUILL_API_KEY_ENV}");
        }

        "missing".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_field_navigation_wraps() {
        assert_eq!(SetupField::TrustCurrentFolder.next(), SetupField::Model);
        assert_eq!(SetupField::Save.next(), SetupField::TrustCurrentFolder);
        assert_eq!(SetupField::TrustCurrentFolder.previous(), SetupField::Save);
    }

    #[test]
    fn setup_state_masks_api_key() {
        let mut state = SetupState::new(PathBuf::from("/tmp/termquill.toml"), None);

        assert_eq!(state.masked_api_key(), "<empty>");

        state.api_key = "secret".to_owned();
        assert_eq!(state.masked_api_key(), "********");
    }
}
