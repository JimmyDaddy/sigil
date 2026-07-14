use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

use anyhow::{Context, Result, bail};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

use super::store::session_entry_from_stored_event;
use super::*;
use crate::{
    BranchId, CompactionId, ContextSensitivity, EffectiveTokenBudget, EventId, RequestFitProof,
    SessionId, TokenMeasurementBinding, TokenMeasurementScope, VersionedProfileIdentity,
    WorkspaceSnapshotId, projection_apply_decision,
};

/// Schema version for provider-neutral native-continuation durable payloads.
pub const PROVIDER_CONTINUATION_SCHEMA_VERSION: u16 = 1;

/// Projection schema version for native-continuation observations, candidates, payloads, and
/// provider-observed resolution plans.
pub const PROVIDER_CONTINUATION_PROJECTION_SCHEMA_VERSION: u16 = 2;

/// Maximum retained durable refs recorded by one provider-observed resolution plan.
pub const MAX_PROVIDER_CONTINUATION_RESOLUTION_RETAINED_REFS: usize = 512;

/// Maximum protected durable refs recorded by one provider-observed resolution plan.
pub const MAX_PROVIDER_CONTINUATION_RESOLUTION_PROTECTED_REFS: usize = 512;

/// Maximum aggregate byte size of retained/protected refs in one resolution plan.
pub const MAX_PROVIDER_CONTINUATION_RESOLUTION_REFERENCE_BYTES: usize = 64 * 1024;

static PROVIDER_CONTINUATION_INTEGRITY_KEY: OnceLock<[u8; 32]> = OnceLock::new();

/// Provider-neutral metadata needed to bind an opaque native compaction artifact to its wire
/// contract without putting provider-specific request fields in the kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeProviderCompactionMetadata {
    /// Process-keyed fingerprint of the configured provider route. The route itself is never
    /// written to the session stream.
    pub provider_route_fingerprint: String,
    /// Versioned provider/model metadata contract used to interpret the artifact later.
    pub model_metadata_profile: VersionedProfileIdentity,
    /// Versioned native wire contract that produced the opaque payload.
    pub wire_profile: VersionedProfileIdentity,
    /// Provider-local protocol family name, not a kernel protocol enum.
    pub wire_protocol: String,
    /// Provider-local wire schema revision.
    pub wire_schema_version: String,
    /// Versioned rules for composing the opaque artifact into a later request.
    pub composition_profile: VersionedProfileIdentity,
    /// Provider-local label for the opaque artifact payload.
    pub artifact_kind: String,
    /// Sensitivity inherited by the encrypted artifact.
    pub sensitivity: ContextSensitivity,
}

impl NativeProviderCompactionMetadata {
    pub(crate) fn validate_for_request(&self, provider_name: &str, model_name: &str) -> Result<()> {
        validate_provider_profiles(
            provider_name,
            &self.provider_route_fingerprint,
            model_name,
            &self.model_metadata_profile,
            &self.wire_profile,
            &self.wire_protocol,
            &self.wire_schema_version,
        )?;
        self.composition_profile.validate()?;
        validate_label(
            "provider continuation native compaction artifact kind",
            &self.artifact_kind,
        )
    }
}

/// Derives a session-bound, process-keyed fingerprint for one configured provider route.
///
/// The route may contain deployment-specific information, so it is never placed directly in a
/// durable event. The returned tag is suitable only for in-process audit correlation and fails
/// closed after process restart if an unproven new request attempts to reuse it.
pub fn provider_continuation_route_fingerprint(
    session_scope_id: &str,
    provider_name: &str,
    route: &str,
) -> Result<String> {
    provider_continuation_integrity_tag(
        session_scope_id,
        "provider_route",
        &[provider_name.as_bytes(), route.as_bytes()],
    )
}

/// Derives a session-bound integrity tag for opaque native payload bytes without retaining them
/// in the JSONL stream.
pub fn provider_continuation_observed_payload_integrity_tag(
    session_scope_id: &str,
    payload: &[u8],
) -> Result<String> {
    provider_continuation_integrity_tag(session_scope_id, "observed_payload", &[payload])
}

/// Stable identity for one provider-observed continuation block or item.
pub type ProviderContinuationObservationId = String;

/// Stable identity for one native continuation candidate.
pub type ProviderContinuationCandidateId = String;

/// Stable identity for one provider-observed resolution plan.
pub type ProviderObservedResolutionPlanId = String;

/// Maximum unresolved provider tool calls referenced by one continuation candidate.
pub const MAX_PROVIDER_CONTINUATION_TOOL_CLOSURE_REFS: usize = 64;

/// Maximum aggregate byte size of unresolved provider tool-call identities in one candidate.
pub const MAX_PROVIDER_CONTINUATION_TOOL_CLOSURE_REFERENCE_BYTES: usize = 16 * 1024;

/// Maximum absolute wait lease for response-local provider tool calls.
pub const MAX_PROVIDER_CONTINUATION_TOOL_CLOSURE_LEASE_MS: u64 = 7 * 24 * 60 * 60 * 1_000;

/// Stable identity for one provider-native payload without exposing the payload bytes in JSONL.
pub type ProviderContinuationPayloadId = String;

/// Opaque identity of one artifact managed by the future session payload backend.
pub type ProviderContinuationArtifactId = String;

/// Opaque identity of one encrypted server-handle state managed by that backend.
pub type ProviderContinuationStateId = String;

/// Native continuation payload identity. K25.12A records only this bounded identity; K25.12B
/// adds the storage manifest, lifecycle, retention, and deletion protocol.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    rename_all = "snake_case",
    tag = "kind",
    content = "digest",
    deny_unknown_fields
)]
pub enum ProviderContinuationPayloadIntegrity {
    Sha256(String),
    KeyedMac(String),
}

impl ProviderContinuationPayloadIntegrity {
    fn validate_for_kind(&self, kind: ProviderContinuationPayloadKind) -> Result<()> {
        match (kind, self) {
            (ProviderContinuationPayloadKind::Artifact, Self::Sha256(digest)) => {
                validate_digest("provider continuation artifact digest", digest, "sha256:")
            }
            (ProviderContinuationPayloadKind::HandleState, Self::KeyedMac(tag)) => validate_digest(
                "provider continuation handle integrity tag",
                tag,
                "hmac-sha256:",
            ),
            (ProviderContinuationPayloadKind::Artifact, Self::KeyedMac(_)) => {
                bail!("provider continuation artifact must use a sha256 digest")
            }
            (ProviderContinuationPayloadKind::HandleState, Self::Sha256(_)) => {
                bail!("provider continuation handle state must use a keyed integrity tag")
            }
        }
    }
}

/// The payload class whose bytes are intentionally absent from the durable event stream.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderContinuationPayloadKind {
    Artifact,
    HandleState,
}

impl ProviderContinuationPayloadKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Artifact => "artifact",
            Self::HandleState => "handle_state",
        }
    }
}

/// Bounded identity for a payload managed by the future session-scoped continuation store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderContinuationPayloadIdentity {
    pub payload_id: ProviderContinuationPayloadId,
    pub integrity: ProviderContinuationPayloadIntegrity,
    pub byte_size: u64,
}

impl ProviderContinuationPayloadIdentity {
    fn validate_for_kind(
        &self,
        candidate_id: &str,
        kind: ProviderContinuationPayloadKind,
    ) -> Result<()> {
        validate_identity("provider continuation payload id", &self.payload_id, 512)?;
        if self.payload_id != provider_continuation_payload_id(candidate_id, kind) {
            bail!("provider continuation payload id does not match its candidate")
        }
        if self.byte_size == 0 {
            bail!("provider continuation payload byte size must be non-zero")
        }
        self.integrity.validate_for_kind(kind)
    }
}

/// The only provider-native artifact composition shapes the kernel may name.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderArtifactComposition {
    ReplacementWindow,
    PrefixSegment,
}

/// Provider-neutral reference to an opaque compacted artifact payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderCompactionArtifactRef {
    pub candidate_id: ProviderContinuationCandidateId,
    pub payload: ProviderContinuationPayloadIdentity,
    pub artifact_id: ProviderContinuationArtifactId,
    pub provider_name: String,
    pub provider_route_fingerprint: String,
    pub model_name: String,
    pub model_metadata_profile: VersionedProfileIdentity,
    pub wire_profile: VersionedProfileIdentity,
    pub wire_protocol: String,
    pub wire_schema_version: String,
    pub composition_profile: VersionedProfileIdentity,
    pub artifact_kind: String,
    pub composition_mode: ProviderArtifactComposition,
    pub covers_through: CompactionCursor,
    pub request_fingerprint: String,
    pub sensitivity: ContextSensitivity,
}

impl ProviderCompactionArtifactRef {
    fn validate_shape(&self) -> Result<()> {
        validate_candidate_binding(
            &self.candidate_id,
            &self.provider_name,
            &self.provider_route_fingerprint,
            &self.model_name,
            &self.model_metadata_profile,
            &self.wire_profile,
            &self.wire_protocol,
            &self.wire_schema_version,
            &self.composition_profile,
            &self.request_fingerprint,
        )?;
        self.payload.validate_for_kind(
            &self.candidate_id,
            ProviderContinuationPayloadKind::Artifact,
        )?;
        validate_identity("provider continuation artifact id", &self.artifact_id, 512)?;
        validate_label("provider continuation artifact kind", &self.artifact_kind)?;
        validate_cursor_shape(&self.covers_through)
    }
}

/// Provider-neutral reference to an opaque server-side continuation handle state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderContinuationHandleRef {
    pub candidate_id: ProviderContinuationCandidateId,
    pub payload: ProviderContinuationPayloadIdentity,
    pub state_id: ProviderContinuationStateId,
    pub provider_name: String,
    pub provider_route_fingerprint: String,
    pub model_name: String,
    pub model_metadata_profile: VersionedProfileIdentity,
    pub wire_profile: VersionedProfileIdentity,
    pub wire_protocol: String,
    pub wire_schema_version: String,
    pub composition_profile: VersionedProfileIdentity,
    pub covers_through: CompactionCursor,
    pub request_fingerprint: String,
    pub sensitivity: ContextSensitivity,
    pub expires_at_unix_ms: Option<u64>,
}

impl ProviderContinuationHandleRef {
    fn validate_shape(&self) -> Result<()> {
        validate_candidate_binding(
            &self.candidate_id,
            &self.provider_name,
            &self.provider_route_fingerprint,
            &self.model_name,
            &self.model_metadata_profile,
            &self.wire_profile,
            &self.wire_protocol,
            &self.wire_schema_version,
            &self.composition_profile,
            &self.request_fingerprint,
        )?;
        self.payload.validate_for_kind(
            &self.candidate_id,
            ProviderContinuationPayloadKind::HandleState,
        )?;
        validate_identity("provider continuation state id", &self.state_id, 512)?;
        if self.expires_at_unix_ms == Some(0) {
            bail!("provider continuation handle expiry must be non-zero when present")
        }
        validate_cursor_shape(&self.covers_through)
    }
}

/// Exactly one opaque native continuation representation is selected for a candidate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ProviderContinuationCandidate {
    Artifact(ProviderCompactionArtifactRef),
    Handle(ProviderContinuationHandleRef),
}

impl ProviderContinuationCandidate {
    /// Returns the durable candidate identity.
    #[must_use]
    pub fn candidate_id(&self) -> &str {
        match self {
            Self::Artifact(reference) => &reference.candidate_id,
            Self::Handle(reference) => &reference.candidate_id,
        }
    }

    /// Returns the payload identity without exposing provider-private bytes.
    #[must_use]
    pub fn payload(&self) -> &ProviderContinuationPayloadIdentity {
        match self {
            Self::Artifact(reference) => &reference.payload,
            Self::Handle(reference) => &reference.payload,
        }
    }

    /// Returns the kind expected by the future payload manifest.
    #[must_use]
    pub const fn payload_kind(&self) -> ProviderContinuationPayloadKind {
        match self {
            Self::Artifact(_) => ProviderContinuationPayloadKind::Artifact,
            Self::Handle(_) => ProviderContinuationPayloadKind::HandleState,
        }
    }

