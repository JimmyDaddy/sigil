use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::event::canonical_json_bytes;

/// Schema version shared by the provider-neutral Intent Stack V1 domain contracts.
pub const INTENT_CONTRACT_SCHEMA_VERSION: u16 = 1;
/// Schema version exposed by bounded Intent Stack adapter DTOs.
pub const INTENT_PUBLIC_DTO_SCHEMA_VERSION: u16 = 1;
/// Prefix used by canonical Intent Stack digests.
pub const INTENT_CANONICAL_DIGEST_PREFIX: &str = "sha256:jcs-v1:";
/// Maximum bytes retained for an intent title.
pub const MAX_INTENT_TITLE_BYTES: usize = 256;
/// Maximum bytes retained for an intent statement or acceptance criterion.
pub const MAX_INTENT_STATEMENT_BYTES: usize = 4 * 1024;
/// Maximum bytes retained for a safe, non-authoritative operation reason.
pub const MAX_INTENT_REASON_BYTES: usize = 2 * 1024;
/// Maximum intent definitions accepted by one V1 plan.
pub const MAX_INTENTS_PER_PLAN: usize = 64;
/// Maximum criteria accepted by one V1 intent.
pub const MAX_INTENT_CRITERIA: usize = 64;
/// Maximum direct dependencies accepted by one V1 intent.
pub const MAX_INTENT_DEPENDENCIES: usize = 64;

