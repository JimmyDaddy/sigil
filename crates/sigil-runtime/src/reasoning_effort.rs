use std::fmt::Write as _;

use sha2::{Digest, Sha256};
use sigil_kernel::{ReasoningEffort, RootConfig};
use sigil_provider_deepseek::deepseek_reasoning_efforts;
use sigil_provider_openai_responses::openai_responses_reasoning_efforts;

use crate::{
    DEEPSEEK_PROVIDER_KEY, OPENAI_RESPONSES_PROVIDER_KEY, load_deepseek_config,
    normalize_provider_name,
};

const REASONING_EFFORT_BINDING_DOMAIN: &[u8] = b"sigil-reasoning-effort-binding-v1";

#[must_use]
pub(crate) fn supported_reasoning_efforts(
    provider_name: &str,
    model_name: &str,
) -> Vec<ReasoningEffort> {
    match normalize_provider_name(provider_name) {
        DEEPSEEK_PROVIDER_KEY => deepseek_reasoning_efforts(model_name),
        OPENAI_RESPONSES_PROVIDER_KEY => openai_responses_reasoning_efforts(model_name),
        _ => Vec::new(),
    }
}

/// Keeps a requested effort only when the exact provider/model pair has a local capability
/// mapping for it. Unknown models and providers omit the field instead of guessing support.
#[must_use]
pub fn admitted_reasoning_effort(
    provider_name: &str,
    model_name: &str,
    requested: Option<ReasoningEffort>,
) -> Option<ReasoningEffort> {
    let supported = supported_reasoning_efforts(provider_name, model_name);
    requested.filter(|effort| supported.contains(effort))
}

#[must_use]
pub(crate) fn configured_default_reasoning_effort(
    root_config: &RootConfig,
) -> Option<ReasoningEffort> {
    let provider = crate::provider_connections::resolve_default_model_route(root_config)
        .ok()
        .map(|(provider, _)| provider)?;
    let supported = supported_reasoning_efforts(&provider, &root_config.agent.model);
    let configured = match normalize_provider_name(&provider) {
        DEEPSEEK_PROVIDER_KEY => Some(
            load_deepseek_config(root_config)
                .ok()
                .map_or(ReasoningEffort::Max, |config| {
                    config.profile().default_reasoning_effort
                }),
        ),
        OPENAI_RESPONSES_PROVIDER_KEY => Some(ReasoningEffort::High),
        _ => None,
    }?;
    supported.contains(&configured).then_some(configured)
}

#[must_use]
pub(crate) fn reasoning_effort_binding(
    provider_name: &str,
    model_name: &str,
    supported: &[ReasoningEffort],
) -> Option<String> {
    if supported.is_empty() {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(REASONING_EFFORT_BINDING_DOMAIN);
    digest.update([0]);
    digest.update(normalize_provider_name(provider_name).as_bytes());
    digest.update([0]);
    digest.update(model_name.as_bytes());
    for effort in supported {
        digest.update([0]);
        digest.update(effort.as_str().as_bytes());
    }
    let mut encoded = String::with_capacity(64);
    for byte in digest.finalize() {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    Some(encoded)
}