    fn covers_through(&self) -> &CompactionCursor {
        match self {
            Self::Artifact(reference) => &reference.covers_through,
            Self::Handle(reference) => &reference.covers_through,
        }
    }

    fn request_fingerprint(&self) -> &str {
        match self {
            Self::Artifact(reference) => &reference.request_fingerprint,
            Self::Handle(reference) => &reference.request_fingerprint,
        }
    }

    fn validate_resolution_target_identity(
        &self,
        identity: &ProviderContinuationTargetExecutionIdentity,
    ) -> Result<()> {
        let (
            provider_name,
            provider_route_fingerprint,
            model_name,
            model_metadata_profile,
            wire_profile,
            wire_protocol,
            wire_schema_version,
            composition_profile,
        ) = match self {
            Self::Artifact(reference) => (
                &reference.provider_name,
                &reference.provider_route_fingerprint,
                &reference.model_name,
                &reference.model_metadata_profile,
                &reference.wire_profile,
                &reference.wire_protocol,
                &reference.wire_schema_version,
                &reference.composition_profile,
            ),
            Self::Handle(reference) => (
                &reference.provider_name,
                &reference.provider_route_fingerprint,
                &reference.model_name,
                &reference.model_metadata_profile,
                &reference.wire_profile,
                &reference.wire_protocol,
                &reference.wire_schema_version,
                &reference.composition_profile,
            ),
        };
        if provider_name != &identity.provider_name
            || provider_route_fingerprint != &identity.provider_route_fingerprint
            || model_name != &identity.model_name
            || model_metadata_profile != &identity.model_metadata_profile
            || wire_profile != &identity.wire_profile
            || wire_protocol != &identity.wire_protocol
            || wire_schema_version != &identity.wire_schema_version
            || composition_profile != &identity.composition_profile
        {
            bail!("provider observed resolution target identity does not match its candidate")
        }
        Ok(())
    }

    fn validate_observation_binding(
        &self,
        observation: &ProviderContinuationObservedEntry,
    ) -> Result<()> {
        let (
            provider_name,
            provider_route_fingerprint,
            model_name,
            model_metadata_profile,
            wire_profile,
            wire_protocol,
            wire_schema_version,
        ) = match self {
            Self::Artifact(reference) => (
                &reference.provider_name,
                &reference.provider_route_fingerprint,
                &reference.model_name,
                &reference.model_metadata_profile,
                &reference.wire_profile,
                &reference.wire_protocol,
                &reference.wire_schema_version,
            ),
            Self::Handle(reference) => (
                &reference.provider_name,
                &reference.provider_route_fingerprint,
                &reference.model_name,
                &reference.model_metadata_profile,
                &reference.wire_profile,
                &reference.wire_protocol,
                &reference.wire_schema_version,
            ),
        };
        if provider_name != &observation.provider_name
            || provider_route_fingerprint != &observation.provider_route_fingerprint
            || model_name != &observation.model_name
            || model_metadata_profile != &observation.model_metadata_profile
            || wire_profile != &observation.wire_profile
            || wire_protocol != &observation.wire_protocol
            || wire_schema_version != &observation.wire_schema_version
        {
            bail!("provider observed resolution candidate does not match its observation")
        }
        Ok(())
    }

    fn validate_shape(&self) -> Result<()> {
        match self {
            Self::Artifact(reference) => reference.validate_shape(),
            Self::Handle(reference) => reference.validate_shape(),
        }
    }
}

/// Opaque storage identity for a payload. It contains neither a filesystem path nor provider
/// bytes; K25.12B2 owns backend resolution and encryption.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ProviderContinuationPayloadStorageRef {
    Artifact {
        artifact_id: ProviderContinuationArtifactId,
    },
    SensitiveState {
        state_id: ProviderContinuationStateId,
        key_slot_id: String,
    },
}

impl ProviderContinuationPayloadStorageRef {
    fn validate_for_kind(&self, kind: ProviderContinuationPayloadKind) -> Result<()> {
        match (kind, self) {
            (ProviderContinuationPayloadKind::Artifact, Self::Artifact { artifact_id }) => {
                validate_identity("provider continuation artifact id", artifact_id, 512)
            }
            (
                ProviderContinuationPayloadKind::HandleState,
                Self::SensitiveState {
                    state_id,
                    key_slot_id,
                },
            ) => {
                validate_identity("provider continuation state id", state_id, 512)?;
                validate_identity("provider continuation key slot id", key_slot_id, 512)
            }
            (ProviderContinuationPayloadKind::Artifact, Self::SensitiveState { .. }) => {
                bail!("provider continuation artifact payload must use artifact storage")
            }
            (ProviderContinuationPayloadKind::HandleState, Self::Artifact { .. }) => {
                bail!("provider continuation handle payload must use sensitive-state storage")
            }
        }
    }

    fn validate_for_candidate(&self, candidate: &ProviderContinuationCandidate) -> Result<()> {
        self.validate_for_kind(candidate.payload_kind())?;
        match (candidate, self) {
            (
                ProviderContinuationCandidate::Artifact(candidate),
                Self::Artifact { artifact_id },
            ) if artifact_id == &candidate.artifact_id => Ok(()),
            (
                ProviderContinuationCandidate::Handle(candidate),
                Self::SensitiveState { state_id, .. },
            ) if state_id == &candidate.state_id => Ok(()),
            (ProviderContinuationCandidate::Artifact(_), Self::Artifact { .. }) => {
                bail!("provider continuation artifact storage id does not match its candidate")
            }
            (ProviderContinuationCandidate::Handle(_), Self::SensitiveState { .. }) => {
                bail!("provider continuation handle storage id does not match its candidate")
            }
            _ => unreachable!("storage kind was validated before matching candidate identity"),
        }
    }
}

/// Durable source from which one payload manifest is allowed to originate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ProviderContinuationPayloadSource {
    Initiated {
        started_event_id: EventId,
        attempt_id: CompactionAttemptId,
    },
    ProviderObserved {
        observation_event_id: EventId,
        observation_id: ProviderContinuationObservationId,
    },
}

/// Append-only lifecycle state for one opaque payload manifest.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderContinuationPayloadLifecycleState {
    Committed,
    Invalidated,
    OrphanDiscovered,
    Deleted,
}

impl ProviderContinuationPayloadLifecycleState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Invalidated => "invalidated",
            Self::OrphanDiscovered => "orphan_discovered",
            Self::Deleted => "deleted",
        }
    }
}

/// Direct-JSON manifest and lifecycle record for one provider-native payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderContinuationPayloadLifecycleEntry {
    pub schema_version: u16,
    pub payload_id: ProviderContinuationPayloadId,
    pub candidate_id: ProviderContinuationCandidateId,
    pub source: ProviderContinuationPayloadSource,
    pub kind: ProviderContinuationPayloadKind,
    pub storage_ref: ProviderContinuationPayloadStorageRef,
    pub integrity: ProviderContinuationPayloadIntegrity,
    pub byte_size: u64,
    pub state: ProviderContinuationPayloadLifecycleState,
    pub reason: Option<String>,
}

impl ProviderContinuationPayloadLifecycleEntry {
    pub(crate) fn validate_shape(&self) -> Result<()> {
        if self.schema_version != PROVIDER_CONTINUATION_SCHEMA_VERSION {
            bail!(
                "unsupported provider continuation schema version {}",
                self.schema_version
            )
        }
        validate_identity("provider continuation payload id", &self.payload_id, 512)?;
        validate_identity(
            "provider continuation candidate id",
            &self.candidate_id,
            512,
        )?;
        match &self.source {
            ProviderContinuationPayloadSource::Initiated {
                started_event_id,
                attempt_id,
            } => {
                validate_identity(
                    "provider continuation started source event id",
                    started_event_id,
                    512,
                )?;
                validate_identity("provider continuation source attempt id", attempt_id, 512)?;
            }
            ProviderContinuationPayloadSource::ProviderObserved {
                observation_event_id,
                observation_id,
            } => {
                validate_identity(
                    "provider continuation observation source event id",
                    observation_event_id,
                    512,
                )?;
                validate_identity("provider continuation observation id", observation_id, 512)?;
            }
        }
        self.storage_ref.validate_for_kind(self.kind)?;
        self.integrity.validate_for_kind(self.kind)?;
        if self.byte_size == 0 {
            bail!("provider continuation payload byte size must be non-zero")
        }
        match (&self.state, &self.reason) {
            (ProviderContinuationPayloadLifecycleState::Committed, None) => {}
            (ProviderContinuationPayloadLifecycleState::Committed, Some(_)) => {
                bail!("provider continuation committed payload must not carry a lifecycle reason")
            }
            (_, Some(reason)) => {
                validate_identity("provider continuation lifecycle reason", reason, 1024)?
            }
            (_, None) => bail!("provider continuation lifecycle transition requires a reason"),
        }
        Ok(())
    }
}

/// Provider response evidence from which a native continuation candidate may be derived.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderContinuationObservedEntry {
    pub schema_version: u16,
    pub observation_id: ProviderContinuationObservationId,
    pub physical_attempt_id: ProviderPhysicalAttemptId,
    pub response_item_ordinal: u32,
    pub observed_payload_integrity_tag: String,
    pub provider_name: String,
    pub provider_route_fingerprint: String,
    pub model_name: String,
    pub model_metadata_profile: VersionedProfileIdentity,
    pub wire_profile: VersionedProfileIdentity,
    pub wire_protocol: String,
    pub wire_schema_version: String,
    pub provider_request_id: Option<String>,
    pub provider_response_id: Option<String>,
    pub observed_at_unix_ms: u64,
}

impl ProviderContinuationObservedEntry {
    pub(crate) fn validate_for_session(&self, session_id: &str) -> Result<()> {
        if self.schema_version != PROVIDER_CONTINUATION_SCHEMA_VERSION {
            bail!(
                "unsupported provider continuation schema version {}",
                self.schema_version
            )
        }
        validate_identity(
            "provider continuation observation id",
            &self.observation_id,
            512,
        )?;
        validate_identity(
            "provider continuation physical attempt id",
            &self.physical_attempt_id,
            512,
        )?;
        validate_digest(
            "provider continuation observed payload integrity tag",
            &self.observed_payload_integrity_tag,
            "hmac-sha256:",
        )?;
        validate_provider_profiles(
            &self.provider_name,
            &self.provider_route_fingerprint,
            &self.model_name,
            &self.model_metadata_profile,
            &self.wire_profile,
            &self.wire_protocol,
            &self.wire_schema_version,
        )?;
        if let Some(provider_request_id) = &self.provider_request_id {
            validate_identity("provider continuation request id", provider_request_id, 512)?;
        }
        if let Some(provider_response_id) = &self.provider_response_id {
            validate_identity(
                "provider continuation response id",
                provider_response_id,
                512,
            )?;
        }
        if self.observation_id
            != provider_continuation_observation_id(
                session_id,
                &self.provider_route_fingerprint,
                &self.physical_attempt_id,
                self.response_item_ordinal,
                &self.observed_payload_integrity_tag,
            )
        {
            bail!("provider continuation observation id does not match its durable source")
        }
        Ok(())
    }
}

/// Critical record binding one deterministic native candidate to its prior durable source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderContinuationCandidateRecordedEntry {
    pub schema_version: u16,
    pub candidate_id: ProviderContinuationCandidateId,
    pub observation_id: Option<ProviderContinuationObservationId>,
    pub candidate: ProviderContinuationCandidate,
    pub resolution_mode: ProviderContinuationResolutionMode,
    pub activation_gate: ProviderContinuationActivationGate,
    pub source_event_id: EventId,
    pub created_at_unix_ms: u64,
}

