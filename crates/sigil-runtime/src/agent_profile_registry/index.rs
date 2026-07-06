use std::collections::BTreeSet;

use anyhow::Result;
use serde_json::json;
use sigil_kernel::{
    AgentProfileId, AgentProfileKind, AgentResultPolicy, AgentTrustState, ToolRegistryScope,
};

use super::{ResolvedAgentProfile, hash_json};

/// Deterministic model-visible projection of trusted invocable agent profiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelVisibleAgentIndex {
    pub entries: Vec<ModelVisibleAgentIndexEntry>,
    pub hidden_count: usize,
    pub fingerprint: String,
}

/// One bounded profile row visible to model-facing spawn-agent descriptions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelVisibleAgentIndexEntry {
    pub profile_id: AgentProfileId,
    pub kind: AgentProfileKind,
    pub description: String,
    pub result_policy: AgentResultPolicy,
    pub tool_scope_hash: String,
}

/// Current run constraints used to filter the model-visible agent index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProfileIndexContext {
    pub tool_scope: ToolRegistryScope,
    pub allowed_profile_ids: Option<BTreeSet<AgentProfileId>>,
    pub max_entries: Option<usize>,
}

impl Default for AgentProfileIndexContext {
    fn default() -> Self {
        Self {
            tool_scope: ToolRegistryScope {
                allow_all: true,
                ..ToolRegistryScope::default()
            },
            allowed_profile_ids: None,
            max_entries: None,
        }
    }
}

pub(super) fn build_model_visible_index(
    profiles: &[ResolvedAgentProfile],
    context: &AgentProfileIndexContext,
) -> Result<ModelVisibleAgentIndex> {
    let mut candidates = profiles
        .iter()
        .filter(|profile| profile.effective_enabled())
        .filter(|profile| profile.trust_state == AgentTrustState::Trusted)
        .filter(|profile| profile.effective_model_invocation_allowed())
        .filter(|profile| profile_allowed_by_context(profile, context))
        .map(|profile| {
            Ok(ModelVisibleAgentIndexEntry {
                profile_id: profile.profile.id.clone(),
                kind: profile.profile.kind,
                description: profile.profile.description.clone(),
                result_policy: profile.profile.result_policy,
                tool_scope_hash: hash_json(&serde_json::to_value(&profile.profile.tool_scope)?)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    candidates.sort_by(|left, right| left.profile_id.cmp(&right.profile_id));
    let total = candidates.len();
    if let Some(max_entries) = context.max_entries {
        candidates.truncate(max_entries);
    }
    let hidden_count = total.saturating_sub(candidates.len());
    let fingerprint = model_visible_fingerprint(&candidates, hidden_count, context)?;
    Ok(ModelVisibleAgentIndex {
        entries: candidates,
        hidden_count,
        fingerprint,
    })
}

fn profile_allowed_by_context(
    profile: &ResolvedAgentProfile,
    context: &AgentProfileIndexContext,
) -> bool {
    if let Some(allowed_profile_ids) = &context.allowed_profile_ids
        && !allowed_profile_ids.contains(&profile.profile.id)
    {
        return false;
    }
    tool_scope_contains(&context.tool_scope, &profile.profile.tool_scope)
}

fn tool_scope_contains(parent: &ToolRegistryScope, child: &ToolRegistryScope) -> bool {
    if parent.allow_all {
        return true;
    }
    if child.allow_all {
        return false;
    }
    child.names.iter().all(|name| parent.allows(name))
        && child.prefixes.iter().all(|prefix| parent.allows(prefix))
}

fn model_visible_fingerprint(
    entries: &[ModelVisibleAgentIndexEntry],
    hidden_count: usize,
    context: &AgentProfileIndexContext,
) -> Result<String> {
    let entries_json = entries
        .iter()
        .map(|entry| {
            json!({
                "profile_id": entry.profile_id.as_str(),
                "kind": entry.kind,
                "description": entry.description,
                "result_policy": entry.result_policy,
                "tool_scope_hash": entry.tool_scope_hash,
            })
        })
        .collect::<Vec<_>>();
    hash_json(&json!({
        "entries": entries_json,
        "hidden_count": hidden_count,
        "tool_scope": context.tool_scope,
        "allowed_profile_ids": context.allowed_profile_ids.as_ref().map(|ids| {
            ids.iter().map(|id| id.as_str().to_owned()).collect::<Vec<_>>()
        }),
    }))
}
