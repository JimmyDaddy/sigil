use std::{collections::BTreeSet, fmt};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use sha2::{Digest, Sha256};

use crate::{
    BackgroundTaskHandle, BackgroundTaskStatus, CitationSupport, ExternalProvenanceEntry,
    ExternalSourceRecord, ProviderChunk, ProviderContinuationState, ResponseHandle, SecretString,
    UsageStats,
};

const HOSTED_ID_MAX_BYTES: usize = 512;
const HOSTED_DOMAIN_FILTER_MAX_ITEMS: usize = 100;
const HOSTED_DOMAIN_FILTER_MAX_BYTES: usize = 2_048;
const HOSTED_DOMAIN_FILTER_TOTAL_BYTES: usize = 32 * 1_024;

/// Provider-neutral hosted capability requested for one completion.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostedToolKind {
    WebSearch,
}

/// Provider-neutral limits attached to one hosted-tool request.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HostedToolLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_domains: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_domains: Vec<String>,
}

impl HostedToolLimits {
    /// Validates cross-provider invariants before wire materialization.
    ///
    /// # Errors
    ///
    /// Returns [`HostedToolRequestError`] when a limit is empty, zero, or ambiguous.
    pub fn validate(&self) -> Result<(), HostedToolRequestError> {
        if self.max_uses == Some(0) {
            return Err(HostedToolRequestError::ZeroMaxUses);
        }
        if !self.allowed_domains.is_empty() && !self.blocked_domains.is_empty() {
            return Err(HostedToolRequestError::ConflictingDomainFilters);
        }
        if self
            .allowed_domains
            .iter()
            .chain(&self.blocked_domains)
            .any(|domain| domain.trim().is_empty())
        {
            return Err(HostedToolRequestError::EmptyDomainFilter);
        }
        let domains = self
            .allowed_domains
            .iter()
            .chain(&self.blocked_domains)
            .collect::<Vec<_>>();
        if domains.len() > HOSTED_DOMAIN_FILTER_MAX_ITEMS {
            return Err(HostedToolRequestError::DomainFilterLimitExceeded);
        }
        let total_bytes = domains
            .iter()
            .fold(0usize, |total, domain| total.saturating_add(domain.len()));
        if total_bytes > HOSTED_DOMAIN_FILTER_TOTAL_BYTES {
            return Err(HostedToolRequestError::DomainFilterBytesExceeded);
        }
        let mut unique = BTreeSet::new();
        for domain in domains {
            validate_domain_filter(domain)?;
            if !unique.insert(domain) {
                return Err(HostedToolRequestError::DuplicateDomainFilter);
            }
        }
        Ok(())
    }
}

/// One authorized provider-hosted capability carried into request materialization.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HostedToolRequest {
    pub authorization_id: String,
    pub request_fingerprint: String,
    pub kind: HostedToolKind,
    #[serde(default)]
    pub limits: HostedToolLimits,
}

impl HostedToolRequest {
    /// Constructs a request and binds its fingerprint to authorization, kind, and limits.
    ///
    /// # Errors
    ///
    /// Returns [`HostedToolRequestError`] when the identity or limits are invalid.
    pub fn new(
        authorization_id: impl Into<String>,
        kind: HostedToolKind,
        limits: HostedToolLimits,
    ) -> Result<Self, HostedToolRequestError> {
        let authorization_id = authorization_id.into();
        validate_hosted_identity(&authorization_id)?;
        limits.validate()?;
        let request_fingerprint = hosted_request_fingerprint(&authorization_id, kind, &limits);
        Ok(Self {
            authorization_id,
            request_fingerprint,
            kind,
            limits,
        })
    }

    /// Validates correlation material and limits before a provider request is sent.
    ///
    /// # Errors
    ///
    /// Returns [`HostedToolRequestError`] when required fields or limits are invalid.
    pub fn validate(&self) -> Result<(), HostedToolRequestError> {
        if self.authorization_id.trim().is_empty() {
            return Err(HostedToolRequestError::MissingAuthorizationId);
        }
        if self.request_fingerprint.trim().is_empty() {
            return Err(HostedToolRequestError::MissingRequestFingerprint);
        }
        validate_hosted_identity(&self.authorization_id)?;
        validate_hosted_identity(&self.request_fingerprint)?;
        self.limits.validate()?;
        let expected = hosted_request_fingerprint(&self.authorization_id, self.kind, &self.limits);
        if self.request_fingerprint != expected {
            return Err(HostedToolRequestError::RequestFingerprintMismatch);
        }
        Ok(())
    }
}