impl ProviderContinuationCandidateRecordedEntry {
    pub(crate) fn validate_shape(&self) -> Result<()> {
        if self.schema_version != PROVIDER_CONTINUATION_SCHEMA_VERSION {
            bail!(
                "unsupported provider continuation schema version {}",
                self.schema_version
            )
        }
        validate_identity(
            "provider continuation candidate id",
            &self.candidate_id,
            512,
        )?;
        if let Some(observation_id) = &self.observation_id {
            validate_identity("provider continuation observation id", observation_id, 512)?;
        }
        validate_identity(
            "provider continuation source event id",
            &self.source_event_id,
            512,
        )?;
        self.candidate.validate_shape()?;
        if self.candidate_id != self.candidate.candidate_id() {
            bail!("provider continuation candidate id does not match its reference")
        }
        if self.created_at_unix_ms == 0 {
            bail!("provider continuation candidate creation time must be non-zero")
        }
        self.activation_gate
            .validate_shape(self.created_at_unix_ms)?;
        Ok(())
    }
}

/// Why a durable provider-observed candidate can no longer proceed toward activation.
///
/// This records an auditable terminal for a candidate; it does not delete its payload or change
/// the active compaction boundary. Cleanup remains a separately ordered lifecycle step.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderContinuationCandidateInvalidationReason {
    FrozenEvidenceRejected,
    ResolutionPlanPersistenceAbsent,
    ActivationLeaseExpired,
    SourceAttemptDidNotComplete,
}

/// Which durable evidence authorizes a provider-observed candidate invalidation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ProviderContinuationCandidateInvalidationBasis {
    /// The candidate has no durable resolution plan, so no plan-derived evidence may be claimed.
    SourceOnly,
    /// The candidate's exact durable plan is the authoritative frozen evidence.
    ResolutionPlan {
        resolution_plan_id: ProviderObservedResolutionPlanId,
    },
}

impl ProviderContinuationCandidateInvalidationBasis {
    fn validate_shape(&self) -> Result<()> {
        if let Self::ResolutionPlan { resolution_plan_id } = self {
            validate_identity(
                "provider continuation invalidation resolution plan id",
                resolution_plan_id,
                512,
            )?;
        }
        Ok(())
    }
}

/// Recovery-critical terminal that invalidates exactly one provider-observed candidate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderContinuationCandidateInvalidatedEntry {
    pub schema_version: u16,
    pub candidate_id: ProviderContinuationCandidateId,
    pub observation_id: ProviderContinuationObservationId,
    pub source_event_id: EventId,
    pub basis: ProviderContinuationCandidateInvalidationBasis,
    pub reason: ProviderContinuationCandidateInvalidationReason,
    pub invalidated_at_unix_ms: u64,
}

impl ProviderContinuationCandidateInvalidatedEntry {
    pub(crate) fn validate_shape(&self) -> Result<()> {
        if self.schema_version != PROVIDER_CONTINUATION_SCHEMA_VERSION {
            bail!(
                "unsupported provider continuation schema version {}",
                self.schema_version
            )
        }
        validate_identity(
            "provider continuation invalidation candidate id",
            &self.candidate_id,
            512,
        )?;
        validate_identity(
            "provider continuation invalidation observation id",
            &self.observation_id,
            512,
        )?;
        validate_identity(
            "provider continuation invalidation source event id",
            &self.source_event_id,
            512,
        )?;
        self.basis.validate_shape()?;
        if self.invalidated_at_unix_ms == 0 {
            bail!("provider continuation invalidation time must be non-zero")
        }
        Ok(())
    }
}

/// Whether a continuation candidate can proceed immediately or must wait for response-local tool
/// calls to close in the durable stream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ProviderContinuationActivationGate {
    Immediate,
    AwaitingToolClosure {
        tool_calls: Vec<ProviderToolCallClosureRef>,
        lease_expires_at_unix_ms: u64,
    },
}

impl ProviderContinuationActivationGate {
    fn validate_shape(&self, created_at_unix_ms: u64) -> Result<()> {
        match self {
            Self::Immediate => Ok(()),
            Self::AwaitingToolClosure {
                tool_calls,
                lease_expires_at_unix_ms,
            } => {
                if tool_calls.is_empty() {
                    bail!("provider continuation tool-closure gate has no tool calls")
                }
                if tool_calls.len() > MAX_PROVIDER_CONTINUATION_TOOL_CLOSURE_REFS {
                    bail!("provider continuation tool-closure gate has too many tool calls")
                }
                let maximum_expiry = created_at_unix_ms
                    .checked_add(MAX_PROVIDER_CONTINUATION_TOOL_CLOSURE_LEASE_MS)
                    .context("provider continuation tool-closure lease overflowed")?;
                if *lease_expires_at_unix_ms <= created_at_unix_ms
                    || *lease_expires_at_unix_ms > maximum_expiry
                {
                    bail!("provider continuation tool-closure lease is outside its absolute limit")
                }
                let mut previous: Option<(&str, &str)> = None;
                let mut total_bytes = 0usize;
                for tool_call in tool_calls {
                    tool_call.validate_shape()?;
                    let key = (
                        tool_call.tool_call_event_id.as_str(),
                        tool_call.tool_call_id.as_str(),
                    );
                    if previous.is_some_and(|prior| key <= prior) {
                        bail!("provider continuation tool-closure refs are not canonical")
                    }
                    previous = Some(key);
                    total_bytes = total_bytes
                        .checked_add(tool_call.tool_call_event_id.len())
                        .and_then(|bytes| bytes.checked_add(tool_call.tool_call_id.len()))
                        .context("provider continuation tool-closure refs overflowed")?;
                }
                if total_bytes > MAX_PROVIDER_CONTINUATION_TOOL_CLOSURE_REFERENCE_BYTES {
                    bail!("provider continuation tool-closure refs exceed the byte limit")
                }
                Ok(())
            }
        }
    }
}

/// Exact durable response-local tool call that must receive a matching result before activation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderToolCallClosureRef {
    pub tool_call_id: String,
    pub tool_call_event_id: EventId,
}

impl ProviderToolCallClosureRef {
    fn validate_shape(&self) -> Result<()> {
        validate_identity(
            "provider continuation tool call id",
            &self.tool_call_id,
            512,
        )?;
        validate_identity(
            "provider continuation tool call event id",
            &self.tool_call_event_id,
            512,
        )
    }
}

/// Direct durable proof that one response-local provider tool call closed within its candidate's
/// absolute lease. The result payload itself remains in the normal session stream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderContinuationToolClosureRecordedEntry {
    pub schema_version: u16,
    pub candidate_id: ProviderContinuationCandidateId,
    pub tool_call: ProviderToolCallClosureRef,
    pub tool_result_event_id: EventId,
    pub closed_at_unix_ms: u64,
}

impl ProviderContinuationToolClosureRecordedEntry {
    pub(crate) fn validate_shape(&self) -> Result<()> {
        if self.schema_version != PROVIDER_CONTINUATION_SCHEMA_VERSION {
            bail!(
                "unsupported provider continuation schema version {}",
                self.schema_version
            )
        }
        validate_identity(
            "provider continuation closure candidate id",
            &self.candidate_id,
            512,
        )?;
        self.tool_call.validate_shape()?;
        validate_identity(
            "provider continuation tool result event id",
            &self.tool_result_event_id,
            512,
        )?;
        if self.closed_at_unix_ms == 0 {
            bail!("provider continuation tool closure time must be non-zero")
        }
        Ok(())
    }
}

/// The only durable resolution paths allowed for a provider-observed native candidate.
///
/// This records authorization intent, not provider I/O. `NativePlusPortableModelCheckpoint`
/// may authorize a later semantic-compressor request, but only a separate physical-attempt
/// record can prove that request was actually sent.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderContinuationResolutionMode {
    NativeOnly,
    NativePlusPortableModelCheckpoint,
}

/// Complete target-provider identity frozen by a provider-observed resolution plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderContinuationTargetExecutionIdentity {
    pub provider_name: String,
    pub provider_route_fingerprint: String,
    pub model_name: String,
    pub model_metadata_profile: VersionedProfileIdentity,
    pub wire_profile: VersionedProfileIdentity,
    pub wire_protocol: String,
    pub wire_schema_version: String,
    pub composition_profile: VersionedProfileIdentity,
    pub token_measurement_profile: VersionedProfileIdentity,
    pub hosted_parity_profile: Option<VersionedProfileIdentity>,
}

impl ProviderContinuationTargetExecutionIdentity {
    fn validate_shape(&self) -> Result<()> {
        validate_provider_profiles(
            &self.provider_name,
            &self.provider_route_fingerprint,
            &self.model_name,
            &self.model_metadata_profile,
            &self.wire_profile,
            &self.wire_protocol,
            &self.wire_schema_version,
        )?;
        self.composition_profile.validate()?;
        self.token_measurement_profile.validate()?;
        if let Some(profile) = &self.hosted_parity_profile {
            profile.validate()?;
        }
        Ok(())
    }

    fn token_binding(&self) -> TokenMeasurementBinding {
        TokenMeasurementBinding {
            schema_version: crate::COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            provider_name: self.provider_name.clone(),
            model_name: self.model_name.clone(),
            wire_profile: self.wire_profile.clone(),
            token_measurement_profile: self.token_measurement_profile.clone(),
            hosted_parity_profile: self.hosted_parity_profile.clone(),
        }
    }
}

/// Provider-neutral execution identity for a later portable semantic checkpoint stage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderContinuationSemanticCompressorIdentity {
    pub provider_name: String,
    pub provider_route_fingerprint: String,
    pub model_name: String,
    pub model_metadata_profile: VersionedProfileIdentity,
    pub wire_profile: VersionedProfileIdentity,
    pub token_measurement_profile: VersionedProfileIdentity,
    pub hosted_parity_profile: Option<VersionedProfileIdentity>,
    pub request_budget_profile: VersionedProfileIdentity,
    pub prompt_profile: VersionedProfileIdentity,
    pub checkpoint_schema_profile: VersionedProfileIdentity,
    pub validator_profile: VersionedProfileIdentity,
}

impl ProviderContinuationSemanticCompressorIdentity {
    fn validate_shape(&self) -> Result<()> {
        validate_label("semantic compressor provider name", &self.provider_name)?;
        validate_digest(
            "semantic compressor provider route fingerprint",
            &self.provider_route_fingerprint,
            "hmac-sha256:",
        )?;
        validate_label("semantic compressor model name", &self.model_name)?;
        self.model_metadata_profile.validate()?;
        self.wire_profile.validate()?;
        self.token_measurement_profile.validate()?;
        if let Some(profile) = &self.hosted_parity_profile {
            profile.validate()?;
        }
        self.request_budget_profile.validate()?;
        self.prompt_profile.validate()?;
        self.checkpoint_schema_profile.validate()?;
        self.validator_profile.validate()?;
        Ok(())
    }

    fn token_binding(&self) -> TokenMeasurementBinding {
        TokenMeasurementBinding {
            schema_version: crate::COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            provider_name: self.provider_name.clone(),
            model_name: self.model_name.clone(),
            wire_profile: self.wire_profile.clone(),
            token_measurement_profile: self.token_measurement_profile.clone(),
            hosted_parity_profile: self.hosted_parity_profile.clone(),
        }
    }
}

/// Frozen target-input evidence shared by native before/after forms.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderContinuationTargetTokenEvidence {
    pub tokens: u64,
    pub material_fingerprint: String,
    pub binding: TokenMeasurementBinding,
    pub provider_model_snapshot: Option<String>,
    pub provider_system_fingerprint: Option<String>,
}

impl ProviderContinuationTargetTokenEvidence {
    fn validate_for_target(
        &self,
        expected_material_fingerprint: &str,
        expected_binding: &TokenMeasurementBinding,
        requires_hosted_parity: bool,
    ) -> Result<()> {
        if self.tokens == 0 {
            bail!("provider continuation token evidence must be non-zero")
        }
        validate_digest(
            "provider continuation token material fingerprint",
            &self.material_fingerprint,
            "hmac-sha256:",
        )?;
        self.binding.validate()?;
        if self.material_fingerprint != expected_material_fingerprint {
            bail!("provider continuation token evidence material fingerprint drifted")
        }
        if &self.binding != expected_binding {
            bail!("provider continuation token evidence provider or profile binding drifted")
        }
        if self.binding.hosted_parity_profile.is_some() != requires_hosted_parity {
            bail!("provider continuation token evidence hosted-parity mode is invalid")
        }
        if let Some(snapshot) = &self.provider_model_snapshot {
            validate_identity("provider continuation model snapshot", snapshot, 512)?;
        }
        if let Some(fingerprint) = &self.provider_system_fingerprint {
            validate_identity("provider continuation system fingerprint", fingerprint, 512)?;
        }
        Ok(())
    }
}

