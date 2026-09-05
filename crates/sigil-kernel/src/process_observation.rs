//! RFC-0071 section 1.1: host-process identity observation contract.
//!
//! `sigil-process` owns raw platform birth facts. The observer adapter may register a current
//! authority owner or a sandbox-produced child, but it never decides resource settlement. The
//! Resource Authority is the only durable consumer of a verified identity and the only consumer
//! of the recovery probe. A PID-shaped string is deliberately not an observation input.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::resource::CanonicalHash;

/// A process fact that has been checked against an authenticated durable subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessVitalityV1 {
    Live,
    Quiescent,
}

/// The minimum process-cleanup evidence a resource requires before it can settle.
///
/// This is frozen before spawn. A later observation must never downgrade a contained-tree
/// requirement merely because a local backend can only perform bounded native cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessCoverageRequirementV1 {
    BoundedNativeAllowed,
    ContainedTreeRequired,
}

/// The cleanup coverage actually provided by the physical backend for one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessCoverageEffectiveV1 {
    /// The direct child was reaped and the configured process group plus registered members were
    /// checked or cleaned under their exact identities. Unknown historical escapees remain
    /// outside this receipt.
    BoundedNative,
    /// A backend proved that its closed, non-escapable member set has no live process.
    ContainedTree,
}

impl ProcessCoverageEffectiveV1 {
    /// Returns whether this physical coverage can satisfy a frozen resource requirement.
    #[must_use]
    pub const fn satisfies(self, requirement: ProcessCoverageRequirementV1) -> bool {
        matches!(
            (requirement, self),
            (
                ProcessCoverageRequirementV1::BoundedNativeAllowed,
                ProcessCoverageEffectiveV1::BoundedNative
                    | ProcessCoverageEffectiveV1::ContainedTree
            ) | (
                ProcessCoverageRequirementV1::ContainedTreeRequired,
                ProcessCoverageEffectiveV1::ContainedTree
            )
        )
    }
}

/// Physical coverage evidence attached by an observer, rather than declared by a caller.
///
/// A recovery request may state the coverage it needs, but it cannot turn that statement into a
/// stronger receipt. `Leader` is the only evidence the generic host observer can produce: it has
/// re-observed one exact process identity, not a closed set. Adapters that can actually observe a
/// non-escapable backend boundary must bind every observed member (or the complete old-epoch
/// inventory) to the matching opaque object and frontier before they construct either strong
/// variant.
///
/// This type is intentionally not serializable. The enclosing facet and verified receipt remain
/// one-shot, non-clonable values whose issuing observer retains the matching private record. A
/// caller-constructed request can therefore never manufacture strong coverage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessRecoveryCoverageEvidenceV1 {
    /// Only the directly observed subject is covered.
    Leader {
        leader_registration_hash: CanonicalHash,
    },
    /// The observer saw the entire closed member set for one exact containment binding.
    ObservedClosedMemberSet {
        containment_binding: CanonicalHash,
        observed_member_set_hash: CanonicalHash,
        authority_frontier: CanonicalHash,
    },
    /// The observer saw the entire old-epoch member set for one exact inventory snapshot.
    ObservedOldEpoch {
        inventory_snapshot_hash: CanonicalHash,
        observed_member_set_hash: CanonicalHash,
        authority_frontier: CanonicalHash,
    },
}

impl ProcessRecoveryCoverageEvidenceV1 {
    /// Constructs the bounded evidence obtained when an observer re-observes one registered
    /// process subject.
    #[must_use]
    pub const fn leader(leader_registration_hash: CanonicalHash) -> Self {
        Self::Leader {
            leader_registration_hash,
        }
    }

    /// Returns the coverage actually established by this evidence.
    #[must_use]
    pub const fn effective_coverage(&self) -> ProcessCoverageEffectiveV1 {
        match self {
            Self::Leader { .. } => ProcessCoverageEffectiveV1::BoundedNative,
            Self::ObservedClosedMemberSet { .. } | Self::ObservedOldEpoch { .. } => {
                ProcessCoverageEffectiveV1::ContainedTree
            }
        }
    }

