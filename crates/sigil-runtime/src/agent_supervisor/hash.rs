use anyhow::Result;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sigil_kernel::{AgentRunInput, ProviderCapabilities};

pub(super) fn hash_child_input(input: &AgentRunInput) -> Result<String> {
    sigil_kernel::task_participant_input_hash(input)
}

pub(super) fn hash_provider_capabilities(capabilities: &ProviderCapabilities) -> Result<String> {
    hash_json(&serde_json::to_value(capabilities)?)
}

pub(super) fn hash_json(value: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

pub(super) fn hash_text(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub(super) fn short_digest(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}