/// Target input evidence before applying a provider-native continuation candidate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    rename_all = "snake_case",
    tag = "kind",
    content = "evidence",
    deny_unknown_fields
)]
pub enum ProviderContinuationBeforeInputTokenCount {
    Exact(ProviderContinuationTargetTokenEvidence),
    ConservativeLowerBound(ProviderContinuationTargetTokenEvidence),
}

impl ProviderContinuationBeforeInputTokenCount {
    pub(crate) fn guaranteed_tokens(&self) -> u64 {
        match self {
            Self::Exact(evidence) | Self::ConservativeLowerBound(evidence) => evidence.tokens,
        }
    }

    fn material_fingerprint(&self) -> &str {
        match self {
            Self::Exact(evidence) | Self::ConservativeLowerBound(evidence) => {
                &evidence.material_fingerprint
            }
        }
    }

    fn validate_for_target(
        &self,
        expected_material_fingerprint: &str,
        expected_binding: &TokenMeasurementBinding,
    ) -> Result<()> {
        match self {
            Self::Exact(evidence) => {
                evidence.validate_for_target(expected_material_fingerprint, expected_binding, true)
            }
            Self::ConservativeLowerBound(evidence) => {
                evidence.validate_for_target(expected_material_fingerprint, expected_binding, false)
            }
        }
    }
}

/// Target input evidence after applying a provider-native continuation candidate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    rename_all = "snake_case",
    tag = "kind",
    content = "evidence",
    deny_unknown_fields
)]
pub enum ProviderContinuationAfterInputTokenCount {
    Exact(ProviderContinuationTargetTokenEvidence),
    ConservativeUpperBound(ProviderContinuationTargetTokenEvidence),
}

impl ProviderContinuationAfterInputTokenCount {
    pub(crate) fn guaranteed_tokens(&self) -> u64 {
        match self {
            Self::Exact(evidence) | Self::ConservativeUpperBound(evidence) => evidence.tokens,
        }
    }

    fn validate_for_target(&self, expected_binding: &TokenMeasurementBinding) -> Result<()> {
        let evidence = match self {
            Self::Exact(evidence) | Self::ConservativeUpperBound(evidence) => evidence,
        };
        let requires_hosted_parity = matches!(self, Self::Exact(_));
        evidence.validate_for_target(
            &evidence.material_fingerprint,
            expected_binding,
            requires_hosted_parity,
        )
    }
}

/// Frozen semantic-compressor fit for a hybrid resolution plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderContinuationSemanticCompressorRequestFit {
    pub material_fingerprint: String,
    pub proof: RequestFitProof,
}

impl ProviderContinuationSemanticCompressorRequestFit {
    fn validate_for_identity(
        &self,
        identity: &ProviderContinuationSemanticCompressorIdentity,
    ) -> Result<()> {
        validate_digest(
            "semantic compressor material fingerprint",
            &self.material_fingerprint,
            "hmac-sha256:",
        )?;
        self.proof.validate_for(
            &self.material_fingerprint,
            TokenMeasurementScope::RenderedSemanticCompressorInput,
            &identity.token_binding(),
        )?;
        if self.proof.budget.budget_profile != identity.request_budget_profile {
            bail!("semantic compressor request budget profile drifted")
        }
        Ok(())
    }
}

/// Frozen branch, snapshot, cursor, and durable source-ref set for one resolution plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderObservedResolutionPlanLineage {
    pub parent_compaction_id: Option<CompactionId>,
    pub branch_id: Option<BranchId>,
    pub valid_for_snapshot: Option<WorkspaceSnapshotId>,
    pub base_projection_revision: String,
    pub fold_candidate_fingerprint: String,
    pub folded_through: CompactionCursor,
    pub retained_event_ids: Vec<EventId>,
    pub protected_event_ids: Vec<EventId>,
}

impl ProviderObservedResolutionPlanLineage {
    fn validate_shape(&self) -> Result<()> {
        if let Some(parent_compaction_id) = &self.parent_compaction_id {
            validate_identity(
                "provider observed resolution parent compaction id",
                parent_compaction_id,
                512,
            )?;
        }
        if let Some(branch_id) = &self.branch_id {
            validate_identity("provider observed resolution branch id", branch_id, 512)?;
        }
        if let Some(snapshot_id) = &self.valid_for_snapshot {
            validate_identity("provider observed resolution snapshot id", snapshot_id, 512)?;
        }
        validate_identity(
            "provider observed resolution base projection revision",
            &self.base_projection_revision,
            512,
        )?;
        validate_digest(
            "provider observed resolution fold candidate fingerprint",
            &self.fold_candidate_fingerprint,
            "hmac-sha256:",
        )?;
        validate_cursor_shape(&self.folded_through)?;
        validate_resolution_refs(
            "provider observed resolution retained refs",
            &self.retained_event_ids,
            MAX_PROVIDER_CONTINUATION_RESOLUTION_RETAINED_REFS,
        )?;
        validate_resolution_refs(
            "provider observed resolution protected refs",
            &self.protected_event_ids,
            MAX_PROVIDER_CONTINUATION_RESOLUTION_PROTECTED_REFS,
        )?;
        let total_bytes = self
            .retained_event_ids
            .iter()
            .chain(&self.protected_event_ids)
            .map(String::len)
            .sum::<usize>();
        if total_bytes > MAX_PROVIDER_CONTINUATION_RESOLUTION_REFERENCE_BYTES {
            bail!("provider observed resolution refs exceed the byte limit")
        }
        let mut refs = std::collections::BTreeSet::new();
        for event_id in self
            .retained_event_ids
            .iter()
            .chain(&self.protected_event_ids)
        {
            if !refs.insert(event_id) {
                bail!("provider observed resolution refs overlap")
            }
        }
        Ok(())
    }
}

/// Complete target-side economics policy frozen with a provider-observed resolution plan.
///
/// `target_request` proves the post-compaction request can fit. The two savings thresholds are
/// durable integers so later admission/recovery never reinterpret a changed floating-point
/// configuration value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderContinuationEffectiveCompactionBudget {
    pub target_request: EffectiveTokenBudget,
    pub minimum_savings_tokens: u64,
    pub minimum_savings_ratio_ppm: u32,
}

impl ProviderContinuationEffectiveCompactionBudget {
    fn validate_shape(&self) -> Result<()> {
        self.target_request.validate()?;
        if self.minimum_savings_ratio_ppm > 1_000_000 {
            bail!("provider continuation minimum savings ratio exceeds one million ppm")
        }
        Ok(())
    }
}

/// Immutable direct-JSON validation barrier for one provider-observed continuation candidate.
///
/// A plan is not an initiated `CompactionStarted`, does not activate the candidate, and proves no
/// provider I/O. It freezes the only inputs later C2/C3 code may use for admission/recovery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderObservedResolutionPlanRecordedEntry {
    pub schema_version: u16,
    pub resolution_plan_id: ProviderObservedResolutionPlanId,
    pub observation_id: ProviderContinuationObservationId,
    pub candidate_id: ProviderContinuationCandidateId,
    /// The candidate-record event, not the observation event or a synthetic start event.
    pub source_event_id: EventId,
    pub resolution_mode: ProviderContinuationResolutionMode,
    pub lineage: ProviderObservedResolutionPlanLineage,
    pub execution_identity: ProviderContinuationTargetExecutionIdentity,
    pub semantic_compressor: Option<ProviderContinuationSemanticCompressorIdentity>,
    pub target_budget: ProviderContinuationEffectiveCompactionBudget,
    pub before_input: ProviderContinuationBeforeInputTokenCount,
    pub native_after_input: Option<ProviderContinuationAfterInputTokenCount>,
    pub semantic_compressor_primary_fit: Option<ProviderContinuationSemanticCompressorRequestFit>,
    pub recorded_at_unix_ms: u64,
}

impl ProviderObservedResolutionPlanRecordedEntry {
    pub(crate) fn validate_shape(&self) -> Result<()> {
        if self.schema_version != PROVIDER_CONTINUATION_SCHEMA_VERSION {
            bail!(
                "unsupported provider observed resolution plan schema version {}",
                self.schema_version
            )
        }
        validate_identity(
            "provider observed resolution plan id",
            &self.resolution_plan_id,
            512,
        )?;
        validate_identity(
            "provider observed resolution observation id",
            &self.observation_id,
            512,
        )?;
        validate_identity(
            "provider observed resolution candidate id",
            &self.candidate_id,
            512,
        )?;
        validate_identity(
            "provider observed resolution source event id",
            &self.source_event_id,
            512,
        )?;
        if self.recorded_at_unix_ms == 0 {
            bail!("provider observed resolution recorded time must be non-zero")
        }
        self.lineage.validate_shape()?;
        self.execution_identity.validate_shape()?;
        self.target_budget.validate_shape()?;
        let target_binding = self.execution_identity.token_binding();
        self.before_input
            .validate_for_target(self.before_input.material_fingerprint(), &target_binding)?;
        match (
            self.resolution_mode,
            &self.semantic_compressor,
            &self.semantic_compressor_primary_fit,
            &self.native_after_input,
        ) {
            (ProviderContinuationResolutionMode::NativeOnly, None, None, Some(after_input)) => {
                after_input.validate_for_target(&target_binding)
            }
            (
                ProviderContinuationResolutionMode::NativePlusPortableModelCheckpoint,
                Some(identity),
                Some(fit),
                None,
            ) => {
                identity.validate_shape()?;
                fit.validate_for_identity(identity)
            }
            _ => bail!("provider observed resolution mode and evidence options are inconsistent"),
        }
    }
}

/// A read-only observed continuation with its durable source event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderContinuationObservationState {
    pub event_id: EventId,
    pub correlation_id: EventId,
    pub session_id: SessionId,
    pub entry: ProviderContinuationObservedEntry,
}

/// A read-only candidate before K25.12C decides whether it can become active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderContinuationCandidateState {
    pub event_id: EventId,
    pub stream_sequence: u64,
    pub session_id: SessionId,
    pub entry: ProviderContinuationCandidateRecordedEntry,
}

/// Read-only durable terminal for one provider-observed candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderContinuationCandidateInvalidationState {
    pub event_id: EventId,
    pub stream_sequence: u64,
    pub session_id: SessionId,
    pub entry: ProviderContinuationCandidateInvalidatedEntry,
}

/// Read-only provider-observed resolution plan. Its presence does not activate the candidate or
/// prove that a native/semantic provider request was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderObservedResolutionPlanState {
    pub event_id: EventId,
    pub stream_sequence: u64,
    pub session_id: SessionId,
    pub entry: ProviderObservedResolutionPlanRecordedEntry,
}

/// Read-only proof that one awaited provider tool call closed in the durable stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderContinuationToolClosureState {
    pub event_id: EventId,
    pub stream_sequence: u64,
    pub entry: ProviderContinuationToolClosureRecordedEntry,
}

/// Read-only lifecycle state for one payload manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderContinuationPayloadState {
    pub committed_event_id: EventId,
    pub manifest: ProviderContinuationPayloadLifecycleEntry,
    pub latest_event_id: EventId,
    pub latest_lifecycle: ProviderContinuationPayloadLifecycleEntry,
    pub candidate_event_id: Option<EventId>,
}

/// Why a payload must remain retained until a later lifecycle/recovery step proves cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderContinuationRetentionPinKind {
    ManifestOnly,
    CandidatePending,
    CleanupPending,
}

/// Read-only retention pin derived from the durable payload lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderContinuationRetentionPin {
    pub payload_id: ProviderContinuationPayloadId,
    pub candidate_id: ProviderContinuationCandidateId,
    pub kind: ProviderContinuationRetentionPinKind,
}