macro_rules! stable_intent_id {
    ($name:ident, $label:literal) => {
        #[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates a bounded, path-safe runtime identity.
            ///
            /// # Errors
            ///
            /// Returns an error when the identity is empty, too long, or unstable.
            pub fn new(value: impl Into<String>) -> Result<Self> {
                let value = value.into();
                validate_stable_identity($label, &value)?;
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

stable_intent_id!(IntentStackId, "intent stack id");
stable_intent_id!(IntentId, "intent id");
stable_intent_id!(IntentCriterionId, "intent criterion id");
stable_intent_id!(IntentExecutionId, "intent execution id");
stable_intent_id!(IntentArtifactId, "intent artifact id");
stable_intent_id!(IntentOperationId, "intent operation id");

/// Monotonic version of an accepted Intent Stack plan.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct IntentStackVersion(u64);

impl IntentStackVersion {
    /// Creates a non-zero stack version.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is zero.
    pub fn new(value: u64) -> Result<Self> {
        if value == 0 {
            bail!("intent stack version must be non-zero");
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for IntentStackVersion {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Stable reference to one immutable intent definition version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentVersionRef {
    pub intent_id: IntentId,
    pub version: u64,
}

impl IntentVersionRef {
    /// Creates a non-zero immutable intent version reference.
    ///
    /// # Errors
    ///
    /// Returns an error when `version` is zero.
    pub fn new(intent_id: IntentId, version: u64) -> Result<Self> {
        if version == 0 {
            bail!("intent version must be non-zero");
        }
        Ok(Self { intent_id, version })
    }

    /// Validates a deserialized reference.
    ///
    /// # Errors
    ///
    /// Returns an error when `version` is zero.
    pub fn validate(&self) -> Result<()> {
        if self.version == 0 {
            bail!("intent version must be non-zero");
        }
        Ok(())
    }
}

/// Content digest over a domain-separated canonical JSON payload.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct IntentDigest(String);

impl IntentDigest {
    /// Parses a canonical Intent Stack digest.
    ///
    /// # Errors
    ///
    /// Returns an error when the value does not use the locked prefix and SHA-256 shape.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let Some(hex) = value.strip_prefix(INTENT_CANONICAL_DIGEST_PREFIX) else {
            bail!("intent digest must use {INTENT_CANONICAL_DIGEST_PREFIX}");
        };
        if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("intent digest must contain a 64-character hexadecimal sha256 digest");
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for IntentDigest {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// SHA-256 digest over exact file or artifact bytes.
///
/// This is intentionally distinct from [`IntentDigest`], which hashes canonical JSON.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct IntentContentDigest(String);

impl IntentContentDigest {
    /// Parses an exact-byte SHA-256 digest.
    ///
    /// # Errors
    ///
    /// Returns an error when the value is not a `sha256:` digest.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let Some(hex) = value.strip_prefix("sha256:") else {
            bail!("intent content digest must use sha256:");
        };
        if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("intent content digest must contain a 64-character hexadecimal sha256 digest");
        }
        Ok(Self(value))
    }

    /// Computes a digest over exact bytes without JSON normalization.
    #[must_use]
    pub fn from_bytes(value: impl AsRef<[u8]>) -> Self {
        Self(format!("sha256:{:x}", Sha256::digest(value.as_ref())))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for IntentContentDigest {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Canonical digest domain. The domain is serialized into the hashed envelope.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentDigestDomain {
    Plan,
    Proposal,
    LayerCore,
    LayerManifest,
    ArtifactManifest,
    OperationPreview,
}

impl IntentDigestDomain {
    fn as_domain(self) -> &'static str {
        match self {
            Self::Plan => "sigil.intent.plan.v1",
            Self::Proposal => "sigil.intent.proposal.v1",
            Self::LayerCore => "sigil.intent.layer_core.v1",
            Self::LayerManifest => "sigil.intent.layer_manifest.v1",
            Self::ArtifactManifest => "sigil.intent.artifact_manifest.v1",
            Self::OperationPreview => "sigil.intent.operation_preview.v1",
        }
    }
}

/// Computes a domain-separated digest over canonical JSON.
///
/// # Errors
///
/// Returns an error when `value` cannot be represented as canonical JSON.
pub fn canonical_intent_digest<T: Serialize>(
    domain: IntentDigestDomain,
    value: &T,
) -> Result<IntentDigest> {
    let payload = serde_json::to_value(value).context("failed to serialize intent digest input")?;
    let envelope = serde_json::json!({
        "domain": domain.as_domain(),
        "schema_version": INTENT_CONTRACT_SCHEMA_VERSION,
        "payload": payload,
    });
    let bytes = canonical_json_bytes(&envelope)?;
    IntentDigest::new(format!(
        "{INTENT_CANONICAL_DIGEST_PREFIX}{:x}",
        Sha256::digest(bytes)
    ))
}

/// Admission path for an intent plan.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentPlanKind {
    UserDeclaredRoot,
    SuggestedDecomposition,
}

/// Provenance for one intent definition. Provenance is descriptive, never mutation authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum IntentSourceV1 {
    UserTurn {
        source_turn_id: String,
    },
    AcceptedSuggestion {
        source_turn_id: String,
        proposal_digest: IntentDigest,
    },
    TrustedSpec {
        source_ref: String,
        content_digest: IntentContentDigest,
    },
}

/// Untrusted criterion proposed by a provider before runtime identity resolution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentProposalCriterionV1 {
    pub criterion_alias: String,
    pub statement: String,
    pub required: bool,
}

/// Untrusted intent proposal. Aliases are display-local and never mutation authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentProposalUnitV1 {
    pub intent_alias: String,
    pub title: String,
    pub statement: String,
    pub acceptance_criteria: Vec<IntentProposalCriterionV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on_aliases: Vec<String>,
}

/// Digest-bound suggested decomposition before independent user acceptance.
///
/// It deliberately contains no [`IntentId`], stack version, execution identity, or authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentPlanProposalV1 {
    pub schema_version: u16,
    pub proposal_id: String,
    pub source_turn_id: String,
    pub intents: Vec<IntentProposalUnitV1>,
    pub proposal_digest: IntentDigest,
}

impl IntentPlanProposalV1 {
    /// Creates a digest-bound provider proposal without granting acceptance authority.
    ///
    /// Runtime identities and the proposal digest are always derived by the host. Provider output
    /// supplies only display-local aliases and descriptive intent content.
    ///
    /// # Errors
    ///
    /// Returns an error when the proposal contract is malformed.
    pub fn new(
        proposal_id: impl Into<String>,
        source_turn_id: impl Into<String>,
        intents: Vec<IntentProposalUnitV1>,
    ) -> Result<Self> {
        let mut proposal = Self {
            schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
            proposal_id: proposal_id.into(),
            source_turn_id: source_turn_id.into(),
            intents,
            proposal_digest: canonical_intent_digest(
                IntentDigestDomain::Proposal,
                &serde_json::json!({}),
            )?,
        };
        proposal.proposal_digest = proposal.computed_digest()?;
        proposal.validate_contract()?;
        Ok(proposal)
    }

    /// Computes the canonical proposal digest with the digest field omitted.
    ///
    /// # Errors
    ///
    /// Returns an error when the proposal cannot be serialized.
    pub fn computed_digest(&self) -> Result<IntentDigest> {
        let mut value = serde_json::to_value(self)
            .context("failed to serialize intent proposal digest payload")?;
        value
            .as_object_mut()
            .context("intent proposal digest payload must be an object")?
            .remove("proposal_digest");
        canonical_intent_digest(IntentDigestDomain::Proposal, &value)
    }

    /// Validates proposal shape without granting acceptance or execution authority.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported schema, invalid aliases, missing criteria, or digest
    /// mismatch.
    pub fn validate_contract(&self) -> Result<()> {
        validate_schema_version(self.schema_version)?;
        validate_bounded_identity("intent proposal id", &self.proposal_id)?;
        validate_bounded_identity("intent proposal source turn id", &self.source_turn_id)?;
        if self.intents.is_empty() {
            bail!("intent proposal must contain at least one intent");
        }
        if self.intents.len() > MAX_INTENTS_PER_PLAN {
            bail!("intent proposal contains too many intents");
        }
        let mut aliases = BTreeSet::new();
        for intent in &self.intents {
            validate_stable_identity("intent proposal alias", &intent.intent_alias)?;
            validate_bounded_text("intent title", &intent.title, MAX_INTENT_TITLE_BYTES)?;
            validate_bounded_text(
                "intent statement",
                &intent.statement,
                MAX_INTENT_STATEMENT_BYTES,
            )?;
            if intent.acceptance_criteria.is_empty() {
                bail!("intent proposal must contain at least one acceptance criterion");
            }
            if intent.acceptance_criteria.len() > MAX_INTENT_CRITERIA {
                bail!("intent proposal contains too many acceptance criteria");
            }
            if intent.depends_on_aliases.len() > MAX_INTENT_DEPENDENCIES {
                bail!("intent proposal contains too many dependencies");
            }
            if !aliases.insert(intent.intent_alias.as_str()) {
                bail!("intent proposal contains duplicate aliases");
            }
            let mut criteria = BTreeSet::new();
            for criterion in &intent.acceptance_criteria {
                validate_stable_identity(
                    "intent proposal criterion alias",
                    &criterion.criterion_alias,
                )?;
                validate_bounded_text(
                    "intent proposal criterion",
                    &criterion.statement,
                    MAX_INTENT_STATEMENT_BYTES,
                )?;
                if !criteria.insert(criterion.criterion_alias.as_str()) {
                    bail!("intent proposal contains duplicate criterion aliases");
                }
            }
        }
        for intent in &self.intents {
            let mut dependencies = BTreeSet::new();
            for dependency in &intent.depends_on_aliases {
                if dependency == &intent.intent_alias {
                    bail!("intent proposal cannot depend on itself");
                }
                if !aliases.contains(dependency.as_str()) {
                    bail!("intent proposal dependency alias does not exist");
                }
                if !dependencies.insert(dependency.as_str()) {
                    bail!("intent proposal contains duplicate dependency aliases");
                }
            }
        }
        validate_proposal_dependency_dag(&self.intents)?;
        if self.computed_digest()? != self.proposal_digest {
            bail!("intent proposal canonical digest mismatch");
        }
        Ok(())
    }
}

/// One checkable criterion attached to an immutable intent version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentAcceptanceCriterionV1 {
    pub criterion_id: IntentCriterionId,
    pub statement: String,
    pub required: bool,
}

/// One immutable definition inside an accepted plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentDefinitionV1 {
    pub intent_ref: IntentVersionRef,
    pub title: String,
    pub statement: String,
    pub acceptance_criteria: Vec<IntentAcceptanceCriterionV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<IntentId>,
    pub source: IntentSourceV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<IntentVersionRef>,
}

/// Immutable, digest-bound plan admitted by the runtime.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentPlanV1 {
    pub schema_version: u16,
    pub stack_id: IntentStackId,
    pub stack_version: IntentStackVersion,
    pub workspace_id: String,
    pub source_session_id: String,
    pub kind: IntentPlanKind,
    pub intents: Vec<IntentDefinitionV1>,
    pub plan_digest: IntentDigest,
}

impl IntentPlanV1 {
    /// Computes the canonical plan digest with the digest field omitted.
    ///
    /// # Errors
    ///
    /// Returns an error when the plan cannot be serialized.
    pub fn computed_digest(&self) -> Result<IntentDigest> {
        canonical_intent_digest(IntentDigestDomain::Plan, &self.digest_payload()?)
    }