/// Typed provider-neutral hosted request validation failure.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum HostedToolRequestError {
    #[error("hosted tool request is missing authorization id")]
    MissingAuthorizationId,
    #[error("hosted tool request is missing request fingerprint")]
    MissingRequestFingerprint,
    #[error("hosted tool max uses must be greater than zero")]
    ZeroMaxUses,
    #[error("hosted tool allowed and blocked domain filters are mutually exclusive")]
    ConflictingDomainFilters,
    #[error("hosted tool domain filter must not be empty")]
    EmptyDomainFilter,
    #[error("hosted tool domain filter count exceeds the hard limit")]
    DomainFilterLimitExceeded,
    #[error("hosted tool domain filters exceed the aggregate byte limit")]
    DomainFilterBytesExceeded,
    #[error("hosted tool domain filter is not a canonical bounded domain pattern")]
    InvalidDomainFilter,
    #[error("hosted tool domain filter is duplicated")]
    DuplicateDomainFilter,
    #[error("hosted tool identity is not bounded safe ASCII")]
    InvalidIdentity,
    #[error("hosted tool request fingerprint does not match its canonical content")]
    RequestFingerprintMismatch,
}

fn validate_hosted_identity(value: &str) -> Result<(), HostedToolRequestError> {
    if value.is_empty()
        || value.len() > HOSTED_ID_MAX_BYTES
        || !value.is_ascii()
        || value
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte)))
    {
        return Err(HostedToolRequestError::InvalidIdentity);
    }
    Ok(())
}

fn validate_domain_filter(value: &str) -> Result<(), HostedToolRequestError> {
    if value.len() > HOSTED_DOMAIN_FILTER_MAX_BYTES
        || !value.is_ascii()
        || value.bytes().any(|byte| byte.is_ascii_control())
        || value.contains(['@', '?', '#', '\\'])
        || value.contains("://")
    {
        return Err(HostedToolRequestError::InvalidDomainFilter);
    }
    let (host, path) = value.split_once('/').unwrap_or((value, ""));
    if host.is_empty()
        || host.contains('*')
        || !host.contains('.')
        || host != host.to_ascii_lowercase()
        || !matches!(url::Host::parse(host), Ok(url::Host::Domain(domain)) if domain == host)
        || path.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return Err(HostedToolRequestError::InvalidDomainFilter);
    }
    Ok(())
}

fn hosted_request_fingerprint(
    authorization_id: &str,
    kind: HostedToolKind,
    limits: &HostedToolLimits,
) -> String {
    let mut allowed_domains = limits.allowed_domains.clone();
    let mut blocked_domains = limits.blocked_domains.clone();
    allowed_domains.sort();
    blocked_domains.sort();
    let canonical = serde_json::json!({
        "authorization_id": authorization_id,
        "kind": hosted_kind_label(kind),
        "limits": {
            "max_uses": limits.max_uses,
            "allowed_domains": allowed_domains,
            "blocked_domains": blocked_domains,
        }
    });
    let bytes = serde_json::to_vec(&canonical)
        .expect("hosted request fingerprint material contains only serializable values");
    let digest = Sha256::digest(bytes);
    format!("hosted-v1:{digest:x}")
}

/// Whether a model exposes a hosted tool at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HostedToolSupport {
    #[default]
    Unsupported,
    ServerManaged,
}

/// When an exact provider-generated query can become visible to Sigil.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HostedQueryVisibility {
    #[default]
    Unavailable,
    ProviderReportedPostExecution,
}

/// Fidelity of source metadata returned by a hosted provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HostedSourceFidelity {
    #[default]
    Unavailable,
    UrlAndTitle,
}

/// Fidelity of claim-level citations returned by a hosted provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HostedCitationFidelity {
    #[default]
    Unavailable,
    OutputSpan,
}

/// Strength with which a provider enforces a hosted-tool request constraint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HostedConstraintEnforcement {
    #[default]
    Unsupported,
    BestEffort,
    Hard,
}

