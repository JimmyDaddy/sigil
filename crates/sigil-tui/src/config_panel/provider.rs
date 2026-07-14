use std::collections::BTreeMap;

use sigil_kernel::RootConfig;
use sigil_runtime::{
    ANTHROPIC_PROVIDER_KEY, DEEPSEEK_PROVIDER_KEY, GEMINI_PROVIDER_KEY, OPENAI_COMPAT_PROVIDER_KEY,
    OPENAI_RESPONSES_PROVIDER_KEY, ProviderConfigFields, default_provider_config_fields,
    next_provider_name, normalize_provider_name, provider_config_fields,
};

use super::ConfigDraft;

pub(super) type ProviderFieldDraft = ProviderConfigFields;

pub(super) fn provider_drafts_from_root_config(
    root_config: &RootConfig,
) -> BTreeMap<String, ProviderFieldDraft> {
    let mut provider_drafts = BTreeMap::new();
    provider_drafts.insert(
        DEEPSEEK_PROVIDER_KEY.to_owned(),
        provider_config_fields(root_config, DEEPSEEK_PROVIDER_KEY, &root_config.agent.model),
    );
    provider_drafts.insert(
        OPENAI_COMPAT_PROVIDER_KEY.to_owned(),
        provider_config_fields(
            root_config,
            OPENAI_COMPAT_PROVIDER_KEY,
            &root_config.agent.model,
        ),
    );
    provider_drafts.insert(
        OPENAI_RESPONSES_PROVIDER_KEY.to_owned(),
        provider_config_fields(
            root_config,
            OPENAI_RESPONSES_PROVIDER_KEY,
            &root_config.agent.model,
        ),
    );
    provider_drafts.insert(
        ANTHROPIC_PROVIDER_KEY.to_owned(),
        provider_config_fields(
            root_config,
            ANTHROPIC_PROVIDER_KEY,
            &root_config.agent.model,
        ),
    );
    provider_drafts.insert(
        GEMINI_PROVIDER_KEY.to_owned(),
        provider_config_fields(root_config, GEMINI_PROVIDER_KEY, &root_config.agent.model),
    );
    provider_drafts
}

pub(super) fn current_provider_field_draft(
    root_config: &RootConfig,
    provider_name: &str,
    provider_drafts: &BTreeMap<String, ProviderFieldDraft>,
) -> ProviderFieldDraft {
    provider_drafts
        .get(provider_name)
        .cloned()
        .unwrap_or_else(|| default_provider_field_draft(provider_name, &root_config.agent.model))
}

pub(super) fn default_provider_field_draft(provider_name: &str, model: &str) -> ProviderFieldDraft {
    default_provider_config_fields(provider_name, model)
}

pub(crate) fn cycle_provider_name(provider: &str) -> String {
    next_provider_name(provider).to_owned()
}

impl ConfigDraft {
    pub(crate) fn cycle_provider(&mut self) {
        self.capture_current_provider_draft();
        let provider_name = cycle_provider_name(&self.provider_name);
        self.provider_name = provider_name.clone();
        self.load_provider_draft(&provider_name);
    }

    fn capture_current_provider_draft(&mut self) {
        let provider_name = normalize_provider_name(&self.provider_name).to_owned();
        self.provider_drafts.insert(
            provider_name,
            ProviderFieldDraft {
                model: self.provider_model.clone(),
                api_key: self.provider_api_key.clone(),
                base_url: self.provider_base_url.clone(),
            },
        );
    }

    fn load_provider_draft(&mut self, provider_name: &str) {
        let provider_name = normalize_provider_name(provider_name);
        let draft = self
            .provider_drafts
            .get(provider_name)
            .cloned()
            .unwrap_or_else(|| {
                default_provider_field_draft(provider_name, &self.base_root_config.agent.model)
            });
        self.provider_model = draft.model;
        self.provider_api_key = draft.api_key;
        self.provider_base_url = draft.base_url;
    }
}