    /// Validates the locked plan schema and its canonical digest.
    ///
    /// This validates contract shape only. Admission, acceptance, DAG scheduling, and durable
    /// batch behavior are implemented by later RFC-0051 slices.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported schema, invalid shape, or digest mismatch.
    pub fn validate_contract(&self) -> Result<()> {
        validate_schema_version(self.schema_version)?;
        validate_bounded_identity("intent workspace id", &self.workspace_id)?;
        validate_bounded_identity("intent source session id", &self.source_session_id)?;
        if self.intents.is_empty() {
            bail!("intent plan must contain at least one intent");
        }
        if self.intents.len() > MAX_INTENTS_PER_PLAN {
            bail!("intent plan contains too many intents");
        }
        match self.kind {
            IntentPlanKind::UserDeclaredRoot => {
                if self.intents.len() != 1 {
                    bail!("user-declared root plan must contain exactly one intent");
                }
                if !matches!(&self.intents[0].source, IntentSourceV1::UserTurn { .. }) {
                    bail!("user-declared root plan must use user-turn provenance");
                }
            }
            IntentPlanKind::SuggestedDecomposition => {
                if self
                    .intents
                    .iter()
                    .any(|intent| matches!(&intent.source, IntentSourceV1::UserTurn { .. }))
                {
                    bail!("suggested decomposition cannot use direct user-root provenance");
                }
            }
        }
        let mut ids = BTreeSet::new();
        for intent in &self.intents {
            intent.intent_ref.validate()?;
            validate_bounded_text("intent title", &intent.title, MAX_INTENT_TITLE_BYTES)?;
            validate_bounded_text(
                "intent statement",
                &intent.statement,
                MAX_INTENT_STATEMENT_BYTES,
            )?;
            if intent.acceptance_criteria.is_empty() {
                bail!("intent must contain at least one acceptance criterion");
            }
            if intent.acceptance_criteria.len() > MAX_INTENT_CRITERIA {
                bail!("intent contains too many acceptance criteria");
            }
            if intent.depends_on.len() > MAX_INTENT_DEPENDENCIES {
                bail!("intent contains too many dependencies");
            }
            let mut criterion_ids = BTreeSet::new();
            for criterion in &intent.acceptance_criteria {
                validate_bounded_text(
                    "intent acceptance criterion",
                    &criterion.statement,
                    MAX_INTENT_STATEMENT_BYTES,
                )?;
                if !criterion_ids.insert(criterion.criterion_id.as_str()) {
                    bail!("intent contains duplicate acceptance criterion ids");
                }
            }
            if !ids.insert(intent.intent_ref.intent_id.as_str()) {
                bail!("intent plan contains duplicate intent ids");
            }
            validate_intent_source(&intent.source)?;
            if let Some(supersedes) = &intent.supersedes {
                supersedes.validate()?;
                if supersedes.intent_id != intent.intent_ref.intent_id
                    || supersedes.version >= intent.intent_ref.version
                {
                    bail!("intent supersedes must reference an earlier version of the same intent");
                }
            }
        }
        for intent in &self.intents {
            let mut dependencies = BTreeSet::new();
            for dependency in &intent.depends_on {
                if dependency == &intent.intent_ref.intent_id {
                    bail!("intent cannot depend on itself");
                }
                if !ids.contains(dependency.as_str()) {
                    bail!("intent dependency does not exist in the same plan");
                }
                if !dependencies.insert(dependency.as_str()) {
                    bail!("intent contains duplicate dependencies");
                }
            }
        }
        validate_dependency_dag(&self.intents)?;
        if self.computed_digest()? != self.plan_digest {
            bail!("intent plan canonical digest mismatch");
        }
        Ok(())
    }

    fn digest_payload(&self) -> Result<Value> {
        let mut value =
            serde_json::to_value(self).context("failed to serialize intent plan digest payload")?;
        value
            .as_object_mut()
            .context("intent plan digest payload must be an object")?
            .remove("plan_digest");
        Ok(value)
    }
}

/// Whether an immutable intent definition is active in the accepted plan lineage.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentDefinitionState {
    Proposed,
    Accepted,
    Superseded,
    Invalid,
}

/// Application state is intentionally independent from definition state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentApplicationState {
    Unapplied,
    Applied,
    Dropped,
    NeedsReview,
    NeedsRebuild,
    ReadOnly,
    OutOfScope,
}

/// Mutation authority inherited at a session/workspace boundary.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentAuthorityState {
    Active,
    ReadOnlyProvenance,
    OutOfScope,
}

/// Exact runtime execution origin for one intent binding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum IntentExecutionOriginV1 {
    Task {
        task_id: String,
        task_plan_version: u32,
        step_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt_id: Option<String>,
    },
    Chat {
        root_logical_run_id: String,
        source_turn_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt_id: Option<String>,
    },
}

/// Purpose of an execution binding.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentExecutionBindingKind {
    Admission,
    Mutation,
    Review,
    Verification,
}

/// Provenance link from an immutable intent version to a concrete execution origin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentExecutionBindingV1 {
    pub execution_id: IntentExecutionId,
    pub intent_ref: IntentVersionRef,
    pub origin: IntentExecutionOriginV1,
    pub binding_kind: IntentExecutionBindingKind,
    pub source_event_id: String,
}

/// Classification of an artifact associated with one intent.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentArtifactKind {
    FileHunk,
    TestEvidence,
    Documentation,
    ChangeSet,
    VerificationReceipt,
    UnsupportedSideEffect,
}

/// Conservative V1 ownership state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentArtifactOwnership {
    Exclusive,
    Shared,
    Unowned,
    Drifted,
}

/// Current lifecycle of mutation evidence required for executable intent operations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentArtifactAvailability {
    Available,
    Deleted,
    Expired,
    Corrupted,
}

/// Half-open UTF-8 byte range in canonical before/after file bytes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentByteRangeV1 {
    pub start: u64,
    pub end: u64,
}

/// Bounded, adapter-neutral artifact subject. It never carries raw patch or file content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BoundedIntentArtifactSubjectV1 {
    FileHunk {
        normalized_relative_path: String,
        before_file_digest: IntentContentDigest,
        after_file_digest: IntentContentDigest,
        old_range: IntentByteRangeV1,
        new_range: IntentByteRangeV1,
        old_content_digest: IntentContentDigest,
        new_content_digest: IntentContentDigest,
        layer_core_digest: IntentDigest,
    },
    Evidence {
        reference_id: String,
    },
    UnsupportedSideEffect {
        effect_kind: String,
        safe_summary: String,
    },
}

/// Durable origin evidence for an artifact binding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentArtifactProvenanceV1 {
    pub source_event_id: String,
    pub execution_id: IntentExecutionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_batch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changeset_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_receipt_id: Option<String>,
}

/// One bounded artifact binding. Raw forward/reverse patch bytes live in the artifact store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentArtifactBindingV1 {
    pub artifact_id: IntentArtifactId,
    pub artifact_kind: IntentArtifactKind,
    pub subject: BoundedIntentArtifactSubjectV1,
    pub ownership: IntentArtifactOwnership,
    pub availability: IntentArtifactAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_digest: Option<IntentContentDigest>,
    pub after_digest: IntentContentDigest,
    pub provenance: IntentArtifactProvenanceV1,
}