    /// Returns whether the evidence is structurally complete before a verifier consumes it.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        match self {
            Self::Leader {
                leader_registration_hash,
            } => canonical_hash_is_nonzero(*leader_registration_hash),
            Self::ObservedClosedMemberSet {
                containment_binding,
                observed_member_set_hash,
                authority_frontier,
            } => {
                canonical_hash_is_nonzero(*containment_binding)
                    && canonical_hash_is_nonzero(*observed_member_set_hash)
                    && canonical_hash_is_nonzero(*authority_frontier)
            }
            Self::ObservedOldEpoch {
                inventory_snapshot_hash,
                observed_member_set_hash,
                authority_frontier,
            } => {
                canonical_hash_is_nonzero(*inventory_snapshot_hash)
                    && canonical_hash_is_nonzero(*observed_member_set_hash)
                    && canonical_hash_is_nonzero(*authority_frontier)
            }
        }
    }

    /// Returns whether this observer-issued evidence, rather than the caller's request field,
    /// proves the exact requested coverage and physical object.
    #[must_use]
    pub fn proves(&self, request: &ProcessRecoveryFacetRequestV1) -> bool {
        if !self.is_well_formed() || !request.is_well_formed() {
            return false;
        }

        match (self, &request.physical_object) {
            (
                Self::Leader { .. },
                ProcessRecoveryPhysicalObjectV1::Owner
                | ProcessRecoveryPhysicalObjectV1::DirectChildLeader,
            ) => {
                request.effective_coverage == ProcessCoverageEffectiveV1::BoundedNative
                    && request.required_coverage
                        == ProcessCoverageRequirementV1::BoundedNativeAllowed
            }
            (
                Self::Leader {
                    leader_registration_hash,
                },
                ProcessRecoveryPhysicalObjectV1::RegisteredMember { member_binding },
            ) => {
                *leader_registration_hash == *member_binding
                    && request.effective_coverage == ProcessCoverageEffectiveV1::BoundedNative
                    && request.required_coverage
                        == ProcessCoverageRequirementV1::BoundedNativeAllowed
            }
            (
                Self::ObservedClosedMemberSet {
                    containment_binding,
                    authority_frontier,
                    ..
                },
                ProcessRecoveryPhysicalObjectV1::ContainedTree {
                    containment_binding: requested_binding,
                },
            ) => {
                *containment_binding == *requested_binding
                    && *authority_frontier == request.authority_frontier
                    && request.effective_coverage == ProcessCoverageEffectiveV1::ContainedTree
            }
            (
                Self::ObservedOldEpoch {
                    inventory_snapshot_hash,
                    authority_frontier,
                    ..
                },
                ProcessRecoveryPhysicalObjectV1::OldEpoch {
                    inventory_snapshot_hash: requested_snapshot,
                },
            ) => {
                *inventory_snapshot_hash == *requested_snapshot
                    && *authority_frontier == request.authority_frontier
                    && request.effective_coverage == ProcessCoverageEffectiveV1::ContainedTree
            }
            _ => false,
        }
    }
}

fn canonical_hash_is_nonzero(hash: CanonicalHash) -> bool {
    hash.as_bytes().iter().any(|byte| *byte != 0)
}

/// The exact question an authority recovery consumer is asking of a physical observer.
///
/// These variants deliberately distinguish an owner from a direct-child leader, a registered
/// escaped member, a closed contained tree, and an entire old authority epoch. In particular,
/// observing an owner or a leader terminal cannot answer a contained-tree or old-epoch question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessRecoveryPurposeV1 {
    OwnerClaimRecovery,
    DirectChildSettlement,
    RegisteredMemberSettlement,
    ContainedTreeQuiescence,
    OldEpochQuiescence,
}