/// Read-only reconstruction of native-continuation observations and candidates.
///
/// This projection deliberately exposes no active candidate. Resolution, retention, cleanup, and
/// provider request materialization are later K25.12 slices and must not be inferred from an
/// observation or candidate record alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderContinuationProjection {
    cursor: Option<ProjectionCursor>,
    observations: BTreeMap<ProviderContinuationObservationId, ProviderContinuationObservationState>,
    candidates: BTreeMap<ProviderContinuationCandidateId, ProviderContinuationCandidateState>,
    candidate_invalidations:
        BTreeMap<ProviderContinuationCandidateId, ProviderContinuationCandidateInvalidationState>,
    tool_closures:
        BTreeMap<(ProviderContinuationCandidateId, EventId), ProviderContinuationToolClosureState>,
    resolution_plans:
        BTreeMap<ProviderObservedResolutionPlanId, ProviderObservedResolutionPlanState>,
    resolution_plan_candidates:
        BTreeMap<ProviderContinuationCandidateId, ProviderObservedResolutionPlanId>,
    event_sequences: BTreeMap<EventId, u64>,
    event_sessions: BTreeMap<EventId, SessionId>,
    event_correlations: BTreeMap<EventId, Option<EventId>>,
    tool_call_events: BTreeMap<EventId, BTreeSet<String>>,
    tool_result_events: BTreeMap<EventId, String>,
    payloads: BTreeMap<ProviderContinuationPayloadId, ProviderContinuationPayloadState>,
    candidate_sources: BTreeMap<EventId, ProviderContinuationCandidateId>,
    initiated_sources: BTreeMap<EventId, (SessionId, CompactionAttemptId)>,
}