impl IntentArtifactBindingV1 {
    /// Validates bounded artifact shape without inferring ownership or availability.
    ///
    /// # Errors
    ///
    /// Returns an error when subject shape, byte ranges, or kind binding is inconsistent.
    pub fn validate_contract(&self) -> Result<()> {
        validate_bounded_identity(
            "intent artifact source event id",
            &self.provenance.source_event_id,
        )?;
        if let Some(mutation_batch_id) = &self.provenance.mutation_batch_id {
            validate_bounded_identity("intent artifact mutation batch id", mutation_batch_id)?;
        }
        if let Some(changeset_id) = &self.provenance.changeset_id {
            validate_bounded_identity("intent artifact change set id", changeset_id)?;
        }
        if let Some(receipt_id) = &self.provenance.verification_receipt_id {
            validate_bounded_identity("intent artifact verification receipt id", receipt_id)?;
        }
        match &self.subject {
            BoundedIntentArtifactSubjectV1::FileHunk {
                normalized_relative_path,
                old_range,
                new_range,
                old_content_digest,
                new_content_digest,
                ..
            } => {
                if self.artifact_kind != IntentArtifactKind::FileHunk {
                    bail!("file hunk subject requires file_hunk artifact kind");
                }
                if self.provenance.mutation_batch_id.is_none()
                    || self.provenance.changeset_id.is_none()
                {
                    bail!("file hunk artifact requires mutation and change set lineage");
                }
                validate_normalized_relative_path(normalized_relative_path)?;
                if old_range.start > old_range.end || new_range.start > new_range.end {
                    bail!("intent byte range start must not exceed end");
                }
                if self.before_digest.as_ref() != Some(old_content_digest)
                    || &self.after_digest != new_content_digest
                {
                    bail!("intent file hunk content digests are inconsistent");
                }
            }
            BoundedIntentArtifactSubjectV1::Evidence { reference_id } => {
                validate_bounded_identity("intent artifact evidence reference", reference_id)?;
                if matches!(
                    self.artifact_kind,
                    IntentArtifactKind::FileHunk | IntentArtifactKind::UnsupportedSideEffect
                ) {
                    bail!("evidence subject has incompatible artifact kind");
                }
            }
            BoundedIntentArtifactSubjectV1::UnsupportedSideEffect {
                effect_kind,
                safe_summary,
            } => {
                if self.artifact_kind != IntentArtifactKind::UnsupportedSideEffect {
                    bail!("unsupported side effect subject requires matching artifact kind");
                }
                validate_bounded_identity("unsupported side effect kind", effect_kind)?;
                validate_bounded_text(
                    "unsupported side effect summary",
                    safe_summary,
                    MAX_INTENT_REASON_BYTES,
                )?;
                if self.ownership == IntentArtifactOwnership::Exclusive {
                    bail!("unsupported side effect cannot have exclusive ownership");
                }
            }
        }
        Ok(())
    }
}

/// Cycle-free core of an executable intent layer.
///
/// The core is digested before hunk identities are derived. Artifact bindings may reference this
/// digest, while the final layer manifest references the resulting artifact manifest in one
/// direction only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentLayerCoreV1 {
    pub schema_version: u16,
    pub intent_ref: IntentVersionRef,
    pub execution_id: IntentExecutionId,
    pub execution_origin: IntentExecutionOriginV1,
    pub base_snapshot_id: String,
    pub result_snapshot_id: String,
    pub changeset_ids: Vec<String>,
    pub operation_ids: Vec<IntentOperationId>,
    pub forward_patch_artifact_id: String,
    pub reverse_patch_artifact_id: String,
    pub core_digest: IntentDigest,
}

impl IntentLayerCoreV1 {
    /// Computes the canonical layer core digest with the digest field omitted.
    ///
    /// # Errors
    ///
    /// Returns an error when the layer core cannot be serialized.
    pub fn computed_digest(&self) -> Result<IntentDigest> {
        let mut value = serde_json::to_value(self)
            .context("failed to serialize intent layer core digest payload")?;
        value
            .as_object_mut()
            .context("intent layer core digest payload must be an object")?
            .remove("core_digest");
        canonical_intent_digest(IntentDigestDomain::LayerCore, &value)
    }

    /// Validates exact terminal execution lineage and the cycle-free core digest.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported schema, missing terminal attempt, or digest mismatch.
    pub fn validate_contract(&self) -> Result<()> {
        validate_schema_version(self.schema_version)?;
        self.intent_ref.validate()?;
        validate_execution_origin(&self.execution_origin, true)?;
        validate_bounded_identity("intent layer base snapshot id", &self.base_snapshot_id)?;
        validate_bounded_identity("intent layer result snapshot id", &self.result_snapshot_id)?;
        if self.changeset_ids.is_empty() {
            bail!("executable intent layer requires change set lineage");
        }
        let mut changeset_ids = BTreeSet::new();
        for changeset_id in &self.changeset_ids {
            validate_bounded_identity("intent layer change set id", changeset_id)?;
            if !changeset_ids.insert(changeset_id.as_str()) {
                bail!("intent layer contains duplicate change set ids");
            }
        }
        validate_bounded_identity(
            "intent layer forward patch artifact id",
            &self.forward_patch_artifact_id,
        )?;
        validate_bounded_identity(
            "intent layer reverse patch artifact id",
            &self.reverse_patch_artifact_id,
        )?;
        if self.computed_digest()? != self.core_digest {
            bail!("intent layer core canonical digest mismatch");
        }
        Ok(())
    }
}

/// Exact ordered artifact set derived from one already-digested layer core.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentArtifactManifestV1 {
    pub schema_version: u16,
    pub intent_ref: IntentVersionRef,
    pub execution_id: IntentExecutionId,
    pub layer_core_digest: IntentDigest,
    pub artifacts: Vec<IntentArtifactBindingV1>,
    pub manifest_digest: IntentDigest,
}

impl IntentArtifactManifestV1 {
    /// Computes the canonical artifact manifest digest with the digest field omitted.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest cannot be serialized.
    pub fn computed_digest(&self) -> Result<IntentDigest> {
        let mut value = serde_json::to_value(self)
            .context("failed to serialize intent artifact manifest digest payload")?;
        value
            .as_object_mut()
            .context("intent artifact manifest digest payload must be an object")?
            .remove("manifest_digest");
        canonical_intent_digest(IntentDigestDomain::ArtifactManifest, &value)
    }