/// Opaque physical object binding supplied by the authenticated inventory or sandbox ledger.
///
/// No raw PID, PGID, path, Job name, or cgroup locator crosses this kernel contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessRecoveryPhysicalObjectV1 {
    Owner,
    DirectChildLeader,
    RegisteredMember {
        member_binding: CanonicalHash,
    },
    ContainedTree {
        containment_binding: CanonicalHash,
    },
    OldEpoch {
        inventory_snapshot_hash: CanonicalHash,
    },
}

/// Frozen recovery-facet binding that Resource Authority supplies with an authenticated subject.
///
/// `authority_frontier` is the exact durable inventory frontier the caller will revalidate before
/// consuming the one-shot result. Its all-zero value is rejected so a missing frontier cannot
/// silently become a wildcard. `effective_coverage` is the coverage requested from the physical
/// observer, not evidence that the caller has already obtained it; a verified facet exposes the
/// separate observer-issued [`ProcessRecoveryCoverageEvidenceV1`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessRecoveryFacetRequestV1 {
    pub purpose: ProcessRecoveryPurposeV1,
    pub physical_object: ProcessRecoveryPhysicalObjectV1,
    pub required_coverage: ProcessCoverageRequirementV1,
    pub effective_coverage: ProcessCoverageEffectiveV1,
    pub authority_frontier: CanonicalHash,
}

impl ProcessRecoveryFacetRequestV1 {
    /// Returns whether the purpose, physical object, coverage, and frontier form one exact
    /// fail-closed recovery question.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        let object_matches_purpose = matches!(
            (&self.purpose, &self.physical_object),
            (
                ProcessRecoveryPurposeV1::OwnerClaimRecovery,
                ProcessRecoveryPhysicalObjectV1::Owner
            ) | (
                ProcessRecoveryPurposeV1::DirectChildSettlement,
                ProcessRecoveryPhysicalObjectV1::DirectChildLeader
            ) | (
                ProcessRecoveryPurposeV1::RegisteredMemberSettlement,
                ProcessRecoveryPhysicalObjectV1::RegisteredMember { .. }
            ) | (
                ProcessRecoveryPurposeV1::ContainedTreeQuiescence,
                ProcessRecoveryPhysicalObjectV1::ContainedTree { .. }
            ) | (
                ProcessRecoveryPurposeV1::OldEpochQuiescence,
                ProcessRecoveryPhysicalObjectV1::OldEpoch { .. }
            )
        );
        let strong_purpose = matches!(
            self.purpose,
            ProcessRecoveryPurposeV1::ContainedTreeQuiescence
                | ProcessRecoveryPurposeV1::OldEpochQuiescence
        );
        let physical_object_binding_is_nonzero = match &self.physical_object {
            ProcessRecoveryPhysicalObjectV1::Owner
            | ProcessRecoveryPhysicalObjectV1::DirectChildLeader => true,
            ProcessRecoveryPhysicalObjectV1::RegisteredMember { member_binding } => {
                canonical_hash_is_nonzero(*member_binding)
            }
            ProcessRecoveryPhysicalObjectV1::ContainedTree {
                containment_binding,
            } => canonical_hash_is_nonzero(*containment_binding),
            ProcessRecoveryPhysicalObjectV1::OldEpoch {
                inventory_snapshot_hash,
            } => canonical_hash_is_nonzero(*inventory_snapshot_hash),
        };

        object_matches_purpose
            && physical_object_binding_is_nonzero
            && self.effective_coverage.satisfies(self.required_coverage)
            && (!strong_purpose
                || self.effective_coverage == ProcessCoverageEffectiveV1::ContainedTree)
            && self
                .authority_frontier
                .as_bytes()
                .iter()
                .any(|byte| *byte != 0)
    }
}

