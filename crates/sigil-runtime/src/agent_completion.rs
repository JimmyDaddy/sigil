use std::{collections::BTreeSet, fmt::Debug, future::Future, pin::Pin};

use anyhow::Result;
use futures::{StreamExt, stream::FuturesUnordered};
use thiserror::Error;

type BoxedAgentCompletionFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
type BoxedAgentTerminalFuture<'a, K, C, T> =
    Pin<Box<dyn Future<Output = AgentTerminalEnvelope<K, C, T>> + Send + 'a>>;

/// One preflighted participant registered with the runtime completion hub.
pub(crate) struct AgentCompletionRegistration<'a, K, C, T> {
    key: K,
    sequence: u64,
    context: C,
    future: BoxedAgentCompletionFuture<'a, T>,
}

impl<'a, K, C, T> AgentCompletionRegistration<'a, K, C, T> {
    pub(crate) fn new<F>(key: K, sequence: u64, context: C, future: F) -> Self
    where
        F: Future<Output = Result<T>> + Send + 'a,
    {
        Self {
            key,
            sequence,
            context,
            future: Box::pin(future),
        }
    }

    pub(crate) fn into_parts(self) -> (K, u64, C, BoxedAgentCompletionFuture<'a, T>) {
        (self.key, self.sequence, self.context, self.future)
    }
}

/// Exactly one terminal delivery produced for one registered participant attempt.
///
/// `completion_index` reflects arrival order. Consumers may independently sort by `sequence`
/// before constructing deterministic model context or parent control entries.
pub(crate) struct AgentTerminalEnvelope<K, C, T> {
    pub(crate) key: K,
    pub(crate) sequence: u64,
    pub(crate) completion_index: u64,
    pub(crate) context: C,
    pub(crate) result: Result<T>,
}

/// Rejects a completion batch before any participant future is polled.
#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum AgentCompletionHubError {
    #[error("agent completion batch contains duplicate registration {key}")]
    DuplicateRegistration { key: String },
}

/// Owns every unpolled registration when whole-batch completion preflight fails.
pub(crate) struct AgentCompletionBatchRejection<'a, K, C, T> {
    error: AgentCompletionHubError,
    registrations: Vec<AgentCompletionRegistration<'a, K, C, T>>,
}

impl<'a, K, C, T> AgentCompletionBatchRejection<'a, K, C, T> {
    #[cfg(test)]
    pub(crate) fn error(&self) -> &AgentCompletionHubError {
        &self.error
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        AgentCompletionHubError,
        Vec<AgentCompletionRegistration<'a, K, C, T>>,
    ) {
        (self.error, self.registrations)
    }

    #[cfg(test)]
    pub(crate) fn into_error(self) -> AgentCompletionHubError {
        self.error
    }
}

/// Runtime-owned completion collector for one preflighted participant batch.
///
/// Construction validates the complete batch before moving any future into the active stream.
/// `collect` consumes the hub, so every accepted registration can yield at most one terminal
/// envelope and the same batch cannot be delivered twice.
pub(crate) struct AgentCompletionHub<'a, K, C, T> {
    pending: FuturesUnordered<BoxedAgentTerminalFuture<'a, K, C, T>>,
}

impl<'a, K, C, T> AgentCompletionHub<'a, K, C, T>
where
    K: Clone + Debug + Ord + Send + 'a,
    C: Send + 'a,
    T: Send + 'a,
{
    pub(crate) fn from_batch(
        registrations: Vec<AgentCompletionRegistration<'a, K, C, T>>,
    ) -> std::result::Result<Self, AgentCompletionBatchRejection<'a, K, C, T>> {
        let mut registered = BTreeSet::new();
        for registration in &registrations {
            if !registered.insert(registration.key.clone()) {
                return Err(AgentCompletionBatchRejection {
                    error: AgentCompletionHubError::DuplicateRegistration {
                        key: format!("{:?}", registration.key),
                    },
                    registrations,
                });
            }
        }

        let pending = FuturesUnordered::new();
        for registration in registrations {
            pending.push(Box::pin(async move {
                AgentTerminalEnvelope {
                    key: registration.key,
                    sequence: registration.sequence,
                    completion_index: 0,
                    context: registration.context,
                    result: registration.future.await,
                }
            }) as BoxedAgentTerminalFuture<'a, K, C, T>);
        }
        Ok(Self { pending })
    }

    pub(crate) async fn collect(self) -> Vec<AgentTerminalEnvelope<K, C, T>> {
        self.collect_with(|_| {}).await
    }

    pub(crate) async fn collect_with<F>(
        mut self,
        mut on_terminal: F,
    ) -> Vec<AgentTerminalEnvelope<K, C, T>>
    where
        F: FnMut(&AgentTerminalEnvelope<K, C, T>),
    {
        let mut completion_index = 0_u64;
        let mut completed = Vec::with_capacity(self.pending.len());
        while let Some(mut envelope) = self.pending.next().await {
            envelope.completion_index = completion_index;
            completion_index = completion_index.saturating_add(1);
            on_terminal(&envelope);
            completed.push(envelope);
        }
        completed
    }
}

#[cfg(test)]
#[path = "tests/agent_completion_tests.rs"]
mod tests;
