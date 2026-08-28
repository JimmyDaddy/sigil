//! RFC-0071 section 9.5 / R71.4: session export event envelope and migration contract.
//!
//! Default/portable exports become sealed ArtifactStaging/Store publishes; explicit external
//! destinations become create-new/no-overwrite file admissions that never truncate user
//! content. The workspace-state session-exports allocator and direct writer are superseded.

use serde::{Deserialize, Serialize};

use crate::recovery::EffectSettlementV1;
use crate::resource::{
    CanonicalHash, OpaqueArtifactId, OpaqueDomainEventId, OpaquePermissionSubjectRef,
    OpaqueSessionExportId,
};

/// Closed export outcome classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionExportOutcomeV1 {
    Artifact {
        artifact_id: OpaqueArtifactId,
        object_key_hash: CanonicalHash,
    },
    ExternalFile {
        destination_binding_hash: CanonicalHash,
        content_digest: CanonicalHash,
    },
    OutcomeUncertain {
        evidence_digest: CanonicalHash,
    },
}

/// Closed export phase classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionExportPhaseV1 {
    Planned,
    ArtifactPrepared,
    ArtifactPublished,
    ExternalRegistered,
    ExternalCreated,
    Committed,
    RecoveryStarted,
    RecoverySubjectBound,
    RecoverySettled,
    Superseded,
}

/// Full event envelope: closed phase state machine, exact frontier and receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionExportEventEnvelopeV1 {
    pub event_id: OpaqueDomainEventId,
    pub export_id: OpaqueSessionExportId,
    pub phase: SessionExportPhaseV1,
    pub session_source_frontier_hash: CanonicalHash,
    pub destination_subject_ref: Option<OpaquePermissionSubjectRef>,
    pub destination_binding_hash: Option<CanonicalHash>,
    pub content_digest: CanonicalHash,
    pub byte_length: u64,
    pub outcome: Option<SessionExportOutcomeV1>,
    pub effect_settlement: EffectSettlementV1,
    pub event_hash: CanonicalHash,
}

/// Closed export error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionExportErrorV1 {
    #[error("export already committed; duplicate publish is rejected")]
    DuplicateCommit,
    #[error("external destination already exists; create-new no-overwrite refused")]
    DestinationExists,
    #[error("external destination identity drifted between registration and create")]
    DestinationDrift,
    #[error("unknown external create requires the user to reselect the same file before reconcile")]
    ReselectRequired,
    #[error("export is outcome-uncertain; no blind replay")]
    OutcomeUncertain,
}

/// Validates the closed export phase ladder (monotonic, no blind jump).
pub fn validate_export_phase_ladder(
    current: SessionExportPhaseV1,
    next: SessionExportPhaseV1,
) -> Result<(), SessionExportErrorV1> {
    use SessionExportPhaseV1::*;
    let allowed = matches!(
        (current, next),
        (Planned, ArtifactPrepared)
            | (ArtifactPrepared, ArtifactPublished)
            | (ArtifactPublished, Committed)
            | (Planned, ExternalRegistered)
            | (ExternalRegistered, ExternalCreated)
            | (ExternalCreated, Committed)
            | (Committed, RecoveryStarted)
            | (RecoveryStarted, RecoverySubjectBound)
            | (RecoverySubjectBound, RecoverySettled)
            | (Committed, Superseded)
    );
    if !allowed {
        // Any jump to the next phase must be a legal single step; a committed export can only
        // be reconciled or superseded.
        return Err(SessionExportErrorV1::OutcomeUncertain);
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/session_export_tests.rs"]
mod tests;
