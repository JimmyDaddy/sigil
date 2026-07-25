#![cfg(test)]

use sigil_runtime::{ProviderConfigFields, default_provider_config_fields, next_provider_name};

pub(super) type ProviderFieldDraft = ProviderConfigFields;

pub(super) fn default_provider_field_draft(provider_name: &str, model: &str) -> ProviderFieldDraft {
    default_provider_config_fields(provider_name, model)
}

pub(crate) fn cycle_provider_name(provider: &str) -> String {
    next_provider_name(provider).to_owned()
}