    /// Validates the one-way layer-core-to-artifact digest graph.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported schema, empty or invalid bindings, mismatched core
    /// references, or digest mismatch.
    pub fn validate_contract(&self) -> Result<()> {
        validate_schema_version(self.schema_version)?;
        self.intent_ref.validate()?;
        if self.artifacts.is_empty() {
            bail!("intent artifact manifest must contain at least one artifact");
        }
        let mut artifact_ids = BTreeSet::new();
        for artifact in &self.artifacts {
            artifact.validate_contract()?;
            if !artifact_ids.insert(artifact.artifact_id.as_str()) {
                bail!("intent artifact manifest contains duplicate artifact ids");
            }
            if artifact.provenance.execution_id != self.execution_id {
                bail!("intent artifact provenance references a different execution");
            }
            if let BoundedIntentArtifactSubjectV1::FileHunk {
                layer_core_digest, ..
            } = &artifact.subject
                && layer_core_digest != &self.layer_core_digest
            {
                bail!("intent file hunk references a different layer core digest");
            }
        }
        if self.computed_digest()? != self.manifest_digest {
            bail!("intent artifact manifest canonical digest mismatch");
        }
        Ok(())
    }
}

/// Content-addressed final layer manifest with a cycle-free artifact reference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentLayerManifestV1 {
    pub schema_version: u16,
    pub core: IntentLayerCoreV1,
    pub artifact_manifest_id: String,
    pub artifact_manifest_digest: IntentDigest,
    pub manifest_digest: IntentDigest,
}

impl IntentLayerManifestV1 {
    /// Computes the final layer manifest digest with the digest field omitted.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest cannot be serialized.
    pub fn computed_digest(&self) -> Result<IntentDigest> {
        let mut value = serde_json::to_value(self)
            .context("failed to serialize intent layer manifest digest payload")?;
        value
            .as_object_mut()
            .context("intent layer manifest digest payload must be an object")?
            .remove("manifest_digest");
        canonical_intent_digest(IntentDigestDomain::LayerManifest, &value)
    }

    /// Validates the final cycle-free layer envelope.
    ///
    /// # Errors
    ///
    /// Returns an error when schema, core, or artifact manifest identity is invalid.
    pub fn validate_contract(&self) -> Result<()> {
        validate_schema_version(self.schema_version)?;
        self.core.validate_contract()?;
        validate_bounded_identity("intent artifact manifest id", &self.artifact_manifest_id)?;
        if self.computed_digest()? != self.manifest_digest {
            bail!("intent layer manifest canonical digest mismatch");
        }
        Ok(())
    }
}

/// Strength of a criterion-to-verification link.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentCriterionEvidenceLevel {
    Advisory,
    SystemVerified,
}

/// Bounded verification link. System verification still requires live policy validation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentCriterionEvidenceV1 {
    pub intent_ref: IntentVersionRef,
    pub criterion_id: IntentCriterionId,
    pub level: IntentCriterionEvidenceLevel,
    pub receipt_id: String,
    pub parent_snapshot_id: String,
    pub verification_policy_digest: IntentDigest,
    pub changeset_ids: Vec<String>,
    pub source_event_id: String,
}

/// Operation kinds covered by the V1 adapter contract.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentOperationKind {
    Drop,
    ReviseImpactPreview,
    ReplaceImpactPreview,
    Adopt,
}

/// Terminal result of a durable intent operation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentOperationResolution {
    Committed,
    Rejected,
    Cancelled,
    Conflicted,
    PartiallyApplied,
    Interrupted,
}

/// Locked typed error taxonomy for Intent Stack V1 operations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentOperationErrorCode {
    UnsupportedSchema,
    IntentHistoryUnavailable,
    UnknownIntent,
    UnknownOperation,
    StaleIntentVersion,
    StaleStackVersion,
    InvalidDependencyGraph,
    TargetNotLeaf,
    SharedArtifact,
    UnownedArtifact,
    DriftedArtifact,
    ArtifactUnavailable,
    ArtifactDigestMismatch,
    UnsupportedArtifact,
    UnsupportedSideEffect,
    MissingExecutionLineage,
    MissingParentMutationEvidence,
    MissingCurrentVerificationEvidence,
    PreviewDigestMismatch,
    WorkspaceRevisionMismatch,
    PermissionDenied,
    ApprovalAuthorityUnavailable,
    WorkspaceLeaseUnavailable,
    WorkspaceOutOfScope,
    OperationStateConflict,
    IntentStateConflict,
    PartialApplication,
    ReconciliationRequired,
}

/// File action displayed in an exact intent operation preview.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentOperationFileAction {
    Create,
    Update,
    Delete,
}

/// Bounded file effect in an exact preview. No raw bytes or absolute paths are exposed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentOperationFileSummaryV1 {
    pub normalized_relative_path: String,
    pub action: IntentOperationFileAction,
    pub artifact_ids: Vec<IntentArtifactId>,
}

/// Verification transition caused by an intent operation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentVerificationImpact {
    BecomesStale,
    RerunRequired,
}

/// Bounded verification effect in an exact preview.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentVerificationImpactV1 {
    pub receipt_id: String,
    pub impact: IntentVerificationImpact,
}

/// Exact, digest-bound preview shared by TUI and typed adapters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentOperationPreviewV1 {
    pub schema_version: u16,
    pub operation_id: IntentOperationId,
    pub operation_kind: IntentOperationKind,
    pub stack_id: IntentStackId,
    pub stack_version: IntentStackVersion,
    pub target_intents: Vec<IntentVersionRef>,
    pub target_is_leaf: bool,
    pub workspace_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
    pub file_effects: Vec<IntentOperationFileSummaryV1>,
    pub retained_intents: Vec<IntentVersionRef>,
    pub verification_impacts: Vec<IntentVerificationImpactV1>,
    pub conflicts: Vec<IntentConflictV1>,
    pub preview_digest: IntentDigest,
}

impl IntentOperationPreviewV1 {
    /// Computes the canonical preview digest with the digest field omitted.
    ///
    /// # Errors
    ///
    /// Returns an error when the preview cannot be serialized.
    pub fn computed_digest(&self) -> Result<IntentDigest> {
        let mut value = serde_json::to_value(self)
            .context("failed to serialize intent operation preview digest payload")?;
        value
            .as_object_mut()
            .context("intent operation preview digest payload must be an object")?
            .remove("preview_digest");
        canonical_intent_digest(IntentDigestDomain::OperationPreview, &value)
    }