impl ProviderContinuationProjection {
    /// Rebuilds native-continuation contract state from a validated V2 stream without mutation.
    ///
    /// # Errors
    ///
    /// Returns an error when identities, physical-attempt provenance, source ordering, or the
    /// one-candidate-per-source invariant cannot be proven.
    pub fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        let physical_attempts = ProviderPhysicalAttemptProjection::from_records(records)?;
        let mut projection = Self::default();
        for record in records {
            projection.apply_record(record, &physical_attempts)?;
        }
        Ok(projection)
    }

    /// Returns the latest applied cursor for incremental read-only consumers.
    #[must_use]
    pub fn cursor(&self) -> Option<&ProjectionCursor> {
        self.cursor.as_ref()
    }

    /// Looks up one observed native continuation by its deterministic identity.
    #[must_use]
    pub fn observation(
        &self,
        observation_id: &str,
    ) -> Option<&ProviderContinuationObservationState> {
        self.observations.get(observation_id)
    }

    /// Looks up one recorded candidate. Presence never means that it is active or requestable.
    #[must_use]
    pub fn candidate(&self, candidate_id: &str) -> Option<&ProviderContinuationCandidateState> {
        self.candidates.get(candidate_id)
    }

    /// Looks up the durable invalidation terminal for one candidate, when present.
    #[must_use]
    pub fn candidate_invalidation(
        &self,
        candidate_id: &str,
    ) -> Option<&ProviderContinuationCandidateInvalidationState> {
        self.candidate_invalidations.get(candidate_id)
    }

    /// Returns every recorded continuation candidate in deterministic candidate-id order.
    pub fn candidates(&self) -> impl Iterator<Item = &ProviderContinuationCandidateState> + '_ {
        self.candidates.values()
    }

    /// Looks up one frozen provider-observed resolution plan. Presence is a validation barrier,
    /// not an active compaction boundary or a provider-I/O outcome.
    #[must_use]
    pub fn resolution_plan(
        &self,
        resolution_plan_id: &str,
    ) -> Option<&ProviderObservedResolutionPlanState> {
        self.resolution_plans.get(resolution_plan_id)
    }

    /// Looks up the sole frozen plan for a provider-observed candidate, when one was recorded.
    #[must_use]
    pub fn resolution_plan_for_candidate(
        &self,
        candidate_id: &str,
    ) -> Option<&ProviderObservedResolutionPlanState> {
        self.resolution_plan_candidates
            .get(candidate_id)
            .and_then(|plan_id| self.resolution_plans.get(plan_id))
    }

    /// Returns every frozen provider-observed resolution plan in deterministic identity order.
    pub fn resolution_plans(
        &self,
    ) -> impl Iterator<Item = &ProviderObservedResolutionPlanState> + '_ {
        self.resolution_plans.values()
    }

    /// Returns the exact session/correlation/causation links required to append a plan for one
    /// provider-observed candidate at the current durable frontier.
    ///
    /// This is crate-internal coordination evidence, not an authorization to append or execute a
    /// plan. Callers must still hold the session writer boundary and validate the prospective
    /// stream before performing a durable write.
    pub(crate) fn resolution_plan_append_links(
        &self,
        candidate_id: &str,
    ) -> Result<(SessionId, EventId, EventId)> {
        let candidate = self.candidates.get(candidate_id).with_context(|| {
            format!("provider observed resolution plan references unknown candidate {candidate_id}")
        })?;
        let observation_id = candidate.entry.observation_id.as_deref().context(
            "provider observed resolution plan cannot bind an initiated continuation candidate",
        )?;
        let observation = self.observations.get(observation_id).with_context(|| {
            format!(
                "provider observed resolution plan references unknown observation {observation_id}"
            )
        })?;
        if candidate.session_id != observation.session_id {
            bail!("provider observed resolution plan candidate belongs to a different session")
        }
        Ok((
            candidate.session_id.clone(),
            observation.correlation_id.clone(),
            self.resolution_plan_causation(candidate)?,
        ))
    }

    /// Returns every durable tool-closure proof for one candidate in canonical call-event order.
    #[must_use]
    pub fn tool_closures_for_candidate(
        &self,
        candidate_id: &str,
    ) -> Vec<&ProviderContinuationToolClosureState> {
        self.tool_closures
            .iter()
            .filter(move |((recorded_candidate_id, _), _)| recorded_candidate_id == candidate_id)
            .map(|(_, closure)| closure)
            .collect()
    }

    /// Looks up one payload manifest and its latest durable lifecycle state.
    #[must_use]
    pub fn payload(&self, payload_id: &str) -> Option<&ProviderContinuationPayloadState> {
        self.payloads.get(payload_id)
    }

    /// Returns every durable payload lifecycle state for internal recovery coordination.
    pub(crate) fn payload_states(
        &self,
    ) -> impl Iterator<Item = &ProviderContinuationPayloadState> + '_ {
        self.payloads.values()
    }

    /// Returns every payload still pinned for a candidate, cleanup, or manifest-only recovery.
    #[must_use]
    pub fn retention_pins(&self) -> Vec<ProviderContinuationRetentionPin> {
        self.payloads
            .values()
            .filter_map(|payload| {
                let kind = match payload.latest_lifecycle.state {
                    ProviderContinuationPayloadLifecycleState::Committed
                        if payload.candidate_event_id.is_some() =>
                    {
                        if self
                            .candidate_invalidations
                            .contains_key(&payload.manifest.candidate_id)
                        {
                            ProviderContinuationRetentionPinKind::CleanupPending
                        } else {
                            ProviderContinuationRetentionPinKind::CandidatePending
                        }
                    }
                    ProviderContinuationPayloadLifecycleState::Committed => {
                        ProviderContinuationRetentionPinKind::ManifestOnly
                    }
                    ProviderContinuationPayloadLifecycleState::Invalidated
                    | ProviderContinuationPayloadLifecycleState::OrphanDiscovered => {
                        ProviderContinuationRetentionPinKind::CleanupPending
                    }
                    ProviderContinuationPayloadLifecycleState::Deleted => return None,
                };
                Some(ProviderContinuationRetentionPin {
                    payload_id: payload.manifest.payload_id.clone(),
                    candidate_id: payload.manifest.candidate_id.clone(),
                    kind,
                })
            })
            .collect()
    }

    fn apply_record(
        &mut self,
        record: &SessionStreamRecord,
        physical_attempts: &ProviderPhysicalAttemptProjection,
    ) -> Result<()> {
        let next_cursor = record.projection_cursor(PROVIDER_CONTINUATION_PROJECTION_SCHEMA_VERSION);
        let event = record.stored_event();
        match projection_apply_decision(self.cursor.as_ref(), event)? {
            ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
            ProjectionApplyDecision::Apply => {}
        }
        if self
            .event_sequences
            .insert(event.event_id.clone(), event.stream_sequence)
            .is_some()
        {
            bail!("provider continuation stream contains a duplicate event id")
        }
        self.event_sessions
            .insert(event.event_id.clone(), event.session_id.clone());
        self.event_correlations
            .insert(event.event_id.clone(), event.correlation_id.clone());
        self.record_tool_session_entry(event)?;

        match decode_typed_stored_event(event.clone())? {
            TypedStoredEventDecode::Known(typed) => match *typed {
                TypedDomainEvent::CompactionStarted(entry) => {
                    if event.correlation_id.as_deref() != Some(event.event_id.as_str())
                        || event.causation_id.is_some()
                    {
                        bail!(
                            "provider continuation initiated source must be a valid compaction start"
                        )
                    }
                    if self.initiated_sources.contains_key(&event.event_id) {
                        bail!(
                            "provider continuation initiated source event was recorded more than once"
                        )
                    }
                    self.initiated_sources.insert(
                        event.event_id.clone(),
                        (event.session_id.clone(), entry.attempt_id),
                    );
                }
                TypedDomainEvent::ProviderContinuationObserved(entry) => {
                    self.apply_observation(event, entry, physical_attempts)?;
                }
                TypedDomainEvent::ProviderContinuationPayloadLifecycleRecorded(entry) => {
                    self.apply_payload_lifecycle(event, entry)?;
                }
                TypedDomainEvent::ProviderContinuationCandidateRecorded(entry) => {
                    self.apply_candidate(event, entry)?;
                }
                TypedDomainEvent::ProviderContinuationCandidateInvalidated(entry) => {
                    self.apply_candidate_invalidation(event, entry, physical_attempts)?;
                }
                TypedDomainEvent::ProviderContinuationToolClosureRecorded(entry) => {
                    self.apply_tool_closure(event, entry)?;
                }
                TypedDomainEvent::ProviderObservedResolutionPlanRecorded(entry) => {
                    self.apply_resolution_plan(event, *entry)?;
                }
                _ => {}
            },
            TypedStoredEventDecode::UnknownNonCritical(_) => {}
        }

        self.cursor = Some(next_cursor);
        Ok(())
    }

    fn record_tool_session_entry(&mut self, event: &StoredEvent) -> Result<()> {
        let Some(entry) = session_entry_from_stored_event(event)? else {
            return Ok(());
        };
        match entry {
            SessionLogEntry::Assistant(message)
                if message.role == crate::MessageRole::Assistant =>
            {
                let tool_call_ids = message
                    .tool_calls
                    .into_iter()
                    .filter_map(|tool_call| {
                        (!tool_call.id.trim().is_empty()).then_some(tool_call.id)
                    })
                    .collect::<BTreeSet<_>>();
                if !tool_call_ids.is_empty()
                    && self
                        .tool_call_events
                        .insert(event.event_id.clone(), tool_call_ids)
                        .is_some()
                {
                    bail!("provider continuation stream repeats a tool-call event id")
                }
            }
            SessionLogEntry::ToolResult(message) if message.role == crate::MessageRole::Tool => {
                if let Some(tool_call_id) = message.tool_call_id.filter(|id| !id.trim().is_empty())
                    && self
                        .tool_result_events
                        .insert(event.event_id.clone(), tool_call_id)
                        .is_some()
                {
                    bail!("provider continuation stream repeats a tool-result event id")
                }
            }
            SessionLogEntry::User(_)
            | SessionLogEntry::Assistant(_)
            | SessionLogEntry::ToolResult(_)
            | SessionLogEntry::Control(_) => {}
        }
        Ok(())
    }

    fn apply_observation(
        &mut self,
        event: &StoredEvent,
        entry: ProviderContinuationObservedEntry,
        physical_attempts: &ProviderPhysicalAttemptProjection,
    ) -> Result<()> {
        entry.validate_for_session(&event.session_id)?;
        if event.event_id != provider_continuation_observed_event_id(&entry.observation_id) {
            bail!("provider continuation observed event id does not match its observation")
        }
        let attempt = physical_attempts
            .attempt(&entry.physical_attempt_id)
            .with_context(|| {
                format!(
                    "provider continuation observation references unknown physical attempt {}",
                    entry.physical_attempt_id
                )
            })?;
        if attempt.entry.purpose != ProviderPhysicalAttemptPurpose::NativeCompaction {
            bail!("provider continuation observation must reference a native compaction attempt")
        }
        if attempt.started_stream_sequence >= event.stream_sequence {
            bail!("provider continuation observation precedes its physical attempt")
        }
        if event.session_id != attempt.session_id() {
            bail!("provider continuation observation belongs to a different session scope")
        }
        if event.correlation_id.as_deref() != Some(attempt.started_event_id.as_str()) {
            bail!("provider continuation observation correlation does not match physical attempt")
        }
        if entry.provider_name != attempt.entry.provider_name
            || entry.model_name != attempt.entry.model_name
        {
            bail!(
                "provider continuation observation provider binding does not match physical attempt"
            )
        }
        if self.observations.contains_key(&entry.observation_id) {
            bail!(
                "provider continuation observation {} was recorded more than once",
                entry.observation_id
            )
        }
        self.observations.insert(
            entry.observation_id.clone(),
            ProviderContinuationObservationState {
                event_id: event.event_id.clone(),
                correlation_id: event
                    .correlation_id
                    .clone()
                    .expect("observation correlation was validated above"),
                session_id: event.session_id.clone(),
                entry,
            },
        );
        Ok(())
    }

    fn apply_candidate(
        &mut self,
        event: &StoredEvent,
        entry: ProviderContinuationCandidateRecordedEntry,
    ) -> Result<()> {
        entry.validate_shape()?;
        if event.event_id != provider_continuation_candidate_recorded_event_id(&entry.candidate_id)
        {
            bail!("provider continuation candidate event id does not match its candidate")
        }
        let expected_candidate_id = match &entry.observation_id {
            Some(observation_id) => {
                let observation = self.observations.get(observation_id).with_context(|| {
                    format!(
                        "provider continuation candidate references unknown observation {observation_id}"
                    )
                })?;
                if entry.source_event_id != observation.event_id {
                    bail!("provider continuation candidate source does not match its observation")
                }
                if event.session_id != observation.session_id {
                    bail!("provider continuation candidate belongs to a different session")
                }
                provider_continuation_candidate_id_from_observation(observation_id)
            }
            None => {
                let (session_id, attempt_id) = self
                    .initiated_sources
                    .get(&entry.source_event_id)
                    .with_context(|| {
                    format!(
                        "provider continuation initiated candidate source {} is missing",
                        entry.source_event_id
                    )
                })?;
                if event.session_id != *session_id {
                    bail!(
                        "provider continuation initiated candidate belongs to a different session"
                    )
                }
                provider_continuation_candidate_id_from_initiated(
                    session_id,
                    &entry.source_event_id,
                    attempt_id,
                )
            }
        };
        if entry.candidate_id != expected_candidate_id {
            bail!("provider continuation candidate id does not match its durable source")
        }
        self.validate_candidate_activation_gate(event, &entry)?;
        let payload_id = entry.candidate.payload().payload_id.clone();
        let payload_state = self.payloads.get(&payload_id).with_context(|| {
            format!(
                "provider continuation candidate {} references a missing committed payload manifest",
                entry.candidate_id
            )
        })?;
        validate_candidate_payload_manifest(&entry, payload_state)?;
        if self.candidate_sources.contains_key(&entry.source_event_id) {
            bail!("provider continuation source already has a recorded candidate")
        }
        if self.candidates.contains_key(&entry.candidate_id) {
            bail!(
                "provider continuation candidate {} was recorded more than once",
                entry.candidate_id
            )
        }
        self.candidate_sources
            .insert(entry.source_event_id.clone(), entry.candidate_id.clone());
        self.candidates.insert(
            entry.candidate_id.clone(),
            ProviderContinuationCandidateState {
                event_id: event.event_id.clone(),
                stream_sequence: event.stream_sequence,
                session_id: event.session_id.clone(),
                entry,
            },
        );
        self.payloads
            .get_mut(&payload_id)
            .expect("candidate payload manifest was checked above")
            .candidate_event_id = Some(event.event_id.clone());
        Ok(())
    }

    fn apply_candidate_invalidation(
        &mut self,
        event: &StoredEvent,
        entry: ProviderContinuationCandidateInvalidatedEntry,
        physical_attempts: &ProviderPhysicalAttemptProjection,
    ) -> Result<()> {
        entry.validate_shape()?;
        if event.event_id
            != provider_continuation_candidate_invalidated_event_id(&entry.candidate_id)
        {
            bail!("provider continuation invalidation event id does not match its candidate")
        }
        let candidate = self.candidates.get(&entry.candidate_id).with_context(|| {
            format!(
                "provider continuation invalidation references unknown candidate {}",
                entry.candidate_id
            )
        })?;
        let candidate_observation_id = candidate
            .entry
            .observation_id
            .as_deref()
            .context("provider continuation invalidation cannot bind an initiated candidate")?;
        if entry.observation_id != candidate_observation_id
            || entry.source_event_id != candidate.event_id
        {
            bail!("provider continuation invalidation source does not match its candidate")
        }
        let observation = self.observations.get(candidate_observation_id).with_context(|| {
            format!(
                "provider continuation invalidation references unknown observation {candidate_observation_id}"
            )
        })?;
        if event.session_id != observation.session_id
            || event.correlation_id.as_deref() != Some(observation.correlation_id.as_str())
        {
            bail!("provider continuation invalidation does not share its observation session chain")
        }
        if event.stream_sequence <= candidate.stream_sequence {
            bail!("provider continuation invalidation does not follow its candidate")
        }
        let source_attempt = physical_attempts
            .attempt(&observation.entry.physical_attempt_id)
            .context("provider continuation invalidation source physical attempt is missing")?;
        let terminal_sequence = source_attempt
            .terminal_stream_sequence
            .context("provider continuation invalidation source terminal is not durable")?;
        if terminal_sequence >= event.stream_sequence {
            bail!("provider continuation invalidation precedes its source terminal")
        }
        let expected_causation_id = match &entry.basis {
            ProviderContinuationCandidateInvalidationBasis::SourceOnly => {
                if self
                    .resolution_plan_candidates
                    .contains_key(&entry.candidate_id)
                {
                    bail!(
                        "provider continuation invalidation cannot claim source-only evidence after a resolution plan"
                    )
                }
                candidate.event_id.clone()
            }
            ProviderContinuationCandidateInvalidationBasis::ResolutionPlan {
                resolution_plan_id,
            } => {
                let plan = self.resolution_plans.get(resolution_plan_id).with_context(|| {
                    format!(
                        "provider continuation invalidation references unknown resolution plan {resolution_plan_id}"
                    )
                })?;
                if plan.entry.candidate_id != entry.candidate_id
                    || self.resolution_plan_candidates.get(&entry.candidate_id)
                        != Some(resolution_plan_id)
                {
                    bail!("provider continuation invalidation plan does not match its candidate")
                }
                if plan.stream_sequence >= event.stream_sequence {
                    bail!("provider continuation invalidation precedes its resolution plan")
                }
                plan.event_id.clone()
            }
        };
        if event.causation_id.as_deref() != Some(expected_causation_id.as_str()) {
            bail!("provider continuation invalidation causation does not match its evidence")
        }
        let payload_id = &candidate.entry.candidate.payload().payload_id;
        let payload = self.payloads.get(payload_id).with_context(|| {
            format!(
                "provider continuation invalidation candidate {} has no payload manifest",
                entry.candidate_id
            )
        })?;
        if payload.latest_lifecycle.state != ProviderContinuationPayloadLifecycleState::Committed
            || payload.candidate_event_id.as_deref() != Some(candidate.event_id.as_str())
        {
            bail!("provider continuation invalidation candidate payload is not committed")
        }
        if self
            .candidate_invalidations
            .contains_key(&entry.candidate_id)
        {
            bail!("provider continuation candidate was invalidated more than once")
        }
        self.candidate_invalidations.insert(
            entry.candidate_id.clone(),
            ProviderContinuationCandidateInvalidationState {
                event_id: event.event_id.clone(),
                stream_sequence: event.stream_sequence,
                session_id: event.session_id.clone(),
                entry,
            },
        );
        Ok(())
    }

    fn validate_candidate_activation_gate(
        &self,
        event: &StoredEvent,
        entry: &ProviderContinuationCandidateRecordedEntry,
    ) -> Result<()> {
        let ProviderContinuationActivationGate::AwaitingToolClosure { tool_calls, .. } =
            &entry.activation_gate
        else {
            return Ok(());
        };
        let observation_id = entry.observation_id.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "initiated continuation candidates cannot use a provider-observed tool-closure gate"
            )
        })?;
        let observation = self.observations.get(observation_id).with_context(|| {
            format!(
                "provider continuation tool-closure gate references unknown observation {observation_id}"
            )
        })?;
        if event.correlation_id.as_deref() != Some(observation.correlation_id.as_str()) {
            bail!("provider continuation tool-closure candidate correlation is invalid")
        }
        for tool_call in tool_calls {
            let sequence = self
                .event_sequences
                .get(&tool_call.tool_call_event_id)
                .with_context(|| {
                    format!(
                        "provider continuation tool-closure gate references unknown tool call event {}",
                        tool_call.tool_call_event_id
                    )
                })?;
            if *sequence >= event.stream_sequence {
                bail!("provider continuation tool call does not precede its candidate")
            }
            if self.event_sessions.get(&tool_call.tool_call_event_id) != Some(&event.session_id) {
                bail!("provider continuation tool call belongs to a different session")
            }
            if self
                .event_correlations
                .get(&tool_call.tool_call_event_id)
                .and_then(Option::as_deref)
                != Some(observation.correlation_id.as_str())
            {
                bail!("provider continuation tool call does not share the observation chain")
            }
            if !self
                .tool_call_events
                .get(&tool_call.tool_call_event_id)
                .is_some_and(|ids| ids.contains(&tool_call.tool_call_id))
            {
                bail!(
                    "provider continuation tool-closure ref does not match an assistant tool call"
                )
            }
        }
        Ok(())
    }

    fn apply_tool_closure(
        &mut self,
        event: &StoredEvent,
        entry: ProviderContinuationToolClosureRecordedEntry,
    ) -> Result<()> {
        entry.validate_shape()?;
        if event.event_id
            != provider_continuation_tool_closure_recorded_event_id(
                &entry.candidate_id,
                &entry.tool_call.tool_call_event_id,
            )
        {
            bail!("provider continuation tool closure event id does not match its call")
        }
        let candidate = self.candidates.get(&entry.candidate_id).with_context(|| {
            format!(
                "provider continuation tool closure references unknown candidate {}",
                entry.candidate_id
            )
        })?;
        if event.session_id != candidate.session_id {
            bail!("provider continuation tool closure belongs to a different session")
        }
        let observation_id = candidate
            .entry
            .observation_id
            .as_deref()
            .context("provider continuation tool closure cannot bind an initiated candidate")?;
        let observation = self.observations.get(observation_id).with_context(|| {
            format!(
                "provider continuation tool closure references unknown observation {observation_id}"
            )
        })?;
        if event.correlation_id.as_deref() != Some(observation.correlation_id.as_str()) {
            bail!("provider continuation tool closure correlation does not match its observation")
        }
        let ProviderContinuationActivationGate::AwaitingToolClosure {
            tool_calls,
            lease_expires_at_unix_ms,
        } = &candidate.entry.activation_gate
        else {
            bail!("provider continuation tool closure references an immediate candidate")
        };
        if !tool_calls
            .iter()
            .any(|expected| expected == &entry.tool_call)
        {
            bail!("provider continuation tool closure is not required by its candidate")
        }
        let call_sequence = *self
            .event_sequences
            .get(&entry.tool_call.tool_call_event_id)
            .context("provider continuation tool closure lost its call event")?;
        let result_sequence = *self
            .event_sequences
            .get(&entry.tool_result_event_id)
            .context("provider continuation tool closure references unknown result event")?;
        if self.event_sessions.get(&entry.tool_call.tool_call_event_id)
            != Some(&candidate.session_id)
            || self.event_sessions.get(&entry.tool_result_event_id) != Some(&candidate.session_id)
        {
            bail!(
                "provider continuation tool closure call or result belongs to a different session"
            )
        }
        if result_sequence <= call_sequence || result_sequence <= candidate.stream_sequence {
            bail!("provider continuation tool result does not close a later candidate call")
        }
        if event.stream_sequence <= result_sequence {
            bail!("provider continuation tool closure must follow its tool result")
        }
        if self.tool_result_events.get(&entry.tool_result_event_id)
            != Some(&entry.tool_call.tool_call_id)
        {
            bail!("provider continuation tool closure result does not match its tool call")
        }
        if event.causation_id.as_deref() != Some(entry.tool_result_event_id.as_str()) {
            bail!("provider continuation tool closure causation must be its tool result")
        }
        if entry.closed_at_unix_ms < candidate.entry.created_at_unix_ms
            || entry.closed_at_unix_ms > *lease_expires_at_unix_ms
        {
            bail!("provider continuation tool closure lies outside its candidate lease")
        }
        let key = (
            entry.candidate_id.clone(),
            entry.tool_call.tool_call_event_id.clone(),
        );
        if self.tool_closures.contains_key(&key) {
            bail!("provider continuation tool call was closed more than once")
        }
        self.tool_closures.insert(
            key,
            ProviderContinuationToolClosureState {
                event_id: event.event_id.clone(),
                stream_sequence: event.stream_sequence,
                entry,
            },
        );
        Ok(())
    }

    fn apply_payload_lifecycle(
        &mut self,
        event: &StoredEvent,
        entry: ProviderContinuationPayloadLifecycleEntry,
    ) -> Result<()> {
        entry.validate_shape()?;
        if event.event_id
            != provider_continuation_payload_lifecycle_event_id(&entry.payload_id, entry.state)
        {
            bail!("provider continuation payload lifecycle event id does not match its payload")
        }
        self.validate_payload_source(event, &entry)?;
        match entry.state {
            ProviderContinuationPayloadLifecycleState::Committed => {
                if entry.payload_id
                    != provider_continuation_payload_id(&entry.candidate_id, entry.kind)
                {
                    bail!("provider continuation payload manifest id does not match its candidate")
                }
                if self.payloads.contains_key(&entry.payload_id) {
                    bail!(
                        "provider continuation payload {} was committed more than once",
                        entry.payload_id
                    )
                }
                self.payloads.insert(
                    entry.payload_id.clone(),
                    ProviderContinuationPayloadState {
                        committed_event_id: event.event_id.clone(),
                        manifest: entry.clone(),
                        latest_event_id: event.event_id.clone(),
                        latest_lifecycle: entry,
                        candidate_event_id: None,
                    },
                );
            }
            ProviderContinuationPayloadLifecycleState::Invalidated
            | ProviderContinuationPayloadLifecycleState::OrphanDiscovered
            | ProviderContinuationPayloadLifecycleState::Deleted => {
                let payload = self.payloads.get_mut(&entry.payload_id).with_context(|| {
                    format!(
                        "provider continuation payload lifecycle references unknown payload {}",
                        entry.payload_id
                    )
                })?;
                validate_payload_lifecycle_matches_manifest(&entry, payload)?;
                if entry.state == ProviderContinuationPayloadLifecycleState::Invalidated
                    && payload.candidate_event_id.is_some()
                {
                    let invalidation = self
                        .candidate_invalidations
                        .get(&entry.candidate_id)
                        .context(
                            "candidate-backed provider continuation payload requires a source-valid invalidation",
                        )?;
                    if invalidation.stream_sequence >= event.stream_sequence {
                        bail!(
                            "provider continuation payload invalidation precedes its candidate terminal"
                        )
                    }
                    if event.causation_id.as_deref() != Some(invalidation.event_id.as_str()) {
                        bail!(
                            "provider continuation payload invalidation causation does not match its candidate terminal"
                        )
                    }
                }
                match (payload.latest_lifecycle.state, entry.state) {
                    (
                        ProviderContinuationPayloadLifecycleState::Committed,
                        ProviderContinuationPayloadLifecycleState::Invalidated,
                    ) => {}
                    (
                        ProviderContinuationPayloadLifecycleState::Committed,
                        ProviderContinuationPayloadLifecycleState::OrphanDiscovered,
                    ) if payload.candidate_event_id.is_none() => {}
                    (
                        ProviderContinuationPayloadLifecycleState::Invalidated
                        | ProviderContinuationPayloadLifecycleState::OrphanDiscovered,
                        ProviderContinuationPayloadLifecycleState::Deleted,
                    ) => {}
                    _ => bail!("provider continuation payload lifecycle transition is invalid"),
                }
                payload.latest_event_id = event.event_id.clone();
                payload.latest_lifecycle = entry;
            }
        }
        Ok(())
    }

    fn apply_resolution_plan(
        &mut self,
        event: &StoredEvent,
        entry: ProviderObservedResolutionPlanRecordedEntry,
    ) -> Result<()> {
        entry.validate_shape()?;
        if event.event_id
            != provider_observed_resolution_plan_recorded_event_id(&entry.resolution_plan_id)
        {
            bail!("provider observed resolution plan event id does not match its plan")
        }
        if entry.resolution_plan_id
            != provider_observed_resolution_plan_id(&entry.observation_id, &entry.candidate_id)
        {
            bail!("provider observed resolution plan id does not match its source")
        }
        let candidate = self.candidates.get(&entry.candidate_id).with_context(|| {
            format!(
                "provider observed resolution plan references unknown candidate {}",
                entry.candidate_id
            )
        })?;
        if self
            .candidate_invalidations
            .contains_key(&entry.candidate_id)
        {
            bail!("provider observed resolution plan cannot follow candidate invalidation")
        }
        let candidate_observation_id = candidate.entry.observation_id.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "provider observed resolution plan cannot bind an initiated continuation candidate"
            )
        })?;
        if entry.observation_id != candidate_observation_id {
            bail!("provider observed resolution plan observation does not match its candidate")
        }
        if entry.source_event_id != candidate.event_id {
            bail!("provider observed resolution plan source does not match its candidate event")
        }
        if entry.resolution_mode != candidate.entry.resolution_mode {
            bail!("provider observed resolution plan mode does not match its candidate")
        }
        let observation = self.observations.get(candidate_observation_id).with_context(|| {
            format!(
                "provider observed resolution plan references unknown observation {candidate_observation_id}"
            )
        })?;
        if event.session_id != observation.session_id {
            bail!("provider observed resolution plan belongs to a different session")
        }
        if event.correlation_id.as_deref() != Some(observation.correlation_id.as_str()) {
            bail!("provider observed resolution plan correlation does not match its observation")
        }
        let expected_causation_id = self.resolution_plan_causation(candidate)?;
        if event.causation_id.as_deref() != Some(expected_causation_id.as_str()) {
            bail!("provider observed resolution plan causation does not match its activation gate")
        }
        if event.stream_sequence <= candidate.stream_sequence {
            bail!("provider observed resolution plan does not follow its candidate")
        }
        let payload_id = &candidate.entry.candidate.payload().payload_id;
        let payload = self.payloads.get(payload_id).with_context(|| {
            format!(
                "provider observed resolution plan candidate {} has no payload manifest",
                entry.candidate_id
            )
        })?;
        if payload.latest_lifecycle.state != ProviderContinuationPayloadLifecycleState::Committed
            || payload.candidate_event_id.as_deref() != Some(candidate.event_id.as_str())
        {
            bail!("provider observed resolution plan candidate payload is not committed")
        }
        validate_resolution_plan_candidate_binding(
            &entry,
            candidate,
            observation,
            &self.event_sequences,
        )?;
        if self
            .resolution_plan_candidates
            .contains_key(&entry.candidate_id)
        {
            bail!("provider observed continuation candidate already has a resolution plan")
        }
        if self
            .resolution_plans
            .contains_key(&entry.resolution_plan_id)
        {
            bail!("provider observed resolution plan was recorded more than once")
        }
        self.resolution_plan_candidates
            .insert(entry.candidate_id.clone(), entry.resolution_plan_id.clone());
        self.resolution_plans.insert(
            entry.resolution_plan_id.clone(),
            ProviderObservedResolutionPlanState {
                event_id: event.event_id.clone(),
                stream_sequence: event.stream_sequence,
                session_id: event.session_id.clone(),
                entry,
            },
        );
        Ok(())
    }

    fn resolution_plan_causation(
        &self,
        candidate: &ProviderContinuationCandidateState,
    ) -> Result<EventId> {
        match &candidate.entry.activation_gate {
            ProviderContinuationActivationGate::Immediate => Ok(candidate.event_id.clone()),
            ProviderContinuationActivationGate::AwaitingToolClosure { tool_calls, .. } => {
                tool_calls
                    .iter()
                    .map(|tool_call| {
                        self.tool_closures
                            .get(&(
                                candidate.entry.candidate_id.clone(),
                                tool_call.tool_call_event_id.clone(),
                            ))
                            .with_context(|| {
                                format!(
                                    "provider observed resolution plan is missing closure for tool call {}",
                                    tool_call.tool_call_id
                                )
                            })
                    })
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .max_by_key(|closure| closure.stream_sequence)
                    .map(|closure| closure.event_id.clone())
                    .context("provider observed resolution plan has no completed tool closures")
            }
        }
    }

    fn validate_payload_source(
        &self,
        event: &StoredEvent,
        entry: &ProviderContinuationPayloadLifecycleEntry,
    ) -> Result<()> {
        match &entry.source {
            ProviderContinuationPayloadSource::Initiated {
                started_event_id,
                attempt_id,
            } => {
                let (session_id, expected_attempt_id) = self
                    .initiated_sources
                    .get(started_event_id)
                    .with_context(|| {
                        format!(
                            "provider continuation payload initiated source {started_event_id} is missing"
                        )
                    })?;
                if event.session_id != *session_id || attempt_id != expected_attempt_id {
                    bail!("provider continuation payload initiated source binding drifted")
                }
                let expected_candidate_id = provider_continuation_candidate_id_from_initiated(
                    session_id,
                    started_event_id,
                    attempt_id,
                );
                if entry.candidate_id != expected_candidate_id {
                    bail!(
                        "provider continuation payload candidate does not match its initiated source"
                    )
                }
            }
            ProviderContinuationPayloadSource::ProviderObserved {
                observation_event_id,
                observation_id,
            } => {
                let observation = self.observations.get(observation_id).with_context(|| {
                    format!(
                        "provider continuation payload references unknown observation {observation_id}"
                    )
                })?;
                if observation_event_id != &observation.event_id
                    || event.session_id != observation.session_id
                {
                    bail!("provider continuation payload observation source binding drifted")
                }
                let expected_candidate_id =
                    provider_continuation_candidate_id_from_observation(observation_id);
                if entry.candidate_id != expected_candidate_id {
                    bail!(
                        "provider continuation payload candidate does not match its observation source"
                    )
                }
            }
        }
        Ok(())
    }
}