/// Model-specific declaration for native provider web search.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostedWebSearchCapability {
    pub support: HostedToolSupport,
    pub query_visibility: HostedQueryVisibility,
    pub source_fidelity: HostedSourceFidelity,
    pub citation_fidelity: HostedCitationFidelity,
    pub max_uses_enforcement: HostedConstraintEnforcement,
    pub domain_filter_enforcement: HostedConstraintEnforcement,
}

impl HostedWebSearchCapability {
    #[must_use]
    pub fn is_supported(self) -> bool {
        self.support != HostedToolSupport::Unsupported
    }
}

/// Exact provider source candidate. Secret-bearing fields deliberately have no serde contract.
#[derive(Clone)]
pub struct HostedSourceCandidate {
    provider_source_id: SecretString,
    raw_url: SecretString,
    raw_title: Option<SecretString>,
    published_at: Option<String>,
    rank: Option<usize>,
}

impl HostedSourceCandidate {
    #[must_use]
    pub fn new(
        provider_source_id: impl Into<String>,
        raw_url: impl Into<String>,
        raw_title: Option<String>,
    ) -> Self {
        Self {
            provider_source_id: SecretString::new(provider_source_id),
            raw_url: SecretString::new(raw_url),
            raw_title: raw_title.map(SecretString::new),
            published_at: None,
            rank: None,
        }
    }

    #[must_use]
    pub fn with_published_at(mut self, published_at: impl Into<String>) -> Self {
        self.published_at = Some(published_at.into());
        self
    }

    #[must_use]
    pub fn with_rank(mut self, rank: usize) -> Self {
        self.rank = Some(rank);
        self
    }

    #[must_use]
    pub fn provider_source_id(&self) -> &str {
        self.provider_source_id.expose_secret()
    }

    #[must_use]
    pub fn raw_url(&self) -> &str {
        self.raw_url.expose_secret()
    }

    #[must_use]
    pub fn raw_title(&self) -> Option<&str> {
        self.raw_title.as_ref().map(SecretString::expose_secret)
    }

    #[must_use]
    pub fn published_at(&self) -> Option<&str> {
        self.published_at.as_deref()
    }

    #[must_use]
    pub fn rank(&self) -> Option<usize> {
        self.rank
    }
}

impl fmt::Debug for HostedSourceCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostedSourceCandidate")
            .field("provider_source_id", &"[redacted]")
            .field("raw_url", &"[redacted]")
            .field("raw_title", &self.raw_title.as_ref().map(|_| "[redacted]"))
            .field("published_at", &self.published_at)
            .field("rank", &self.rank)
            .finish()
    }
}

/// Exact provider citation offsets into the pre-finalized assistant text.
#[derive(Clone)]
pub struct HostedCitationCandidate {
    provider_source_id: SecretString,
    start_byte: usize,
    end_byte: usize,
}

impl HostedCitationCandidate {
    #[must_use]
    pub fn new(provider_source_id: impl Into<String>, start_byte: usize, end_byte: usize) -> Self {
        Self {
            provider_source_id: SecretString::new(provider_source_id),
            start_byte,
            end_byte,
        }
    }

    #[must_use]
    pub fn provider_source_id(&self) -> &str {
        self.provider_source_id.expose_secret()
    }

    #[must_use]
    pub fn start_byte(&self) -> usize {
        self.start_byte
    }

    #[must_use]
    pub fn end_byte(&self) -> usize {
        self.end_byte
    }
}

impl fmt::Debug for HostedCitationCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostedCitationCandidate")
            .field("provider_source_id", &"[redacted]")
            .field("start_byte", &self.start_byte)
            .field("end_byte", &self.end_byte)
            .finish()
    }
}

/// Transient hosted evidence sidecar. None of its variants are serializable.
#[derive(Clone)]
pub enum HostedEvidence {
    Source(HostedSourceCandidate),
    Citation(HostedCitationCandidate),
    QueryObserved(SecretString),
}

impl fmt::Debug for HostedEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(source) => formatter.debug_tuple("Source").field(source).finish(),
            Self::Citation(citation) => formatter.debug_tuple("Citation").field(citation).finish(),
            Self::QueryObserved(_) => formatter.write_str("QueryObserved([redacted])"),
        }
    }
}

