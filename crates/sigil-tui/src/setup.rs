use std::{collections::BTreeMap, env, path::PathBuf};

use sigil_runtime::{
    DEFAULT_SETUP_PROVIDER_KEY, default_provider_model, next_provider_name,
    provider_api_key_env_name,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetupField {
    Provider,
    Model,
    ApiKey,
    Save,
}

impl SetupField {
    const ORDER: [Self; 4] = [Self::Provider, Self::Model, Self::ApiKey, Self::Save];

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

    pub(crate) fn from_index(index: usize) -> Option<Self> {
        Self::ORDER.get(index).copied()
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Model => "model",
            Self::ApiKey => "api_key",
            Self::Save => "save",
        }
    }
}

#[derive(Debug, Clone)]
struct SetupProviderDraft {
    model: String,
    api_key: String,
}

#[derive(Debug, Clone)]
pub(crate) struct SetupState {
    pub(crate) config_path: PathBuf,
    pub(crate) selected_field: SetupField,
    pub(crate) provider_name: String,
    pub(crate) model: String,
    pub(crate) api_key: String,
    pub(crate) startup_error: Option<String>,
    provider_drafts: BTreeMap<String, SetupProviderDraft>,
}

impl SetupState {
    pub(crate) fn new(config_path: PathBuf, startup_error: Option<String>) -> Self {
        let provider_name = DEFAULT_SETUP_PROVIDER_KEY.to_owned();
        Self {
            config_path,
            selected_field: SetupField::Provider,
            model: default_provider_model(&provider_name)
                .expect("default setup provider must have a default model"),
            api_key: String::new(),
            provider_name,
            startup_error,
            provider_drafts: BTreeMap::new(),
        }
    }

    pub(crate) fn cycle_provider(&mut self) {
        self.provider_drafts.insert(
            self.provider_name.clone(),
            SetupProviderDraft {
                model: self.model.clone(),
                api_key: self.api_key.clone(),
            },
        );
        self.provider_name = next_provider_name(&self.provider_name).to_owned();
        let draft = self
            .provider_drafts
            .get(&self.provider_name)
            .cloned()
            .unwrap_or_else(|| SetupProviderDraft {
                model: default_provider_model(&self.provider_name)
                    .expect("supported setup provider must have a default model"),
                api_key: String::new(),
            });
        self.model = draft.model;
        self.api_key = draft.api_key;
    }

    pub(crate) fn api_key_env_name(&self) -> Option<&'static str> {
        provider_api_key_env_name(&self.provider_name)
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
        if let Some(env_name) = self.api_key_env_name()
            && env::var(env_name).is_ok()
        {
            return format!("env {env_name}");
        }

        "missing".to_owned()
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/setup_tests.rs"]
mod tests;