    /// Validates exact preview shape and canonical digest.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported schema, missing targets, invalid file paths, expired
    /// shape, or digest mismatch.
    pub fn validate_contract(&self) -> Result<()> {
        validate_schema_version(self.schema_version)?;
        if self.target_intents.is_empty() {
            bail!("intent operation preview requires at least one target");
        }
        if self.expires_at_ms == Some(0) {
            bail!("intent operation preview expiry must be non-zero");
        }
        let mut target_intents = BTreeSet::new();
        for intent_ref in &self.target_intents {
            intent_ref.validate()?;
            if !target_intents.insert(intent_ref) {
                bail!("intent operation preview contains duplicate targets");
            }
        }
        let mut retained_intents = BTreeSet::new();
        for intent_ref in &self.retained_intents {
            intent_ref.validate()?;
            if target_intents.contains(intent_ref) {
                bail!("intent operation preview cannot both target and retain one intent");
            }
            if !retained_intents.insert(intent_ref) {
                bail!("intent operation preview contains duplicate retained intents");
            }
        }
        for file in &self.file_effects {
            validate_normalized_relative_path(&file.normalized_relative_path)?;
            if file.artifact_ids.is_empty() {
                bail!("intent operation file effect requires artifact ids");
            }
        }
        for impact in &self.verification_impacts {
            validate_bounded_identity(
                "intent operation verification receipt id",
                &impact.receipt_id,
            )?;
        }
        for conflict in &self.conflicts {
            if let Some(intent_ref) = &conflict.intent_ref {
                intent_ref.validate()?;
            }
            validate_bounded_text(
                "intent operation conflict reason",
                &conflict.safe_reason,
                MAX_INTENT_REASON_BYTES,
            )?;
        }
        if self.computed_digest()? != self.preview_digest {
            bail!("intent operation preview canonical digest mismatch");
        }
        Ok(())
    }
}

/// Structured fail-closed conflict retained by domain events and bounded DTOs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentConflictV1 {
    pub code: IntentOperationErrorCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_ref: Option<IntentVersionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<IntentArtifactId>,
    pub safe_reason: String,
}

/// How an accepted plan was authorized independently from proposal generation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentAcceptanceKind {
    UserDeclaredRootAdmission,
    ExplicitUserConfirmation,
    ContentBoundSpecDecision,
}

/// Exact TaskPlan version admitted in the same durable writer batch as an IntentPlan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentTaskPlanBindingV1 {
    pub task_id: String,
    pub task_plan_version: u32,
}

/// Recovery-critical Intent Stack event payload contract.
///
/// R51.0 froze this schema. R51.1 registers the stack/plan/acceptance subset; later slices register
/// the execution, artifact, verification, and operation variants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "record", rename_all = "snake_case", deny_unknown_fields)]
pub enum IntentEventV1 {
    StackCreated {
        schema_version: u16,
        stack_id: IntentStackId,
        workspace_id: String,
        source_session_id: String,
    },
    PlanRecorded {
        schema_version: u16,
        plan: IntentPlanV1,
    },
    PlanAccepted {
        schema_version: u16,
        stack_id: IntentStackId,
        stack_version: IntentStackVersion,
        plan_digest: IntentDigest,
        acceptance_kind: IntentAcceptanceKind,
        source_turn_id: String,
        acceptance_authority_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_plan_binding: Option<IntentTaskPlanBindingV1>,
    },
    ExecutionBound {
        schema_version: u16,
        stack_id: IntentStackId,
        stack_version: IntentStackVersion,
        binding: IntentExecutionBindingV1,
    },
    ChangeSetBound {
        schema_version: u16,
        intent_ref: IntentVersionRef,
        execution_id: IntentExecutionId,
        changeset_ids: Vec<String>,
    },
    LayerManifestRecorded {
        schema_version: u16,
        manifest: IntentLayerManifestV1,
    },
    ArtifactBindingsRecorded {
        schema_version: u16,
        manifest: IntentArtifactManifestV1,
    },
    VerificationLinked {
        schema_version: u16,
        evidence: Vec<IntentCriterionEvidenceV1>,
    },
    OperationRequested {
        schema_version: u16,
        preview: IntentOperationPreviewV1,
        safe_reason: String,
    },
    OperationPrepared {
        schema_version: u16,
        operation_id: IntentOperationId,
        stack_id: IntentStackId,
        stack_version: IntentStackVersion,
        preview_digest: IntentDigest,
        artifact_manifest_digest: IntentDigest,
        workspace_revision: u64,
        permission_policy_digest: IntentDigest,
        approval_authority_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at_ms: Option<u64>,
        mutation_batch_id: String,
    },
    OperationResolved {
        schema_version: u16,
        operation_id: IntentOperationId,
        resolution: IntentOperationResolution,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mutation_batch_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_snapshot_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_code: Option<IntentOperationErrorCode>,
    },
    ConflictRecorded {
        schema_version: u16,
        stack_id: IntentStackId,
        stack_version: IntentStackVersion,
        conflict: IntentConflictV1,
    },
    VersionSuperseded {
        schema_version: u16,
        previous: IntentVersionRef,
        replacement: IntentVersionRef,
        safe_reason: String,
    },
}