/// Hard limits for the pre-finalization hosted turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostedTurnBufferLimits {
    pub total_bytes: usize,
    pub text_bytes: usize,
    pub reasoning_bytes: usize,
    pub evidence_bytes: usize,
    pub evidence_items: usize,
}

impl Default for HostedTurnBufferLimits {
    fn default() -> Self {
        Self {
            total_bytes: 1024 * 1024,
            text_bytes: 512 * 1024,
            reasoning_bytes: 512 * 1024,
            evidence_bytes: 512 * 1024,
            evidence_items: 256,
        }
    }
}

/// Typed fail-closed hosted buffering/finalization failure.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum HostedTurnError {
    #[error("hosted turn buffer limit exceeded")]
    BufferLimitExceeded,
    #[error("hosted turn received an unsupported local tool-call chunk")]
    UnsupportedLocalToolChunk,
    #[error("hosted evidence processor is required")]
    MissingProcessor,
    #[error("hosted evidence finalization failed")]
    FinalizationFailed,
    #[error("hosted provider reported a terminal search failure")]
    ProviderFailed,
    #[error("hosted provider emitted invalid invocation correlation")]
    InvalidInvocationCorrelation,
}

/// Non-serializable, hard-capped turn material retained until a finalizer succeeds.
#[derive(Clone)]
pub struct HostedTurnBuffer {
    limits: HostedTurnBufferLimits,
    text: String,
    reasoning: String,
    evidence: Vec<HostedEvidence>,
    usages: Vec<UsageStats>,
    background_accepted: Vec<BackgroundTaskHandle>,
    background_statuses: Vec<BackgroundTaskStatus>,
    response_handles: Vec<ResponseHandle>,
    continuation_states: Vec<ProviderContinuationState>,
    observed_uses: Option<u32>,
    total_bytes: usize,
    evidence_bytes: usize,
    started_invocations: BTreeSet<(String, String, &'static str)>,
    failed: bool,
}

impl fmt::Debug for HostedTurnBuffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostedTurnBuffer")
            .field("text_bytes", &self.text.len())
            .field("reasoning_bytes", &self.reasoning.len())
            .field("evidence_items", &self.evidence.len())
            .field("total_bytes", &self.total_bytes)
            .field("failed", &self.failed)
            .finish()
    }
}

impl HostedTurnBuffer {
    #[must_use]
    pub fn new(limits: HostedTurnBufferLimits) -> Self {
        Self {
            limits,
            text: String::new(),
            reasoning: String::new(),
            evidence: Vec::new(),
            usages: Vec::new(),
            background_accepted: Vec::new(),
            background_statuses: Vec::new(),
            response_handles: Vec::new(),
            continuation_states: Vec::new(),
            observed_uses: None,
            total_bytes: 0,
            evidence_bytes: 0,
            started_invocations: BTreeSet::new(),
            failed: false,
        }
    }

    pub fn push(&mut self, chunk: ProviderChunk) -> Result<(), HostedTurnError> {
        match chunk {
            ProviderChunk::TextDelta(delta) => self.push_text(delta),
            ProviderChunk::ReasoningDelta(delta) | ProviderChunk::ReasoningSummaryDelta(delta) => {
                self.push_reasoning(delta)
            }
            ProviderChunk::HostedToolStarted {
                authorization_id,
                invocation_id,
                kind,
            } => self.push_started(authorization_id, invocation_id, kind),
            ProviderChunk::HostedEvidence {
                authorization_id,
                invocation_id,
                kind,
                evidence,
            } => {
                self.require_started(&authorization_id, &invocation_id, kind)?;
                self.push_evidence(evidence)
            }
            ProviderChunk::HostedToolFailed {
                authorization_id,
                invocation_id,
                kind,
                ..
            } => {
                self.require_started(&authorization_id, &invocation_id, kind)?;
                self.failed = true;
                Ok(())
            }
            ProviderChunk::HostedRequestUsage {
                authorization_id,
                kind,
                observed_uses,
            } => {
                if !self.started_invocations.iter().any(
                    |(started_authorization, _, started_kind)| {
                        started_authorization == &authorization_id
                            && *started_kind == hosted_kind_label(kind)
                    },
                ) {
                    return Err(HostedTurnError::InvalidInvocationCorrelation);
                }
                self.observed_uses =
                    Some(self.observed_uses.unwrap_or_default().max(observed_uses));
                Ok(())
            }
            ProviderChunk::Usage(usage) => {
                self.usages.push(usage);
                Ok(())
            }
            ProviderChunk::BackgroundTaskAccepted(handle) => {
                self.background_accepted.push(handle);
                Ok(())
            }
            ProviderChunk::BackgroundTaskStatus(status) => {
                self.background_statuses.push(status);
                Ok(())
            }
            ProviderChunk::ResponseHandle(handle) => {
                self.response_handles.push(handle);
                Ok(())
            }
            ProviderChunk::ContinuationState(state) => {
                self.continuation_states.push(state);
                Ok(())
            }
            ProviderChunk::ReasoningArtifact(_) | ProviderChunk::Done => Ok(()),
            ProviderChunk::ToolCallStart { .. }
            | ProviderChunk::ToolCallArgsDelta { .. }
            | ProviderChunk::ToolCallComplete(_)
            | ProviderChunk::ToolCallStreamError(_) => {
                Err(HostedTurnError::UnsupportedLocalToolChunk)
            }
        }
    }