impl JsonlSessionStore {
    /// Reads the native-continuation contract without writing recovery, retention, or cleanup
    /// records. A recorded candidate remains inactive until K25.12C resolves it.
    ///
    /// # Errors
    ///
    /// Returns an error when the V2 stream or the provider-continuation provenance is invalid.
    pub fn provider_continuation_projection(&self) -> Result<ProviderContinuationProjection> {
        let records = Self::read_event_records(self.path())?;
        ProviderContinuationProjection::from_records(&records)
    }
}

/// Derives an observation id from the durable native-attempt scope and canonical payload tag.
#[must_use]
pub fn provider_continuation_observation_id(
    session_id: &str,
    provider_route_fingerprint: &str,
    physical_attempt_id: &str,
    response_item_ordinal: u32,
    observed_payload_integrity_tag: &str,
) -> ProviderContinuationObservationId {
    stable_event_uuid(
        "sigil-provider-continuation-observation-v1",
        &format!(
            "{session_id}:{provider_route_fingerprint}:{physical_attempt_id}:{response_item_ordinal}:{observed_payload_integrity_tag}"
        ),
    )
}

/// Derives the event id for a provider-observed continuation record.
#[must_use]
pub fn provider_continuation_observed_event_id(observation_id: &str) -> EventId {
    stable_event_uuid(
        "sigil-provider-continuation-observed-event-v1",
        observation_id,
    )
}

