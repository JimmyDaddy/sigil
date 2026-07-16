use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    sync::{Arc, Mutex},
};

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use sigil_kernel::{
    HostedCitationFidelity, HostedConstraintEnforcement, HostedCustomToolCompatibility,
    HostedQueryVisibility, HostedSourceFidelity, HostedToolKind, HostedToolRequest,
    HostedToolSupport, HostedWebSearchCapability, ProviderContinuationState,
};

pub(crate) const ANTHROPIC_WEB_SEARCH_TOOL_TYPE: &str = "web_search_20250305";
pub(crate) const ANTHROPIC_HOSTED_CONTINUATION_KIND: &str =
    "anthropic.hosted_web_search.interrupt_on_restart";

const MAX_CONTINUATIONS: usize = 64;
const MAX_CONTINUATION_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_CONTINUATION_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnthropicHostedPlatform {
    ClaudeApi,
    UnsupportedCompatibleEndpoint,
}

impl AnthropicHostedPlatform {
    pub(crate) fn from_base_url(base_url: &str) -> Self {
        if base_url.trim_end_matches('/') == "https://api.anthropic.com" {
            Self::ClaudeApi
        } else {
            Self::UnsupportedCompatibleEndpoint
        }
    }

    pub(crate) fn supports_web_search(self) -> bool {
        self == Self::ClaudeApi
    }
}

pub(crate) fn hosted_web_search_capability(
    model_name: &str,
    platform: AnthropicHostedPlatform,
) -> HostedWebSearchCapability {
    if !platform.supports_web_search() || !is_hosted_web_search_model(model_name) {
        return HostedWebSearchCapability::default();
    }
    HostedWebSearchCapability {
        support: HostedToolSupport::ServerManaged,
        query_visibility: HostedQueryVisibility::ProviderReportedPostExecution,
        source_fidelity: HostedSourceFidelity::UrlAndTitle,
        citation_fidelity: HostedCitationFidelity::OutputSpan,
        max_uses_enforcement: HostedConstraintEnforcement::Hard,
        domain_filter_enforcement: HostedConstraintEnforcement::Hard,
        custom_tool_compatibility: HostedCustomToolCompatibility::Supported,
    }
}

pub(crate) fn is_hosted_web_search_model(model_name: &str) -> bool {
    matches!(
        model_name,
        "claude-opus-4-8"
            | "claude-opus-4-7"
            | "claude-opus-4-6"
            | "claude-sonnet-5"
            | "claude-sonnet-4-6"
            | "claude-sonnet-4-5"
            | "claude-sonnet-4-5-20250929"
            | "claude-opus-4-5"
            | "claude-opus-4-5-20251101"
            | "claude-haiku-4-5"
            | "claude-haiku-4-5-20251001"
    )
}

pub(crate) fn hosted_web_search_request(
    hosted_tools: &[HostedToolRequest],
) -> Result<Option<&HostedToolRequest>> {
    let mut matches = hosted_tools
        .iter()
        .filter(|request| request.kind == HostedToolKind::WebSearch);
    let request = matches.next();
    if matches.next().is_some() {
        return Err(anyhow!(
            "Anthropic request contains more than one hosted web-search declaration"
        ));
    }
    if let Some(request) = request {
        request.validate()?;
    }
    Ok(request)
}

#[derive(Clone, Default)]
pub(crate) struct AnthropicHostedContinuationStore {
    inner: Arc<Mutex<ContinuationStoreInner>>,
}

impl fmt::Debug for AnthropicHostedContinuationStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.inner.lock().map_err(|_| fmt::Error)?;
        formatter
            .debug_struct("AnthropicHostedContinuationStore")
            .field("entries", &inner.entries.len())
            .field("total_bytes", &inner.total_bytes)
            .finish()
    }
}

#[derive(Default)]
struct ContinuationStoreInner {
    entries: BTreeMap<String, StoredContinuation>,
    order: VecDeque<String>,
    total_bytes: usize,
}

struct StoredContinuation {
    blocks: Vec<Value>,
    bytes: usize,
}

impl AnthropicHostedContinuationStore {
    pub(crate) fn retain_blocks(
        &self,
        blocks: Vec<Value>,
        continuation_reason: &'static str,
    ) -> Result<ProviderContinuationState> {
        let bytes = serde_json::to_vec(&blocks)?.len();
        if bytes > MAX_CONTINUATION_BYTES {
            return Err(anyhow!(
                "Anthropic hosted continuation exceeds the process-local byte limit"
            ));
        }
        let handle = uuid::Uuid::new_v4().to_string();
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| anyhow!("Anthropic hosted continuation store lock is poisoned"))?;
        while inner.entries.len() >= MAX_CONTINUATIONS
            || inner.total_bytes.saturating_add(bytes) > MAX_TOTAL_CONTINUATION_BYTES
        {
            let Some(oldest) = inner.order.pop_front() else {
                break;
            };
            if let Some(removed) = inner.entries.remove(&oldest) {
                inner.total_bytes = inner.total_bytes.saturating_sub(removed.bytes);
            }
        }
        inner.total_bytes = inner.total_bytes.saturating_add(bytes);
        inner.order.push_back(handle.clone());
        inner
            .entries
            .insert(handle.clone(), StoredContinuation { blocks, bytes });
        Ok(ProviderContinuationState {
            provider_name: "anthropic".to_owned(),
            state_kind: ANTHROPIC_HOSTED_CONTINUATION_KIND.to_owned(),
            message_id: None,
            opaque_blob: json!({
                "handle": handle,
                "restart_policy": "interrupt_on_restart",
                "continuation_reason": continuation_reason,
            }),
        })
    }

    pub(crate) fn resolve_for_message(
        &self,
        states: &[ProviderContinuationState],
        message_id: &str,
    ) -> Result<ContinuationResolution> {
        let Some(state) = states.iter().rev().find(|state| {
            state.provider_name == "anthropic"
                && state.state_kind == ANTHROPIC_HOSTED_CONTINUATION_KIND
                && state.message_id.as_deref() == Some(message_id)
        }) else {
            return Ok(ContinuationResolution::Absent);
        };
        let Some(handle) = state.opaque_blob.get("handle").and_then(Value::as_str) else {
            return Ok(ContinuationResolution::InterruptedOnRestart);
        };
        let inner = self
            .inner
            .lock()
            .map_err(|_| anyhow!("Anthropic hosted continuation store lock is poisoned"))?;
        Ok(inner
            .entries
            .get(handle)
            .map_or(ContinuationResolution::InterruptedOnRestart, |stored| {
                ContinuationResolution::Live(stored.blocks.clone())
            }))
    }
}

pub(crate) enum ContinuationResolution {
    Absent,
    Live(Vec<Value>),
    InterruptedOnRestart,
}

#[derive(Clone)]
pub(crate) struct AnthropicHostedStreamContext {
    pub(crate) authorization_id: String,
    pub(crate) continuation_store: AnthropicHostedContinuationStore,
    pub(crate) prior_invocations: BTreeMap<String, String>,
}

#[cfg(test)]
#[path = "tests/hosted_search_tests.rs"]
mod tests;