    fn push_text(&mut self, delta: String) -> Result<(), HostedTurnError> {
        self.charge(delta.len())?;
        if self.text.len().saturating_add(delta.len()) > self.limits.text_bytes {
            return Err(HostedTurnError::BufferLimitExceeded);
        }
        self.text.push_str(&delta);
        Ok(())
    }

    fn push_reasoning(&mut self, delta: String) -> Result<(), HostedTurnError> {
        self.charge(delta.len())?;
        if self.reasoning.len().saturating_add(delta.len()) > self.limits.reasoning_bytes {
            return Err(HostedTurnError::BufferLimitExceeded);
        }
        self.reasoning.push_str(&delta);
        Ok(())
    }

    fn push_evidence(&mut self, evidence: HostedEvidence) -> Result<(), HostedTurnError> {
        let bytes = evidence_exact_bytes(&evidence);
        self.charge(bytes)?;
        if self.evidence.len() >= self.limits.evidence_items
            || self.evidence_bytes.saturating_add(bytes) > self.limits.evidence_bytes
        {
            return Err(HostedTurnError::BufferLimitExceeded);
        }
        self.evidence_bytes = self.evidence_bytes.saturating_add(bytes);
        self.evidence.push(evidence);
        Ok(())
    }

    fn push_started(
        &mut self,
        authorization_id: String,
        invocation_id: String,
        kind: HostedToolKind,
    ) -> Result<(), HostedTurnError> {
        if validate_hosted_identity(&authorization_id).is_err()
            || validate_hosted_identity(&invocation_id).is_err()
        {
            return Err(HostedTurnError::InvalidInvocationCorrelation);
        }
        let key = (authorization_id, invocation_id, hosted_kind_label(kind));
        if !self.started_invocations.insert(key) {
            return Err(HostedTurnError::InvalidInvocationCorrelation);
        }
        Ok(())
    }

    fn require_started(
        &self,
        authorization_id: &str,
        invocation_id: &str,
        kind: HostedToolKind,
    ) -> Result<(), HostedTurnError> {
        let key = (
            authorization_id.to_owned(),
            invocation_id.to_owned(),
            hosted_kind_label(kind),
        );
        if !self.started_invocations.contains(&key) {
            return Err(HostedTurnError::InvalidInvocationCorrelation);
        }
        Ok(())
    }

    fn charge(&mut self, bytes: usize) -> Result<(), HostedTurnError> {
        let next = self.total_bytes.saturating_add(bytes);
        if next > self.limits.total_bytes {
            return Err(HostedTurnError::BufferLimitExceeded);
        }
        self.total_bytes = next;
        Ok(())
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn reasoning(&self) -> &str {
        &self.reasoning
    }

    #[must_use]
    pub fn evidence(&self) -> &[HostedEvidence] {
        &self.evidence
    }

    #[must_use]
    pub fn hosted_used(&self) -> bool {
        !self.started_invocations.is_empty() || !self.evidence.is_empty()
    }

    pub(crate) fn provider_failed(&self) -> bool {
        self.failed
    }

    pub(crate) fn usages(&self) -> &[UsageStats] {
        &self.usages
    }

    pub(crate) fn background_accepted(&self) -> &[BackgroundTaskHandle] {
        &self.background_accepted
    }

    pub(crate) fn background_statuses(&self) -> &[BackgroundTaskStatus] {
        &self.background_statuses
    }

    pub(crate) fn response_handles(&self) -> &[ResponseHandle] {
        &self.response_handles
    }

    pub(crate) fn continuation_states(&self) -> &[ProviderContinuationState] {
        &self.continuation_states
    }

    #[must_use]
    pub fn observed_uses(&self) -> Option<u32> {
        self.observed_uses
    }
}

fn hosted_kind_label(kind: HostedToolKind) -> &'static str {
    match kind {
        HostedToolKind::WebSearch => "web_search",
    }
}