impl IntentEventV1 {
    /// Returns the durable event type name reserved by the R51.0 contract.
    #[must_use]
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::StackCreated { .. } => "intent_stack_created",
            Self::PlanRecorded { .. } => "intent_plan_recorded",
            Self::PlanAccepted { .. } => "intent_plan_accepted",
            Self::ExecutionBound { .. } => "intent_execution_bound",
            Self::ChangeSetBound { .. } => "intent_change_set_bound",
            Self::LayerManifestRecorded { .. } => "intent_layer_manifest_recorded",
            Self::ArtifactBindingsRecorded { .. } => "intent_artifact_bindings_recorded",
            Self::VerificationLinked { .. } => "intent_verification_linked",
            Self::OperationRequested { .. } => "intent_operation_requested",
            Self::OperationPrepared { .. } => "intent_operation_prepared",
            Self::OperationResolved { .. } => "intent_operation_resolved",
            Self::ConflictRecorded { .. } => "intent_conflict_recorded",
            Self::VersionSuperseded { .. } => "intent_version_superseded",
        }
    }

    /// Validates the common event schema version.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload uses an unsupported version.
    pub fn validate_schema(&self) -> Result<()> {
        let version = match self {
            Self::StackCreated { schema_version, .. }
            | Self::PlanRecorded { schema_version, .. }
            | Self::PlanAccepted { schema_version, .. }
            | Self::ExecutionBound { schema_version, .. }
            | Self::ChangeSetBound { schema_version, .. }
            | Self::LayerManifestRecorded { schema_version, .. }
            | Self::ArtifactBindingsRecorded { schema_version, .. }
            | Self::VerificationLinked { schema_version, .. }
            | Self::OperationRequested { schema_version, .. }
            | Self::OperationPrepared { schema_version, .. }
            | Self::OperationResolved { schema_version, .. }
            | Self::ConflictRecorded { schema_version, .. }
            | Self::VersionSuperseded { schema_version, .. } => *schema_version,
        };
        validate_schema_version(version)
    }

    /// Validates bounded payload shape without applying or persisting the event.
    ///
    /// # Errors
    ///
    /// Returns an error when nested contract shape or a terminal field combination is invalid.
    pub fn validate_contract(&self) -> Result<()> {
        self.validate_schema()?;
        match self {
            Self::StackCreated {
                workspace_id,
                source_session_id,
                ..
            } => {
                validate_bounded_identity("intent workspace id", workspace_id)?;
                validate_bounded_identity("intent source session id", source_session_id)
            }
            Self::PlanRecorded { plan, .. } => plan.validate_contract(),
            Self::PlanAccepted {
                source_turn_id,
                acceptance_authority_id,
                task_plan_binding,
                ..
            } => {
                validate_bounded_identity("intent acceptance source turn id", source_turn_id)?;
                validate_bounded_identity(
                    "intent acceptance authority id",
                    acceptance_authority_id,
                )?;
                if let Some(binding) = task_plan_binding {
                    validate_bounded_identity("intent acceptance task id", &binding.task_id)?;
                    if binding.task_plan_version == 0 {
                        bail!("intent acceptance task plan version must be non-zero");
                    }
                }
                Ok(())
            }
            Self::ExecutionBound { binding, .. } => validate_execution_binding(binding),
            Self::ChangeSetBound {
                intent_ref,
                changeset_ids,
                ..
            } => {
                intent_ref.validate()?;
                if changeset_ids.is_empty() {
                    bail!("intent change set binding requires at least one change set");
                }
                for changeset_id in changeset_ids {
                    validate_bounded_identity("intent change set id", changeset_id)?;
                }
                Ok(())
            }
            Self::LayerManifestRecorded { manifest, .. } => manifest.validate_contract(),
            Self::ArtifactBindingsRecorded { manifest, .. } => manifest.validate_contract(),
            Self::VerificationLinked { evidence, .. } => {
                if evidence.is_empty() {
                    bail!("intent verification link requires evidence");
                }
                for item in evidence {
                    item.intent_ref.validate()?;
                    validate_bounded_identity(
                        "intent verification source event id",
                        &item.source_event_id,
                    )?;
                    validate_bounded_identity("intent verification receipt id", &item.receipt_id)?;
                    validate_bounded_identity(
                        "intent verification parent snapshot id",
                        &item.parent_snapshot_id,
                    )?;
                    if item.level == IntentCriterionEvidenceLevel::SystemVerified
                        && item.changeset_ids.is_empty()
                    {
                        bail!("system-verified intent criterion requires change set lineage");
                    }
                    let mut changesets = BTreeSet::new();
                    for changeset_id in &item.changeset_ids {
                        validate_bounded_identity(
                            "intent verification change set id",
                            changeset_id,
                        )?;
                        if !changesets.insert(changeset_id) {
                            bail!("intent verification evidence repeats a change set");
                        }
                    }
                }
                Ok(())
            }
            Self::OperationRequested {
                preview,
                safe_reason,
                ..
            } => {
                preview.validate_contract()?;
                validate_bounded_text(
                    "intent operation reason",
                    safe_reason,
                    MAX_INTENT_REASON_BYTES,
                )
            }
            Self::OperationPrepared {
                approval_authority_id,
                expires_at_ms,
                mutation_batch_id,
                ..
            } => {
                validate_bounded_identity(
                    "intent operation approval authority id",
                    approval_authority_id,
                )?;
                if *expires_at_ms == Some(0) {
                    bail!("intent operation prepared expiry must be non-zero");
                }
                validate_bounded_identity("intent operation mutation batch id", mutation_batch_id)
            }
            Self::OperationResolved {
                resolution,
                mutation_batch_id,
                result_snapshot_id,
                error_code,
                ..
            } => match resolution {
                IntentOperationResolution::Committed => {
                    if mutation_batch_id.is_none()
                        || result_snapshot_id.is_none()
                        || error_code.is_some()
                    {
                        bail!("committed intent operation requires batch and result without error");
                    }
                    Ok(())
                }
                IntentOperationResolution::Rejected | IntentOperationResolution::Cancelled => {
                    if result_snapshot_id.is_some() {
                        bail!("rejected or cancelled intent operation cannot have result snapshot");
                    }
                    Ok(())
                }
                IntentOperationResolution::Conflicted
                | IntentOperationResolution::PartiallyApplied
                | IntentOperationResolution::Interrupted => {
                    if error_code.is_none() {
                        bail!("non-success intent operation resolution requires typed error");
                    }
                    Ok(())
                }
            },
            Self::ConflictRecorded { conflict, .. } => validate_bounded_text(
                "intent conflict reason",
                &conflict.safe_reason,
                MAX_INTENT_REASON_BYTES,
            ),
            Self::VersionSuperseded {
                previous,
                replacement,
                safe_reason,
                ..
            } => {
                previous.validate()?;
                replacement.validate()?;
                if previous.intent_id != replacement.intent_id
                    || replacement.version <= previous.version
                {
                    bail!("intent supersede must advance the same intent lineage");
                }
                validate_bounded_text(
                    "intent supersede reason",
                    safe_reason,
                    MAX_INTENT_REASON_BYTES,
                )
            }
        }
    }
}

/// Bounded public state for one intent card.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PublicIntentV1 {
    pub intent_ref: IntentVersionRef,
    pub title: String,
    pub statement: String,
    pub acceptance_criteria: Vec<IntentAcceptanceCriterionV1>,
    pub depends_on: Vec<IntentId>,
    pub source: PublicIntentSourceV1,
    pub definition_state: IntentDefinitionState,
    pub application_state: IntentApplicationState,
    pub exclusive_artifact_count: u32,
    pub shared_artifact_count: u32,
    pub unowned_artifact_count: u32,
    pub drifted_artifact_count: u32,
    pub unavailable_artifact_count: u32,
    pub advisory_criterion_count: u32,
    pub system_verified_criterion_count: u32,
    pub artifacts: Vec<PublicIntentArtifactSummaryV1>,
    pub available_actions: Vec<IntentOperationKind>,
}

/// Bounded artifact inspect row for adapter renderers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PublicIntentArtifactSummaryV1 {
    pub artifact_id: IntentArtifactId,
    pub artifact_kind: IntentArtifactKind,
    pub ownership: IntentArtifactOwnership,
    pub availability: IntentArtifactAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_relative_path: Option<String>,
}

/// Bounded public provenance. It cannot carry acceptance or mutation authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PublicIntentSourceV1 {
    UserTurn { source_turn_id: String },
    AcceptedSuggestion { source_turn_id: String },
    TrustedSpec { safe_source_label: String },
}