/// The only two host-process subjects admitted by the initial E02 production slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessObservationSubjectKindV1 {
    AuthorityBootstrapOwner,
    ManagedExecution,
}

/// Exact authority and execution context bound to one host-process registration.
///
/// `execution_scope_hash` is opaque to the observer. The Resource Authority and sandbox bind it
/// to the prepared physical attempt before a process can be spawned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessObservationScopeV1 {
    pub authority_epoch: u64,
    pub application_composition_epoch: u64,
    pub execution_scope_hash: CanonicalHash,
}

impl ProcessObservationScopeV1 {
    /// Returns whether the context can describe a live authority epoch.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.authority_epoch != 0 && self.application_composition_epoch != 0
    }
}

/// Durable host-process birth fact emitted after a same-factory registration is verified.
///
/// This is serializable inventory data, not a Rust-level capability: deserialization or the
/// public platform-adapter constructor can manufacture a value. Resource Authority therefore
/// never treats the DTO itself as authorization. Effect entrypoints accept only a current
/// verifier's one-shot issuance, and recovery re-observes this subject against OS birth facts.
/// The fact proves the process birth was observed live at registration time; it is not a claim
/// that the process remains live when the DTO is later consumed.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedHostProcessIdentityV1 {
    process_id: u32,
    birth_identity_hash: CanonicalHash,
    subject_kind: ProcessObservationSubjectKindV1,
    scope: ProcessObservationScopeV1,
    registration_nonce: String,
    registration_hash: CanonicalHash,
    registration_service_instance_hash: CanonicalHash,
    registration_service_generation: u64,
}

impl std::fmt::Debug for VerifiedHostProcessIdentityV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedHostProcessIdentityV1")
            .field("process_id", &self.process_id)
            .field("birth_identity_hash", &self.birth_identity_hash)
            .field("subject_kind", &self.subject_kind)
            .field("scope", &self.scope)
            .field("registration_hash", &self.registration_hash)
            .field(
                "registration_service_instance_hash",
                &self.registration_service_instance_hash,
            )
            .field(
                "registration_service_generation",
                &self.registration_service_generation,
            )
            .finish_non_exhaustive()
    }
}

impl VerifiedHostProcessIdentityV1 {
    /// Constructs durable process-fact data for a platform adapter.
    ///
    /// This constructor is not an authority issuance path. Resource Authority receives a
    /// subject only as the result of
    /// [`HostProcessObservationVerifierV1::verify_registration`], whose private issuance record
    /// it verifies before persisting the DTO.
    #[must_use]
    pub fn from_verified_registration(
        process_id: u32,
        birth_identity_hash: CanonicalHash,
        subject_kind: ProcessObservationSubjectKindV1,
        scope: ProcessObservationScopeV1,
        registration_nonce: String,
        registration_hash: CanonicalHash,
        registration_service_instance_hash: CanonicalHash,
        registration_service_generation: u64,
    ) -> Self {
        Self {
            process_id,
            birth_identity_hash,
            subject_kind,
            scope,
            registration_nonce,
            registration_hash,
            registration_service_instance_hash,
            registration_service_generation,
        }
    }

    #[must_use]
    pub const fn process_id(&self) -> u32 {
        self.process_id
    }

    #[must_use]
    pub const fn birth_identity_hash(&self) -> CanonicalHash {
        self.birth_identity_hash
    }

    #[must_use]
    pub const fn subject_kind(&self) -> ProcessObservationSubjectKindV1 {
        self.subject_kind
    }

    #[must_use]
    pub const fn scope(&self) -> &ProcessObservationScopeV1 {
        &self.scope
    }

    #[must_use]
    pub const fn registration_hash(&self) -> CanonicalHash {
        self.registration_hash
    }

    #[must_use]
    pub fn registration_nonce(&self) -> &str {
        &self.registration_nonce
    }

    #[must_use]
    pub const fn registration_service_instance_hash(&self) -> CanonicalHash {
        self.registration_service_instance_hash
    }

