use anyhow::Result;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{AgentRunInput, ProviderCapabilities};

pub(super) fn hash_child_input(input: &AgentRunInput) -> Result<String> {
    hash_json(&json!({
        "persisted_user_message": input.persisted_user_message.as_deref(),
        "transient_context": &input.transient_context,
        "task_plan_update": input.task_plan_update.as_ref().map(|context| {
            json!({
                "task_id": context.task_id.as_str(),
                "max_plan_steps": context.max_plan_steps,
            })
        }),
    }))
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