/// Bounded adapter projection. It contains no raw patch, absolute path, or mutation authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PublicIntentStackV1 {
    pub schema_version: u16,
    pub stack_id: IntentStackId,
    pub stack_version: IntentStackVersion,
    pub authority_state: IntentAuthorityState,
    pub plan_digest: IntentDigest,
    pub intents: Vec<PublicIntentV1>,
    pub conflicts: Vec<IntentConflictV1>,
}

/// Top-level public availability contract for old sessions and active Intent Stacks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum PublicIntentStackStateV1 {
    Available {
        schema_version: u16,
        stack: PublicIntentStackV1,
    },
    HistoryUnavailable {
        schema_version: u16,
        safe_message: String,
    },
}

/// Bounded public operation error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PublicIntentOperationErrorV1 {
    pub schema_version: u16,
    pub code: IntentOperationErrorCode,
    pub safe_message: String,
    pub retryable: bool,
}

fn validate_schema_version(schema_version: u16) -> Result<()> {
    if schema_version != INTENT_CONTRACT_SCHEMA_VERSION {
        bail!("unsupported intent contract schema version {schema_version}");
    }
    Ok(())
}

fn validate_stable_identity(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{label} cannot be empty");
    }
    if value.len() > 128 {
        bail!("{label} is too long");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        bail!("{label} contains unsupported characters");
    }
    Ok(())
}

fn validate_bounded_identity(label: &str, value: &str) -> Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("{label} cannot be empty");
    }
    if trimmed.len() > 256 {
        bail!("{label} is too long");
    }
    if trimmed.chars().any(char::is_control) {
        bail!("{label} contains control characters");
    }
    Ok(())
}

fn validate_bounded_text(label: &str, value: &str, max_bytes: usize) -> Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("{label} cannot be empty");
    }
    if value.len() > max_bytes {
        bail!("{label} exceeds {max_bytes} bytes");
    }
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        bail!("{label} contains unsupported control characters");
    }
    Ok(())
}

fn validate_normalized_relative_path(value: &str) -> Result<()> {
    if value.is_empty()
        || value.starts_with('/')
        || value.contains('\\')
        || value
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        bail!("intent artifact path must be a normalized relative path");
    }
    if value.chars().any(char::is_control) {
        bail!("intent artifact path contains control characters");
    }
    Ok(())
}

fn validate_execution_binding(binding: &IntentExecutionBindingV1) -> Result<()> {
    binding.intent_ref.validate()?;
    validate_bounded_identity("intent execution source event id", &binding.source_event_id)?;
    validate_execution_origin(&binding.origin, false)
}

fn validate_execution_origin(
    origin: &IntentExecutionOriginV1,
    require_attempt: bool,
) -> Result<()> {
    let attempt_id = match origin {
        IntentExecutionOriginV1::Task {
            task_id,
            task_plan_version,
            step_id,
            attempt_id,
        } => {
            validate_bounded_identity("intent task id", task_id)?;
            if *task_plan_version == 0 {
                bail!("intent task plan version must be non-zero");
            }
            validate_bounded_identity("intent task step id", step_id)?;
            if let Some(attempt_id) = attempt_id {
                validate_bounded_identity("intent task attempt id", attempt_id)?;
            }
            attempt_id
        }
        IntentExecutionOriginV1::Chat {
            root_logical_run_id,
            source_turn_id,
            attempt_id,
        } => {
            validate_bounded_identity("intent root logical run id", root_logical_run_id)?;
            validate_bounded_identity("intent chat source turn id", source_turn_id)?;
            if let Some(attempt_id) = attempt_id {
                validate_bounded_identity("intent chat attempt id", attempt_id)?;
            }
            attempt_id
        }
    };
    if require_attempt && attempt_id.is_none() {
        bail!("executable intent layer requires a concrete terminal attempt");
    }
    Ok(())
}

fn validate_intent_source(source: &IntentSourceV1) -> Result<()> {
    match source {
        IntentSourceV1::UserTurn { source_turn_id }
        | IntentSourceV1::AcceptedSuggestion { source_turn_id, .. } => {
            validate_bounded_identity("intent source turn id", source_turn_id)
        }
        IntentSourceV1::TrustedSpec { source_ref, .. } => {
            validate_bounded_identity("intent trusted spec source ref", source_ref)
        }
    }
}

fn validate_dependency_dag(intents: &[IntentDefinitionV1]) -> Result<()> {
    let mut remaining_dependencies = intents
        .iter()
        .map(|intent| {
            (
                intent.intent_ref.intent_id.as_str(),
                intent.depends_on.len(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut dependents = BTreeMap::<&str, Vec<&str>>::new();
    for intent in intents {
        for dependency in &intent.depends_on {
            dependents
                .entry(dependency.as_str())
                .or_default()
                .push(intent.intent_ref.intent_id.as_str());
        }
    }
    let mut ready = remaining_dependencies
        .iter()
        .filter_map(|(intent_id, count)| (*count == 0).then_some(*intent_id))
        .collect::<BTreeSet<_>>();
    let mut visited = 0;
    while let Some(intent_id) = ready.pop_first() {
        visited += 1;
        if let Some(children) = dependents.get(intent_id) {
            for child in children {
                let count = remaining_dependencies
                    .get_mut(child)
                    .context("intent dependency graph references an unknown child")?;
                *count = count.saturating_sub(1);
                if *count == 0 {
                    ready.insert(child);
                }
            }
        }
    }
    if visited != intents.len() {
        bail!("intent dependency graph contains a cycle");
    }
    Ok(())
}

fn validate_proposal_dependency_dag(intents: &[IntentProposalUnitV1]) -> Result<()> {
    let mut remaining_dependencies = intents
        .iter()
        .map(|intent| {
            (
                intent.intent_alias.as_str(),
                intent.depends_on_aliases.len(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut dependents = BTreeMap::<&str, Vec<&str>>::new();
    for intent in intents {
        for dependency in &intent.depends_on_aliases {
            dependents
                .entry(dependency.as_str())
                .or_default()
                .push(intent.intent_alias.as_str());
        }
    }
    let mut ready = remaining_dependencies
        .iter()
        .filter_map(|(intent_id, count)| (*count == 0).then_some(*intent_id))
        .collect::<BTreeSet<_>>();
    let mut visited = 0;
    while let Some(intent_id) = ready.pop_first() {
        visited += 1;
        if let Some(children) = dependents.get(intent_id) {
            for child in children {
                let count = remaining_dependencies
                    .get_mut(child)
                    .context("intent proposal graph references an unknown child")?;
                *count = count.saturating_sub(1);
                if *count == 0 {
                    ready.insert(child);
                }
            }
        }
    }
    if visited != intents.len() {
        bail!("intent proposal dependency graph contains a cycle");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/intent_tests.rs"]
mod tests;