    #[must_use]
    pub const fn registration_service_generation(&self) -> u64 {
        self.registration_service_generation
    }

    #[must_use]
    pub fn has_exact_binding(
        &self,
        expected_kind: ProcessObservationSubjectKindV1,
        expected_scope: &ProcessObservationScopeV1,
    ) -> bool {
        self.subject_kind == expected_kind && self.scope == *expected_scope
    }
}

/// One-shot observer output. Its public representation is not trusted by Resource Authority;
/// only the matching verifier's private issuance record makes it acceptable for persistence.
#[derive(Debug)]
pub struct HostProcessIdentityRegistrationV1 {
    identity: VerifiedHostProcessIdentityV1,
    issuance_id: String,
    observed_at_monotonic_ms: u64,
}

impl HostProcessIdentityRegistrationV1 {
    /// Creates observer output that still requires same-factory verification.
    #[must_use]
    pub fn new(
        identity: VerifiedHostProcessIdentityV1,
        issuance_id: String,
        observed_at_monotonic_ms: u64,
    ) -> Self {
        Self {
            identity,
            issuance_id,
            observed_at_monotonic_ms,
        }
    }

    #[must_use]
    pub const fn identity(&self) -> &VerifiedHostProcessIdentityV1 {
        &self.identity
    }

    #[must_use]
    pub fn issuance_id(&self) -> &str {
        &self.issuance_id
    }

    #[must_use]
    pub const fn observed_at_monotonic_ms(&self) -> u64 {
        self.observed_at_monotonic_ms
    }
}

/// One-shot fresh recovery observation. The durable subject is supplied separately by Resource
/// Authority, so a missing PID can never become a proof without that subject and matching
/// verifier issuance.
#[derive(Debug)]
pub struct HostProcessRecoveryObservationV1 {
    subject_registration_hash: CanonicalHash,
    vitality: ProcessVitalityV1,
    issuance_id: String,
    observed_at_monotonic_ms: u64,
}

/// One-shot purpose- and frontier-bound recovery evidence for a physical process object.
///
/// Like the V1 observation, this carries no authority by itself: only the same-factory verifier
/// can consume the private issuance entry. The observer sets the expiry from its monotonic
/// service clock; callers cannot choose or extend it.
#[derive(Debug)]
pub struct HostProcessRecoveryFacetV1 {
    subject_registration_hash: CanonicalHash,
    request: ProcessRecoveryFacetRequestV1,
    coverage_evidence: ProcessRecoveryCoverageEvidenceV1,
    vitality: ProcessVitalityV1,
    issuance_id: String,
    observed_at_monotonic_ms: u64,
    expires_at_monotonic_ms: u64,
}

impl HostProcessRecoveryFacetV1 {
    #[must_use]
    pub fn new(
        subject_registration_hash: CanonicalHash,
        request: ProcessRecoveryFacetRequestV1,
        vitality: ProcessVitalityV1,
        issuance_id: String,
        observed_at_monotonic_ms: u64,
        expires_at_monotonic_ms: u64,
    ) -> Self {
        Self::new_with_coverage_evidence(
            subject_registration_hash,
            request,
            ProcessRecoveryCoverageEvidenceV1::leader(subject_registration_hash),
            vitality,
            issuance_id,
            observed_at_monotonic_ms,
            expires_at_monotonic_ms,
        )
    }

    /// Creates observer output with the exact physical coverage the observer established.
    ///
    /// The public DTO is still not a capability: only a same-factory verifier can match this
    /// value to its private issuance record. The compatibility constructor above deliberately
    /// produces leader evidence and therefore cannot be used to manufacture a strong receipt.
    #[must_use]
    pub fn new_with_coverage_evidence(
        subject_registration_hash: CanonicalHash,
        request: ProcessRecoveryFacetRequestV1,
        coverage_evidence: ProcessRecoveryCoverageEvidenceV1,
        vitality: ProcessVitalityV1,
        issuance_id: String,
        observed_at_monotonic_ms: u64,
        expires_at_monotonic_ms: u64,
    ) -> Self {
        Self {
            subject_registration_hash,
            request,
            coverage_evidence,
            vitality,
            issuance_id,
            observed_at_monotonic_ms,
            expires_at_monotonic_ms,
        }
    }

