use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use sigil_kernel::ProviderContinuationState;

pub(crate) const DEEPSEEK_HOSTED_CONTINUATION_KIND: &str =
    "deepseek.hosted_web_search.interrupt_on_restart";

const MAX_CONTINUATIONS: usize = 64;
const MAX_CONTINUATION_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_CONTINUATION_BYTES: usize = 8 * 1024 * 1024;

/// Process-local store for exact content blocks that must be replayed when a
/// hosted web-search turn pauses (for example a `pause_turn` stop reason).
///
/// The store is bounded and per-process; a restart loses live continuations,
/// which is why the persisted state carries `interrupt_on_restart`.
#[derive(Clone, Default)]
pub(crate) struct DeepSeekHostedContinuationStore {
    inner: Arc<Mutex<ContinuationStoreInner>>,
    next_handle: Arc<AtomicU64>,
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

impl DeepSeekHostedContinuationStore {
    pub(crate) fn retain_blocks(
        &self,
        blocks: Vec<Value>,
        continuation_reason: &'static str,
    ) -> Result<ProviderContinuationState> {
        let bytes = serde_json::to_vec(&blocks)?.len();
        if bytes > MAX_CONTINUATION_BYTES {
            return Err(anyhow!(
                "DeepSeek hosted continuation exceeds the process-local byte limit"
            ));
        }
        let handle = format!(
            "deepseek-hosted-{}",
            self.next_handle.fetch_add(1, Ordering::Relaxed)
        );
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| anyhow!("DeepSeek hosted continuation store lock is poisoned"))?;
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
            provider_name: "deepseek".to_owned(),
            state_kind: DEEPSEEK_HOSTED_CONTINUATION_KIND.to_owned(),
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
            state.provider_name == "deepseek"
                && state.state_kind == DEEPSEEK_HOSTED_CONTINUATION_KIND
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
            .map_err(|_| anyhow!("DeepSeek hosted continuation store lock is poisoned"))?;
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

/// Per-request hosted stream context: the authorization that the mapper must
/// correlate evidence against, the continuation store, and server-tool
/// invocations already present in the request (from a replayed prior turn).
#[derive(Clone)]
pub(crate) struct DeepSeekHostedStreamContext {
    pub(crate) authorization_id: String,
    pub(crate) continuation_store: DeepSeekHostedContinuationStore,
    pub(crate) prior_invocations: BTreeMap<String, String>,
}