/// Derives the sole candidate id permitted for one provider observation.
#[must_use]
pub fn provider_continuation_candidate_id_from_observation(
    observation_id: &str,
) -> ProviderContinuationCandidateId {
    stable_event_uuid("sigil-provider-observed-candidate-v1", observation_id)
}

/// Derives the sole provider-observed resolution plan identity for an observation/candidate pair.
///
/// Initiated candidates intentionally have no corresponding plan identity: their real
/// `CompactionStarted` record remains their only frozen-plan source.
#[must_use]
pub fn provider_observed_resolution_plan_id(
    observation_id: &str,
    candidate_id: &str,
) -> ProviderObservedResolutionPlanId {
    stable_event_uuid(
        "sigil-provider-observed-resolution-plan-v1",
        &format!("{observation_id}:{candidate_id}"),
    )
}

/// Derives the deterministic direct-event identity for one provider-observed resolution plan.
#[must_use]
pub fn provider_observed_resolution_plan_recorded_event_id(resolution_plan_id: &str) -> EventId {
    stable_event_uuid(
        "sigil-provider-observed-resolution-plan-recorded-event-v1",
        resolution_plan_id,
    )
}

/// Derives the sole candidate id permitted for one initiated compaction attempt.
#[must_use]
pub fn provider_continuation_candidate_id_from_initiated(
    session_id: &str,
    started_event_id: &str,
    attempt_id: &str,
) -> ProviderContinuationCandidateId {
    stable_event_uuid(
        "sigil-provider-initiated-candidate-v1",
        &format!("{session_id}:{started_event_id}:{attempt_id}"),
    )
}

/// Derives the sole payload identity for one candidate representation.
#[must_use]
pub fn provider_continuation_payload_id(
    candidate_id: &str,
    kind: ProviderContinuationPayloadKind,
) -> ProviderContinuationPayloadId {
    stable_event_uuid(
        "sigil-provider-continuation-payload-v1",
        &format!("{candidate_id}:{}", kind.as_str()),
    )
}

/// Derives the deterministic candidate-record event id used for replay-safe appends.
#[must_use]
pub fn provider_continuation_candidate_recorded_event_id(candidate_id: &str) -> EventId {
    stable_event_uuid(
        "sigil-provider-continuation-candidate-recorded-event-v1",
        candidate_id,
    )
}

/// Derives the sole invalidation-terminal event id for one provider-observed candidate.
#[must_use]
pub fn provider_continuation_candidate_invalidated_event_id(candidate_id: &str) -> EventId {
    stable_event_uuid(
        "sigil-provider-continuation-candidate-invalidated-event-v1",
        candidate_id,
    )
}

/// Derives the sole closure-event identity permitted for one candidate tool-call reference.
#[must_use]
pub fn provider_continuation_tool_closure_recorded_event_id(
    candidate_id: &str,
    tool_call_event_id: &str,
) -> EventId {
    stable_event_uuid(
        "sigil-provider-continuation-tool-closure-recorded-event-v1",
        &format!("{candidate_id}:{tool_call_event_id}"),
    )
}

/// Derives the single lifecycle event identity for one payload state transition.
///
/// The lifecycle is append-only and each state can occur at most once for a payload, so this
/// identity makes replay and duplicate append detection deterministic without encoding any
/// provider-native payload bytes in the event stream.
#[must_use]
pub fn provider_continuation_payload_lifecycle_event_id(
    payload_id: &str,
    state: ProviderContinuationPayloadLifecycleState,
) -> EventId {
    stable_event_uuid(
        "sigil-provider-continuation-payload-lifecycle-event-v1",
        &format!("{payload_id}:{}", state.as_str()),
    )
}

fn validate_candidate_payload_manifest(
    candidate: &ProviderContinuationCandidateRecordedEntry,
    payload: &ProviderContinuationPayloadState,
) -> Result<()> {
    let reference = candidate.candidate.payload();
    if payload.latest_lifecycle.state != ProviderContinuationPayloadLifecycleState::Committed {
        bail!("provider continuation candidate payload is no longer committed")
    }
    if payload.candidate_event_id.is_some() {
        bail!("provider continuation payload already has a recorded candidate")
    }
    if payload.manifest.candidate_id != candidate.candidate_id
        || payload.manifest.payload_id != reference.payload_id
        || payload.manifest.kind != candidate.candidate.payload_kind()
        || payload.manifest.integrity != reference.integrity
        || payload.manifest.byte_size != reference.byte_size
    {
        bail!("provider continuation candidate does not match its payload manifest")
    }
    payload
        .manifest
        .storage_ref
        .validate_for_candidate(&candidate.candidate)
}

fn validate_resolution_plan_candidate_binding(
    plan: &ProviderObservedResolutionPlanRecordedEntry,
    candidate: &ProviderContinuationCandidateState,
    observation: &ProviderContinuationObservationState,
    event_sequences: &BTreeMap<EventId, u64>,
) -> Result<()> {
    candidate
        .entry
        .candidate
        .validate_observation_binding(&observation.entry)?;
    candidate
        .entry
        .candidate
        .validate_resolution_target_identity(&plan.execution_identity)?;
    if plan.lineage.folded_through.session_id != observation.session_id
        || plan.lineage.folded_through != *candidate.entry.candidate.covers_through()
    {
        bail!("provider observed resolution plan cursor does not match its candidate")
    }
    if plan.before_input.material_fingerprint() != candidate.entry.candidate.request_fingerprint() {
        bail!("provider observed resolution plan before evidence does not match its candidate")
    }
    for event_id in plan
        .lineage
        .retained_event_ids
        .iter()
        .chain(&plan.lineage.protected_event_ids)
    {
        let sequence = event_sequences.get(event_id).with_context(|| {
            format!("provider observed resolution plan references unknown event {event_id}")
        })?;
        if *sequence >= candidate.stream_sequence {
            bail!("provider observed resolution plan references an event after its candidate")
        }
    }
    let cursor_sequence = event_sequences
        .get(&plan.lineage.folded_through.through_event_id)
        .with_context(|| {
            format!(
                "provider observed resolution plan cursor references unknown event {}",
                plan.lineage.folded_through.through_event_id
            )
        })?;
    if *cursor_sequence != plan.lineage.folded_through.through_stream_sequence
        || *cursor_sequence >= candidate.stream_sequence
    {
        bail!("provider observed resolution plan cursor is not a prior durable event")
    }
    Ok(())
}

fn validate_resolution_refs(field: &str, refs: &[EventId], max_refs: usize) -> Result<()> {
    if refs.len() > max_refs {
        bail!("{field} exceed the maximum count")
    }
    for event_id in refs {
        validate_identity(field, event_id, 512)?;
    }
    Ok(())
}

fn validate_payload_lifecycle_matches_manifest(
    entry: &ProviderContinuationPayloadLifecycleEntry,
    payload: &ProviderContinuationPayloadState,
) -> Result<()> {
    let mut normalized = entry.clone();
    normalized.state = payload.manifest.state;
    normalized.reason = payload.manifest.reason.clone();
    if normalized != payload.manifest {
        bail!("provider continuation payload lifecycle does not match its committed manifest")
    }
    Ok(())
}

fn validate_candidate_binding(
    candidate_id: &str,
    provider_name: &str,
    provider_route_fingerprint: &str,
    model_name: &str,
    model_metadata_profile: &VersionedProfileIdentity,
    wire_profile: &VersionedProfileIdentity,
    wire_protocol: &str,
    wire_schema_version: &str,
    composition_profile: &VersionedProfileIdentity,
    request_fingerprint: &str,
) -> Result<()> {
    validate_identity("provider continuation candidate id", candidate_id, 512)?;
    validate_provider_profiles(
        provider_name,
        provider_route_fingerprint,
        model_name,
        model_metadata_profile,
        wire_profile,
        wire_protocol,
        wire_schema_version,
    )?;
    composition_profile.validate()?;
    validate_digest(
        "provider continuation request fingerprint",
        request_fingerprint,
        "hmac-sha256:",
    )
}

fn validate_provider_profiles(
    provider_name: &str,
    provider_route_fingerprint: &str,
    model_name: &str,
    model_metadata_profile: &VersionedProfileIdentity,
    wire_profile: &VersionedProfileIdentity,
    wire_protocol: &str,
    wire_schema_version: &str,
) -> Result<()> {
    validate_label("provider continuation provider name", provider_name)?;
    validate_digest(
        "provider continuation provider route fingerprint",
        provider_route_fingerprint,
        "hmac-sha256:",
    )?;
    validate_label("provider continuation model name", model_name)?;
    model_metadata_profile.validate()?;
    wire_profile.validate()?;
    validate_label("provider continuation wire protocol", wire_protocol)?;
    validate_label(
        "provider continuation wire schema version",
        wire_schema_version,
    )
}

fn validate_cursor_shape(cursor: &CompactionCursor) -> Result<()> {
    validate_identity(
        "provider continuation cursor session id",
        &cursor.session_id,
        512,
    )?;
    if cursor.through_stream_sequence == 0 {
        bail!("provider continuation cursor stream sequence must be non-zero")
    }
    validate_identity(
        "provider continuation cursor event id",
        &cursor.through_event_id,
        512,
    )
}

fn provider_continuation_integrity_tag(
    session_scope_id: &str,
    purpose: &str,
    fields: &[&[u8]],
) -> Result<String> {
    validate_identity(
        "provider continuation integrity session scope",
        session_scope_id,
        512,
    )?;
    let mut mac = Hmac::<Sha256>::new_from_slice(provider_continuation_integrity_key())
        .context("failed to initialize provider continuation integrity tag")?;
    update_integrity_field(
        &mut mac,
        "schema_version",
        PROVIDER_CONTINUATION_SCHEMA_VERSION.to_string().as_bytes(),
    );
    update_integrity_field(&mut mac, "session_scope_id", session_scope_id.as_bytes());
    update_integrity_field(&mut mac, "purpose", purpose.as_bytes());
    for field in fields {
        update_integrity_field(&mut mac, "field", field);
    }
    Ok(format!("hmac-sha256:{:x}", mac.finalize().into_bytes()))
}

fn provider_continuation_integrity_key() -> &'static [u8; 32] {
    PROVIDER_CONTINUATION_INTEGRITY_KEY.get_or_init(|| {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let mut key = [0_u8; 32];
        key[..16].copy_from_slice(first.as_bytes());
        key[16..].copy_from_slice(second.as_bytes());
        key
    })
}

fn update_integrity_field(mac: &mut Hmac<Sha256>, label: &str, value: &[u8]) {
    mac.update(&(label.len() as u64).to_be_bytes());
    mac.update(label.as_bytes());
    mac.update(&(value.len() as u64).to_be_bytes());
    mac.update(value);
}

fn validate_label(field: &str, value: &str) -> Result<()> {
    validate_identity(field, value, 256)
}

fn validate_identity(field: &str, value: &str, max_bytes: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        bail!("{field} must be non-empty, bounded, and control-free")
    }
    Ok(())
}

fn validate_digest(field: &str, value: &str, prefix: &str) -> Result<()> {
    let Some(hex) = value.strip_prefix(prefix) else {
        bail!("{field} must use the {prefix} format")
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{field} must contain a sha256 digest")
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/provider_continuation_tests.rs"]
mod tests;