    #[must_use]
    pub const fn subject_registration_hash(&self) -> CanonicalHash {
        self.subject_registration_hash
    }

    #[must_use]
    pub const fn request(&self) -> &ProcessRecoveryFacetRequestV1 {
        &self.request
    }

    #[must_use]
    pub const fn coverage_evidence(&self) -> &ProcessRecoveryCoverageEvidenceV1 {
        &self.coverage_evidence
    }

    #[must_use]
    pub const fn vitality(&self) -> ProcessVitalityV1 {
        self.vitality
    }

    #[must_use]
    pub fn issuance_id(&self) -> &str {
        &self.issuance_id
    }

    #[must_use]
    pub const fn observed_at_monotonic_ms(&self) -> u64 {
        self.observed_at_monotonic_ms
    }

    #[must_use]
    pub const fn expires_at_monotonic_ms(&self) -> u64 {
        self.expires_at_monotonic_ms
    }
}

impl HostProcessRecoveryObservationV1 {
    /// Creates one observer-issued recovery observation pending same-factory verification.
    #[must_use]
    pub fn new(
        subject_registration_hash: CanonicalHash,
        vitality: ProcessVitalityV1,
        issuance_id: String,
        observed_at_monotonic_ms: u64,
    ) -> Self {
        Self {
            subject_registration_hash,
            vitality,
            issuance_id,
            observed_at_monotonic_ms,
        }
    }

    #[must_use]
    pub const fn subject_registration_hash(&self) -> CanonicalHash {
        self.subject_registration_hash
    }

    #[must_use]
    pub const fn vitality(&self) -> ProcessVitalityV1 {
        self.vitality
    }

    #[must_use]
    pub fn issuance_id(&self) -> &str {
        &self.issuance_id
    }

    #[must_use]
    pub const fn observed_at_monotonic_ms(&self) -> u64 {
        self.observed_at_monotonic_ms
    }
}

/// Verified fresh recovery result, issued only by the same-factory verifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedHostProcessRecoveryObservationV1 {
    pub vitality: ProcessVitalityV1,
    pub verifier_instance_hash: CanonicalHash,
    pub verifier_service_generation: u64,
    pub verified_observation_hash: CanonicalHash,
}

/// Same-factory verified recovery facet. It is intentionally not serializable or clonable.
///
/// Resource Authority may use it only together with its exact authenticated subject and durable
/// frontier. A `Quiescent` result for a bounded direct child is therefore never interchangeable
/// with a contained-tree or old-epoch proof.
#[derive(Debug, PartialEq, Eq)]
pub struct VerifiedHostProcessRecoveryFacetV1 {
    request: ProcessRecoveryFacetRequestV1,
    coverage_evidence: ProcessRecoveryCoverageEvidenceV1,
    vitality: ProcessVitalityV1,
    verifier_instance_hash: CanonicalHash,
    verifier_service_generation: u64,
    verified_observation_hash: CanonicalHash,
}

impl VerifiedHostProcessRecoveryFacetV1 {
    #[must_use]
    pub const fn request(&self) -> &ProcessRecoveryFacetRequestV1 {
        &self.request
    }

    /// Returns the observer-issued evidence that established this receipt's actual coverage.
    #[must_use]
    pub const fn coverage_evidence(&self) -> &ProcessRecoveryCoverageEvidenceV1 {
        &self.coverage_evidence
    }