fn evidence_exact_bytes(evidence: &HostedEvidence) -> usize {
    match evidence {
        HostedEvidence::Source(source) => source
            .provider_source_id()
            .len()
            .saturating_add(source.raw_url().len())
            .saturating_add(source.raw_title().map_or(0, str::len)),
        HostedEvidence::Citation(citation) => citation.provider_source_id().len(),
        HostedEvidence::QueryObserved(query) => query.expose_secret().len(),
    }
}

/// Safe finalization context owned by kernel; exact evidence remains in the buffer.
#[derive(Debug, Clone)]
pub struct HostedFinalizationContext {
    pub session_scope_id: String,
    pub provider_name: String,
    pub model_name: String,
}

/// Safe citation after provider source-id rewrite and text offset projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizedHostedCitation {
    pub source_id: String,
    pub start_byte: usize,
    pub end_byte: usize,
}

/// Finalizer output. Every string in this type must already be safe to persist and emit.
#[derive(Debug, Clone)]
pub struct FinalizedHostedTurn {
    pub assistant_text: String,
    pub reasoning_trace: String,
    pub sources: Vec<ExternalSourceRecord>,
    pub citations: Vec<FinalizedHostedCitation>,
    pub hosted_used: bool,
    pub query_observed: bool,
}

impl FinalizedHostedTurn {
    pub fn to_provenance(
        &self,
        session_scope_id: impl Into<String>,
        message_id: impl Into<String>,
        final_safe_text: &str,
    ) -> ExternalProvenanceEntry {
        let session_scope_id = session_scope_id.into();
        let message_id = message_id.into();
        let citations = if final_safe_text == self.assistant_text {
            self.citations
                .iter()
                .filter_map(|citation| {
                    CitationSupport::for_final_safe_text(
                        session_scope_id.clone(),
                        message_id.clone(),
                        citation.source_id.clone(),
                        final_safe_text,
                        citation.start_byte,
                        citation.end_byte,
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        ExternalProvenanceEntry {
            session_scope_id,
            message_id,
            trust: crate::ExternalTrust::ExternalUntrusted,
            sources: self.sources.clone(),
            citations,
        }
    }
}

/// Runtime injection point that must normalize hosted evidence before kernel visibility.
#[async_trait]
pub trait HostedEvidenceProcessor: Send + Sync {
    async fn finalize(
        &self,
        context: HostedFinalizationContext,
        buffer: &HostedTurnBuffer,
    ) -> Result<FinalizedHostedTurn, HostedTurnError>;
}

/// Explicit send boundary used by hosted provider adapters to prohibit post-send retries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HostedRequestWireState {
    #[default]
    Prepared,
    RequestBytesStarted,
    Terminal,
}

impl HostedRequestWireState {
    #[must_use]
    pub fn retry_allowed(self) -> bool {
        self == Self::Prepared
    }

    pub fn mark_request_bytes_started(&mut self) -> Result<(), HostedWireStateError> {
        if *self != Self::Prepared {
            return Err(HostedWireStateError::InvalidTransition);
        }
        *self = Self::RequestBytesStarted;
        Ok(())
    }

    pub fn finish(&mut self) -> Result<(), HostedWireStateError> {
        if *self == Self::Terminal {
            return Err(HostedWireStateError::InvalidTransition);
        }
        *self = Self::Terminal;
        Ok(())
    }
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum HostedWireStateError {
    #[error("invalid hosted request wire-state transition")]
    InvalidTransition,
}

#[cfg(test)]
#[path = "tests/hosted_tool_tests.rs"]
mod tests;