    /// Returns the coverage established by the observer-issued evidence.
    #[must_use]
    pub const fn effective_coverage(&self) -> ProcessCoverageEffectiveV1 {
        self.coverage_evidence.effective_coverage()
    }

    #[must_use]
    pub const fn vitality(&self) -> ProcessVitalityV1 {
        self.vitality
    }

    #[must_use]
    pub const fn verifier_instance_hash(&self) -> CanonicalHash {
        self.verifier_instance_hash
    }

    #[must_use]
    pub const fn verifier_service_generation(&self) -> u64 {
        self.verifier_service_generation
    }

    #[must_use]
    pub const fn verified_observation_hash(&self) -> CanonicalHash {
        self.verified_observation_hash
    }

    /// Constructs verifier output for a platform adapter.
    ///
    /// This is not a recovery authorization path. Resource Authority must still match the
    /// non-serializable value to its authenticated subject, purpose, and durable frontier before
    /// consuming any physical settlement or recovery decision.
    #[must_use]
    pub fn from_verified(
        request: ProcessRecoveryFacetRequestV1,
        vitality: ProcessVitalityV1,
        verifier_instance_hash: CanonicalHash,
        verifier_service_generation: u64,
        verified_observation_hash: CanonicalHash,
    ) -> Self {
        Self::from_verified_with_coverage_evidence(
            request,
            ProcessRecoveryCoverageEvidenceV1::leader(verifier_instance_hash),
            vitality,
            verifier_instance_hash,
            verifier_service_generation,
            verified_observation_hash,
        )
    }

    /// Constructs verifier output with its observer-issued coverage evidence.
    ///
    /// A specialized adapter may use this only after it has observed a real closed member set or
    /// old-epoch inventory. The compatibility constructor above deliberately remains leader-only.
    #[must_use]
    pub fn from_verified_with_coverage_evidence(
        request: ProcessRecoveryFacetRequestV1,
        coverage_evidence: ProcessRecoveryCoverageEvidenceV1,
        vitality: ProcessVitalityV1,
        verifier_instance_hash: CanonicalHash,
        verifier_service_generation: u64,
        verified_observation_hash: CanonicalHash,
    ) -> Self {
        Self {
            request,
            coverage_evidence,
            vitality,
            verifier_instance_hash,
            verifier_service_generation,
            verified_observation_hash,
        }
    }
}

/// Self-registration and sandbox child-registration facet.
///
/// `register_current_authority_owner` never accepts a PID. `register_spawned_process` is called
/// only by the physical sandbox spawn path after it owns a concrete child; its result is still
/// useless until the Resource Authority verifies it against the prepared claim's exact scope.
pub trait HostProcessObservationServiceV1: Send + Sync {
    fn register_current_authority_owner(
        &self,
        scope: ProcessObservationScopeV1,
    ) -> Result<HostProcessIdentityRegistrationV1, ProcessObservationErrorV1>;

    fn register_spawned_process(
        &self,
        scope: ProcessObservationScopeV1,
        process_id: u32,
    ) -> Result<HostProcessIdentityRegistrationV1, ProcessObservationErrorV1>;
}

/// Resource-Authority-only low-level re-observation facet.
pub trait HostProcessIdentityRecoveryProbeV1: Send + Sync {
    fn observe_identity_for_authority_recovery(
        &self,
        subject: &VerifiedHostProcessIdentityV1,
        expected_kind: ProcessObservationSubjectKindV1,
        expected_scope: &ProcessObservationScopeV1,
    ) -> Result<HostProcessRecoveryObservationV1, ProcessObservationErrorV1>;

    /// Issues a recovery facet bound to purpose, effective coverage, physical object, and the
    /// authority inventory frontier. Implementations that have not upgraded to this contract
    /// must reject it rather than fabricate a weaker V1 observation.
    fn observe_recovery_facet_for_authority(
        &self,
        _subject: &VerifiedHostProcessIdentityV1,
        _expected_kind: ProcessObservationSubjectKindV1,
        _expected_scope: &ProcessObservationScopeV1,
        _request: ProcessRecoveryFacetRequestV1,
    ) -> Result<HostProcessRecoveryFacetV1, ProcessObservationErrorV1> {
        Err(ProcessObservationErrorV1::RecoveryFacetUnsupported)
    }
}

/// Verifies same-factory registration and recovery evidence.
pub trait HostProcessObservationVerifierV1: Send + Sync {
    fn verifier_instance_hash(&self) -> CanonicalHash;

    fn verifier_service_generation(&self) -> u64;

    fn verify_registration(
        &self,
        registration: HostProcessIdentityRegistrationV1,
        expected_kind: ProcessObservationSubjectKindV1,
        expected_scope: &ProcessObservationScopeV1,
    ) -> Result<VerifiedHostProcessIdentityV1, ProcessObservationErrorV1>;

    fn verify_recovery_observation(
        &self,
        subject: &VerifiedHostProcessIdentityV1,
        expected_kind: ProcessObservationSubjectKindV1,
        expected_scope: &ProcessObservationScopeV1,
        observation: HostProcessRecoveryObservationV1,
    ) -> Result<VerifiedHostProcessRecoveryObservationV1, ProcessObservationErrorV1>;

    /// Consumes a same-factory purpose-bound recovery facet. The default refuses the operation so
    /// an older verifier cannot accidentally accept a new strong-coverage question.
    fn verify_recovery_facet(
        &self,
        _subject: &VerifiedHostProcessIdentityV1,
        _expected_kind: ProcessObservationSubjectKindV1,
        _expected_scope: &ProcessObservationScopeV1,
        _expected_request: &ProcessRecoveryFacetRequestV1,
        _facet: HostProcessRecoveryFacetV1,
    ) -> Result<VerifiedHostProcessRecoveryFacetV1, ProcessObservationErrorV1> {
        Err(ProcessObservationErrorV1::RecoveryFacetUnsupported)
    }
}

/// Same-instance three-facet factory. Composition injects the recovery probe into Resource
/// Authority only; runtime and UI surfaces receive neither a probe nor a verifier.
pub trait HostProcessObservationFactoryV1: Send + Sync {
    fn observation_service(&self) -> Arc<dyn HostProcessObservationServiceV1>;
    fn authority_recovery_probe(&self) -> Arc<dyn HostProcessIdentityRecoveryProbeV1>;
    fn observation_verifier(&self) -> Arc<dyn HostProcessObservationVerifierV1>;
}

/// Closed observation error taxonomy. None of these errors may be mapped to Quiescent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProcessObservationErrorV1 {
    #[error("process is not observable")]
    NotObservable,
    #[error("process birth identity does not match the durable subject")]
    BirthIdentityMismatch,
    #[error("process observation subject kind does not match")]
    SubjectKindMismatch,
    #[error("process observation scope does not match")]
    ScopeMismatch,
    #[error("process observation scope is malformed")]
    MalformedScope,
    #[error("process observation evidence expired")]
    EvidenceExpired,
    #[error("verifier instance hash drifted; evidence rejected")]
    VerifierInstanceDrift,
    #[error("process remains live")]
    StillLive,
    #[error("process recovery facet is malformed")]
    MalformedRecoveryFacet,
    #[error(
        "process observer did not observe the closed member set or old authority epoch required for strong coverage"
    )]
    StrongCoverageEvidenceUnavailable,
    #[error("process recovery facet is unsupported by this observer")]
    RecoveryFacetUnsupported,
}

/// Closed verifier error shared with the kernel capability issuer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityVerifyErrorV1 {
    #[error("capability verify failed: {0}")]
    VerifyFailed(String),
    #[error("capability verify error carries no recoverable state")]
    NotRecoverable,
}

#[cfg(test)]
#[path = "tests/process_observation_tests.rs"]
mod tests;
