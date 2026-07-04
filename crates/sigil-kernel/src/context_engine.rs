use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{ArtifactId, EventId, ReceiptId, VerificationVerdict};

mod text;

pub use text::estimate_context_token_cost;
use text::{
    bm25_score, context_snippet_around_terms, term_counts, tokenize_context_text,
    truncate_context_body,
};

pub type ContextItemId = String;
pub type ContextEgressDecisionId = String;
pub type ContextRepoRevision = String;
pub type ContextSourceRef = String;
pub type SessionArchiveEntryId = String;

pub const DEFAULT_SESSION_ARCHIVE_MAX_INDEX_BYTES: usize = 4096;
pub const DEFAULT_CONTEXT_RENDER_SNIPPET_MAX_BYTES: usize = 8 * 1024;
pub const UNKNOWN_CONTEXT_REPO_REVISION: &str = "unknown_revision";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextSource {
    SystemPrompt,
    UserMessage,
    WorkspaceInstruction,
    RepositoryFile,
    ToolObservation,
    McpResource,
    DurableEvent,
    EvidenceReceipt,
    MutationEvidence,
    VerificationEvidence,
    LspSymbol,
    LspDiagnostic,
    LspReference,
    CurrentDiff,
    SessionArchive,
    TaskDigest,
    ExtensionProvided,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ContextTrustLevel {
    System,
    UserProvided,
    WorkspaceInstruction,
    UntrustedRepositoryData,
    ToolObservation,
    ExtensionProvided,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ContextSensitivity {
    Public,
    Repository,
    PotentialSecret,
    Secret,
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ContextInclusionReason {
    StablePrompt,
    UserRequest,
    RecentTurn,
    ActiveFile,
    WorkspaceInstruction,
    VerificationState,
    RetrievalHit,
    ExactSymbolMatch,
    SourcePathMatch,
    WarmLspMatch,
    RequiredEvidence,
    TokenBudget,
    ExcludedUntrustedWorkspace,
    ExcludedSecret,
    ExcludedEgressDenied,
    ExcludedTokenBudget,
    ExcludedUnsupported,
}

impl ContextInclusionReason {
    #[must_use]
    pub fn is_included(&self) -> bool {
        !matches!(
            self,
            Self::ExcludedUntrustedWorkspace
                | Self::ExcludedSecret
                | Self::ExcludedEgressDenied
                | Self::ExcludedTokenBudget
                | Self::ExcludedUnsupported
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextBodyRef {
    Inline {
        content_hash: String,
        byte_len: usize,
    },
    WorkspacePath(PathBuf),
    DurableEvent(EventId),
    Receipt(ReceiptId),
    Artifact(ArtifactId),
}

impl ContextBodyRef {
    #[must_use]
    pub fn inline(body: &str) -> Self {
        Self::Inline {
            content_hash: format!("{:x}", Sha256::digest(body.as_bytes())),
            byte_len: body.len(),
        }
    }
}

/// Validates a model-rendered snippet against the metadata carried by its context item.
///
/// Snippets are provided separately from [`ContextItem`] so runtime layers can keep repository/LSP
/// discovery outside the kernel. This check is the kernel boundary that prevents a caller from
/// declaring a cheap item while rendering a larger or different snippet into provider context.
///
/// # Errors
///
/// Returns an error when the snippet exceeds the render byte cap, exceeds the declared token cost,
/// or contradicts an inline body reference.
pub fn validate_context_render_snippet(
    item: &ContextItem,
    snippet: &str,
    max_bytes: usize,
) -> Result<()> {
    if snippet.len() > max_bytes {
        bail!(
            "context item {} snippet exceeds render byte limit: {} > {}",
            item.id,
            snippet.len(),
            max_bytes
        );
    }

    let rendered_token_cost = estimate_context_token_cost(snippet);
    if rendered_token_cost > item.token_cost {
        bail!(
            "context item {} snippet token cost {} exceeds declared token cost {}",
            item.id,
            rendered_token_cost,
            item.token_cost
        );
    }

    if let ContextBodyRef::Inline {
        content_hash,
        byte_len,
    } = &item.body_ref
    {
        if snippet.len() > *byte_len {
            bail!(
                "context item {} snippet byte length {} exceeds inline body length {}",
                item.id,
                snippet.len(),
                byte_len
            );
        }
        if snippet.len() == *byte_len {
            let rendered_hash = format!("{:x}", Sha256::digest(snippet.as_bytes()));
            if rendered_hash != *content_hash {
                bail!(
                    "context item {} snippet hash does not match inline body ref",
                    item.id
                );
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ContextItem {
    pub id: ContextItemId,
    pub source: ContextSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event_id: Option<EventId>,
    pub trust_level: ContextTrustLevel,
    pub sensitivity: ContextSensitivity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress_decision: Option<ContextEgressDecisionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_revision: Option<ContextRepoRevision>,
    pub token_cost: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub score_breakdown: Vec<ContextScoreComponent>,
    pub inclusion_reason: ContextInclusionReason,
    pub body_ref: ContextBodyRef,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoreComponentKind {
    StableContext,
    RequiredContext,
    ExplicitPath,
    ExactSymbol,
    SourcePath,
    SessionBm25,
    RetrievalScore,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ContextScoreComponent {
    pub kind: ContextScoreComponentKind,
    pub value: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoreMissingReason {
    StableContext,
    RequiredContext,
    SourceProvidedWithoutScore,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextPlacementMissingReason {
    ExcludedFromPrompt,
    RuntimePayloadNotRanked,
}

/// Stable provenance row for Context V0 audit, TUI summary, and quality evidence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ContextProvenanceRowV1 {
    pub item_id: ContextItemId,
    pub source: ContextSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<ContextSourceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub score_breakdown: Vec<ContextScoreComponent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_missing_reason: Option<ContextScoreMissingReason>,
    pub token_cost: usize,
    pub trust_level: ContextTrustLevel,
    pub sensitivity: ContextSensitivity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress_decision: Option<ContextEgressDecisionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_revision: Option<ContextRepoRevision>,
    pub inclusion_reason: ContextInclusionReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why_included: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why_excluded: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<ContextPackPlacement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement_missing_reason: Option<ContextPlacementMissingReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<ContextTruncation>,
}

/// Runtime-selected context candidates for one request assembly pass.
///
/// The kernel owns packing and validation, but it must not own repository scans, LSP startup, or
/// plugin execution. Runtime layers provide already-screened candidates through this neutral
/// container, preserving snippets for model-visible Context V0 rendering.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct RuntimeContextCandidates {
    pub items: Vec<ContextItem>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub snippets: BTreeMap<ContextItemId, String>,
}

impl RuntimeContextCandidates {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.snippets.is_empty()
    }

    pub fn extend(&mut self, mut other: Self) {
        self.items.append(&mut other.items);
        self.snippets.append(&mut other.snippets);
    }
}

impl ContextItem {
    /// Validates trust and egress labels before an item can be attached to a digest.
    ///
    /// # Errors
    ///
    /// Returns an error when a trusted workspace instruction is mislabeled or when an included
    /// secret-like or external item lacks an egress decision.
    pub fn validate(&self) -> Result<()> {
        if self.trust_level == ContextTrustLevel::WorkspaceInstruction
            && self.source != ContextSource::WorkspaceInstruction
        {
            bail!("workspace instruction trust requires workspace instruction source");
        }
        if self.source == ContextSource::WorkspaceInstruction
            && self.trust_level != ContextTrustLevel::WorkspaceInstruction
        {
            bail!("workspace instruction source must carry workspace instruction trust");
        }
        if self.inclusion_reason.is_included()
            && matches!(
                self.sensitivity,
                ContextSensitivity::PotentialSecret | ContextSensitivity::Secret
            )
            && self.egress_decision.is_none()
        {
            bail!("included secret context requires an egress decision");
        }
        if self.inclusion_reason.is_included()
            && self.sensitivity == ContextSensitivity::External
            && self.egress_decision.is_none()
        {
            bail!("included external context requires an egress decision");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ContextTruncation {
    pub original_byte_len: usize,
    pub indexed_byte_len: usize,
    pub truncated: bool,
}

impl ContextTruncation {
    #[must_use]
    pub fn none(byte_len: usize) -> Self {
        Self {
            original_byte_len: byte_len,
            indexed_byte_len: byte_len,
            truncated: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionArchiveEntry {
    pub id: SessionArchiveEntryId,
    pub source: ContextSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event_id: Option<EventId>,
    pub body: String,
    pub trust_level: ContextTrustLevel,
    pub sensitivity: ContextSensitivity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress_decision: Option<ContextEgressDecisionId>,
}

impl SessionArchiveEntry {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        source: ContextSource,
        body: impl Into<String>,
        trust_level: ContextTrustLevel,
        sensitivity: ContextSensitivity,
    ) -> Self {
        Self {
            id: id.into(),
            source,
            source_event_id: None,
            body: body.into(),
            trust_level,
            sensitivity,
            egress_decision: None,
        }
    }

    #[must_use]
    pub fn source_event_id(mut self, source_event_id: impl Into<EventId>) -> Self {
        self.source_event_id = Some(source_event_id.into());
        self
    }

    #[must_use]
    pub fn egress_decision(mut self, egress_decision: impl Into<ContextEgressDecisionId>) -> Self {
        self.egress_decision = Some(egress_decision.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct SessionArchiveSearchHit {
    pub item: ContextItem,
    pub snippet: String,
    pub truncation: ContextTruncation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionArchive {
    pub entries: Vec<SessionArchiveEntry>,
    pub max_index_bytes: usize,
}

impl Default for SessionArchive {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionArchive {
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            max_index_bytes: DEFAULT_SESSION_ARCHIVE_MAX_INDEX_BYTES,
        }
    }

    #[must_use]
    pub fn with_max_index_bytes(mut self, max_index_bytes: usize) -> Self {
        self.max_index_bytes = max_index_bytes.max(1);
        self
    }

    #[must_use]
    pub fn with_entry(mut self, entry: SessionArchiveEntry) -> Self {
        self.entries.push(entry);
        self
    }

    #[must_use]
    pub fn search_bm25(&self, query: &str, limit: usize) -> Vec<SessionArchiveSearchHit> {
        let query_terms = tokenize_context_text(query);
        if query_terms.is_empty() || limit == 0 || self.entries.is_empty() {
            return Vec::new();
        }

        let prepared_docs: Vec<_> = self
            .entries
            .iter()
            .map(|entry| {
                let (indexed_body, truncation) =
                    truncate_context_body(&entry.body, self.max_index_bytes);
                let tokens = tokenize_context_text(&indexed_body);
                let term_counts = term_counts(&tokens);
                (entry, indexed_body, truncation, tokens, term_counts)
            })
            .collect();
        let doc_count = prepared_docs.len() as f32;
        let average_doc_len = prepared_docs
            .iter()
            .map(|(_, _, _, tokens, _)| tokens.len() as f32)
            .sum::<f32>()
            / doc_count.max(1.0);

        let mut document_frequency = BTreeMap::<String, usize>::new();
        for query_term in &query_terms {
            let count = prepared_docs
                .iter()
                .filter(|(_, _, _, _, term_counts)| term_counts.contains_key(query_term))
                .count();
            document_frequency.insert(query_term.clone(), count);
        }

        let mut hits = Vec::new();
        for (entry, indexed_body, truncation, tokens, term_counts) in prepared_docs {
            let score = bm25_score(
                &query_terms,
                &term_counts,
                tokens.len(),
                average_doc_len,
                doc_count,
                &document_frequency,
            );
            if score <= 0.0 {
                continue;
            }
            let inclusion_reason = if matches!(
                entry.sensitivity,
                ContextSensitivity::PotentialSecret | ContextSensitivity::Secret
            ) && entry.egress_decision.is_none()
            {
                ContextInclusionReason::ExcludedSecret
            } else if entry.sensitivity == ContextSensitivity::External
                && entry.egress_decision.is_none()
            {
                ContextInclusionReason::ExcludedEgressDenied
            } else {
                ContextInclusionReason::RetrievalHit
            };
            let item = ContextItem {
                id: format!("session-archive:{}", entry.id),
                source: ContextSource::SessionArchive,
                source_event_id: entry.source_event_id.clone(),
                trust_level: entry.trust_level,
                sensitivity: entry.sensitivity,
                egress_decision: entry.egress_decision.clone(),
                repo_revision: None,
                token_cost: estimate_context_token_cost(&indexed_body),
                score: Some(score),
                score_breakdown: Vec::new(),
                inclusion_reason,
                body_ref: ContextBodyRef::inline(&indexed_body),
            };
            hits.push(SessionArchiveSearchHit {
                item,
                snippet: context_snippet_around_terms(&indexed_body, &query_terms, 160),
                truncation,
            });
        }

        hits.sort_by(|left, right| {
            let score_cmp = right
                .item
                .score
                .unwrap_or_default()
                .partial_cmp(&left.item.score.unwrap_or_default())
                .unwrap_or(Ordering::Equal);
            score_cmp.then_with(|| left.item.id.cmp(&right.item.id))
        });
        hits.truncate(limit);
        hits
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextPackPlacement {
    StablePrefix,
    DynamicSuffix,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ContextPackOptions {
    pub max_tokens: usize,
}

impl ContextPackOptions {
    #[must_use]
    pub fn new(max_tokens: usize) -> Self {
        Self { max_tokens }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct PackedContext {
    pub max_tokens: usize,
    pub used_tokens: usize,
    pub stable_prefix: Vec<ContextItem>,
    pub dynamic_suffix: Vec<ContextItem>,
    pub excluded: Vec<ContextItem>,
}

pub const CONTEXT_QUALITY_EVIDENCE_SCHEMA_VERSION: u16 = 1;
pub const CONTEXT_QUALITY_REPORT_SCHEMA_VERSION: u16 = 2;

/// Stable, developer-facing evidence for one Context V0 retrieval and packing run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ContextQualityEvidencePack {
    pub schema_version: u16,
    pub fixture_id: String,
    pub query: String,
    pub max_tokens: usize,
    pub used_tokens: usize,
    pub token_budget_remaining: usize,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub included_by_source: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub excluded_by_reason: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub included: Vec<ContextQualityItemEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<ContextQualityItemEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<ContextQualityFinding>,
}

/// Stable manifest for one Context V0 quality evidence report directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ContextQualityReportManifest {
    pub report_schema_version: u16,
    pub pack_count: usize,
    pub evidence_jsonl_path: PathBuf,
    pub summary_path: PathBuf,
    pub finding_counts: BTreeMap<String, usize>,
    pub fixture_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matrix_dimensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matrix: Vec<ContextQualityMatrixEntry>,
}

/// Matrix coverage row for one deterministic Context V0 fixture.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ContextQualityMatrixEntry {
    pub fixture_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dimensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub included_sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub finding_kinds: Vec<String>,
}

/// Paths written by [`write_context_quality_evidence_artifacts`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ContextQualityReportArtifacts {
    pub evidence_jsonl_path: PathBuf,
    pub summary_path: PathBuf,
    pub manifest_path: PathBuf,
}

/// One item row in a context quality evidence pack.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ContextQualityItemEvidence {
    pub id: ContextItemId,
    pub source: ContextSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<ContextSourceRef>,
    pub trust_level: ContextTrustLevel,
    pub sensitivity: ContextSensitivity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress_decision: Option<ContextEgressDecisionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_revision: Option<ContextRepoRevision>,
    pub token_cost: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub score_breakdown: Vec<ContextScoreComponent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_missing_reason: Option<ContextScoreMissingReason>,
    pub inclusion_reason: ContextInclusionReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why_included: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why_excluded: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<ContextPackPlacement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement_missing_reason: Option<ContextPlacementMissingReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<ContextTruncation>,
    pub body_ref: ContextBodyRef,
}

/// High-level reason a context quality pack should be inspected.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextQualityFindingKind {
    RecallInsufficient,
    RankingInsufficient,
    TokenBudgetPressure,
    SafetyExclusion,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ContextQualityFinding {
    pub kind: ContextQualityFindingKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub item_ids: Vec<ContextItemId>,
}

impl ContextQualityFinding {
    #[must_use]
    pub fn new(
        kind: ContextQualityFindingKind,
        message: impl Into<String>,
        item_ids: Vec<ContextItemId>,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            item_ids,
        }
    }
}

/// Builds deterministic quality evidence from an already packed Context V0 candidate set.
#[must_use]
pub fn build_context_quality_evidence_pack(
    fixture_id: impl Into<String>,
    query: impl Into<String>,
    packed: &PackedContext,
    truncations: impl IntoIterator<Item = (ContextItemId, ContextTruncation)>,
) -> ContextQualityEvidencePack {
    let truncations = truncations.into_iter().collect::<BTreeMap<_, _>>();
    let mut included = Vec::new();
    let mut excluded = Vec::new();
    let mut included_by_source = BTreeMap::<String, usize>::new();
    let mut excluded_by_reason = BTreeMap::<String, usize>::new();

    for (index, item) in packed.stable_prefix.iter().enumerate() {
        *included_by_source
            .entry(serialized_label(&item.source))
            .or_default() += 1;
        included.push(context_quality_item_row(
            item,
            Some(ContextPackPlacement::StablePrefix),
            Some(index + 1),
            &truncations,
        ));
    }
    let dynamic_offset = included.len();
    for (index, item) in packed.dynamic_suffix.iter().enumerate() {
        *included_by_source
            .entry(serialized_label(&item.source))
            .or_default() += 1;
        included.push(context_quality_item_row(
            item,
            Some(ContextPackPlacement::DynamicSuffix),
            Some(dynamic_offset + index + 1),
            &truncations,
        ));
    }
    for item in &packed.excluded {
        *excluded_by_reason
            .entry(serialized_label(&item.inclusion_reason))
            .or_default() += 1;
        excluded.push(context_quality_item_row(item, None, None, &truncations));
    }

    let findings = context_quality_findings(packed);

    ContextQualityEvidencePack {
        schema_version: CONTEXT_QUALITY_EVIDENCE_SCHEMA_VERSION,
        fixture_id: fixture_id.into(),
        query: query.into(),
        max_tokens: packed.max_tokens,
        used_tokens: packed.used_tokens,
        token_budget_remaining: packed.max_tokens.saturating_sub(packed.used_tokens),
        included_by_source,
        excluded_by_reason,
        included,
        excluded,
        findings,
    }
}

/// Writes Context V0 quality evidence artifacts for developer inspection.
///
/// The JSONL file is the machine-readable source. The Markdown summary is intentionally compact:
/// it supports E06.6 trigger decisions without introducing a user-facing context dashboard.
///
/// # Errors
///
/// Returns an error if the output directory or any report artifact cannot be written.
pub fn write_context_quality_evidence_artifacts(
    output_dir: impl AsRef<Path>,
    packs: &[ContextQualityEvidencePack],
) -> Result<ContextQualityReportArtifacts> {
    let output_dir = output_dir.as_ref();
    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let evidence_jsonl_path = output_dir.join("context-quality.jsonl");
    let mut evidence_file = fs::File::create(&evidence_jsonl_path)
        .with_context(|| format!("failed to create {}", evidence_jsonl_path.display()))?;
    for pack in packs {
        serde_json::to_writer(&mut evidence_file, pack)
            .context("failed to serialize context quality evidence pack")?;
        evidence_file
            .write_all(b"\n")
            .context("failed to write context quality evidence newline")?;
    }

    let summary_path = output_dir.join("summary.md");
    fs::write(&summary_path, render_context_quality_summary(packs))
        .with_context(|| format!("failed to write {}", summary_path.display()))?;

    let manifest_path = output_dir.join("manifest.json");
    let manifest =
        build_context_quality_manifest(packs, evidence_jsonl_path.clone(), summary_path.clone());
    let manifest_file = fs::File::create(&manifest_path)
        .with_context(|| format!("failed to create {}", manifest_path.display()))?;
    serde_json::to_writer_pretty(manifest_file, &manifest)
        .context("failed to serialize context quality manifest")?;

    Ok(ContextQualityReportArtifacts {
        evidence_jsonl_path,
        summary_path,
        manifest_path,
    })
}

fn build_context_quality_manifest(
    packs: &[ContextQualityEvidencePack],
    evidence_jsonl_path: PathBuf,
    summary_path: PathBuf,
) -> ContextQualityReportManifest {
    let mut finding_counts = BTreeMap::<String, usize>::new();
    let mut fixture_ids = Vec::<String>::new();
    let mut matrix_dimensions = BTreeSet::<String>::new();
    let mut matrix = Vec::<ContextQualityMatrixEntry>::new();
    for pack in packs {
        push_unique_string(&mut fixture_ids, pack.fixture_id.clone());
        for finding in &pack.findings {
            *finding_counts
                .entry(serialized_label(&finding.kind))
                .or_default() += 1;
        }
        let entry = context_quality_matrix_entry(pack);
        matrix_dimensions.extend(entry.dimensions.iter().cloned());
        matrix.push(entry);
    }

    ContextQualityReportManifest {
        report_schema_version: CONTEXT_QUALITY_REPORT_SCHEMA_VERSION,
        pack_count: packs.len(),
        evidence_jsonl_path,
        summary_path,
        finding_counts,
        fixture_ids,
        matrix_dimensions: matrix_dimensions.into_iter().collect(),
        matrix,
    }
}

fn render_context_quality_summary(packs: &[ContextQualityEvidencePack]) -> String {
    let mut out = String::new();
    out.push_str("# Sigil Context Quality Evidence\n\n");
    out.push_str(&format!("Total packs: {}\n\n", packs.len()));
    let matrix = packs
        .iter()
        .map(context_quality_matrix_entry)
        .collect::<Vec<_>>();
    let mut dimensions = BTreeSet::<String>::new();
    for entry in &matrix {
        dimensions.extend(entry.dimensions.iter().cloned());
    }
    if !matrix.is_empty() {
        out.push_str("## Matrix Coverage\n\n");
        out.push_str(&format!(
            "- covered finding groups: {}\n",
            dimensions
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
        for entry in &matrix {
            out.push_str(&format!(
                "- {}: {}\n",
                entry.fixture_id,
                if entry.dimensions.is_empty() {
                    "none".to_owned()
                } else {
                    entry.dimensions.join(", ")
                }
            ));
        }
        out.push('\n');
    }
    for pack in packs {
        out.push_str(&format!("## {}\n\n", pack.fixture_id));
        out.push_str(&format!("- query: `{}`\n", pack.query));
        out.push_str(&format!(
            "- budget: {} / {} tokens\n",
            pack.used_tokens, pack.max_tokens
        ));
        out.push_str(&format!("- included: {} items\n", pack.included.len()));
        out.push_str(&format!("- excluded: {} items\n", pack.excluded.len()));
        if pack.findings.is_empty() {
            out.push_str("- findings: none\n\n");
        } else {
            out.push_str("- findings:\n");
            for finding in &pack.findings {
                out.push_str(&format!("  - {:?}: {}\n", finding.kind, finding.message));
            }
            out.push('\n');
        }
    }
    out
}

fn context_quality_matrix_entry(pack: &ContextQualityEvidencePack) -> ContextQualityMatrixEntry {
    let included_sources = pack.included_by_source.keys().cloned().collect::<Vec<_>>();
    let excluded_reasons = pack.excluded_by_reason.keys().cloned().collect::<Vec<_>>();
    let finding_kinds = pack
        .findings
        .iter()
        .map(|finding| serialized_label(&finding.kind))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut dimensions = BTreeSet::<String>::new();
    if !pack.included.is_empty() || !pack.excluded.is_empty() {
        dimensions.insert("recall".to_owned());
    }
    if pack
        .included
        .iter()
        .chain(pack.excluded.iter())
        .any(|item| item.score.is_some() || !item.score_breakdown.is_empty())
    {
        dimensions.insert("ranking".to_owned());
    }
    for finding in &pack.findings {
        match finding.kind {
            ContextQualityFindingKind::RecallInsufficient => {
                dimensions.insert("recall".to_owned());
            }
            ContextQualityFindingKind::RankingInsufficient => {
                dimensions.insert("ranking".to_owned());
            }
            ContextQualityFindingKind::TokenBudgetPressure => {
                dimensions.insert("budget".to_owned());
            }
            ContextQualityFindingKind::SafetyExclusion => {
                dimensions.insert("safety".to_owned());
            }
        }
    }
    for reason in pack.excluded_by_reason.keys() {
        match reason.as_str() {
            "excluded_secret" => {
                dimensions.insert("safety".to_owned());
            }
            "excluded_untrusted_workspace" => {
                dimensions.insert("trust".to_owned());
            }
            "excluded_egress_denied" => {
                dimensions.insert("egress".to_owned());
            }
            "excluded_token_budget" => {
                dimensions.insert("budget".to_owned());
            }
            _ => {}
        }
    }
    for source in pack.included_by_source.keys() {
        if matches!(
            source.as_str(),
            "task_digest" | "session_archive" | "evidence_receipt" | "verification_evidence"
        ) {
            dimensions.insert("memory_evidence_boundary".to_owned());
        }
    }
    for item in &pack.excluded {
        let source = serialized_label(&item.source);
        if matches!(
            source.as_str(),
            "task_digest" | "session_archive" | "evidence_receipt" | "verification_evidence"
        ) {
            dimensions.insert("memory_evidence_boundary".to_owned());
        }
    }
    ContextQualityMatrixEntry {
        fixture_id: pack.fixture_id.clone(),
        dimensions: dimensions.into_iter().collect(),
        included_sources,
        excluded_reasons,
        finding_kinds,
    }
}

fn push_unique_string(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

#[must_use]
pub fn context_provenance_row_v1(
    item: &ContextItem,
    placement: Option<ContextPackPlacement>,
    rank: Option<usize>,
    truncation: Option<ContextTruncation>,
) -> ContextProvenanceRowV1 {
    ContextProvenanceRowV1 {
        item_id: item.id.clone(),
        source: item.source.clone(),
        source_ref: context_source_ref(item),
        score: item.score,
        score_breakdown: context_score_breakdown(item),
        score_missing_reason: context_score_missing_reason(item, placement),
        token_cost: item.token_cost,
        trust_level: item.trust_level,
        sensitivity: item.sensitivity,
        egress_decision: item.egress_decision.clone(),
        repo_revision: context_repo_revision(item),
        inclusion_reason: item.inclusion_reason.clone(),
        why_included: item
            .inclusion_reason
            .is_included()
            .then(|| serialized_label(&item.inclusion_reason)),
        why_excluded: (!item.inclusion_reason.is_included())
            .then(|| serialized_label(&item.inclusion_reason)),
        placement,
        rank,
        placement_missing_reason: context_placement_missing_reason(item, placement, rank),
        truncation,
    }
}

fn context_source_ref(item: &ContextItem) -> Option<ContextSourceRef> {
    Some(match &item.body_ref {
        ContextBodyRef::Inline {
            content_hash,
            byte_len,
        } => format!("inline:{content_hash}:{byte_len}"),
        ContextBodyRef::WorkspacePath(path) => format!("workspace:{}", path.display()),
        ContextBodyRef::DurableEvent(event_id) => format!("event:{event_id}"),
        ContextBodyRef::Receipt(receipt_id) => format!("receipt:{receipt_id}"),
        ContextBodyRef::Artifact(artifact_id) => format!("artifact:{artifact_id}"),
    })
}

fn context_score_breakdown(item: &ContextItem) -> Vec<ContextScoreComponent> {
    if !item.score_breakdown.is_empty() {
        return item.score_breakdown.clone();
    }
    item.score
        .map(|value| ContextScoreComponent {
            kind: context_score_component_kind(item),
            value,
        })
        .into_iter()
        .collect()
}

fn context_score_component_kind(item: &ContextItem) -> ContextScoreComponentKind {
    match item.inclusion_reason {
        ContextInclusionReason::StablePrompt => ContextScoreComponentKind::StableContext,
        ContextInclusionReason::UserRequest
        | ContextInclusionReason::RecentTurn
        | ContextInclusionReason::ActiveFile
        | ContextInclusionReason::WorkspaceInstruction
        | ContextInclusionReason::VerificationState
        | ContextInclusionReason::RequiredEvidence => ContextScoreComponentKind::RequiredContext,
        ContextInclusionReason::ExactSymbolMatch => ContextScoreComponentKind::ExactSymbol,
        ContextInclusionReason::SourcePathMatch => ContextScoreComponentKind::SourcePath,
        ContextInclusionReason::RetrievalHit => match item.source {
            ContextSource::SessionArchive => ContextScoreComponentKind::SessionBm25,
            ContextSource::RepositoryFile
            | ContextSource::McpResource
            | ContextSource::LspSymbol
            | ContextSource::LspDiagnostic
            | ContextSource::LspReference
            | ContextSource::CurrentDiff => ContextScoreComponentKind::RetrievalScore,
            _ => ContextScoreComponentKind::Other,
        },
        ContextInclusionReason::WarmLspMatch => ContextScoreComponentKind::RetrievalScore,
        ContextInclusionReason::TokenBudget
        | ContextInclusionReason::ExcludedUntrustedWorkspace
        | ContextInclusionReason::ExcludedSecret
        | ContextInclusionReason::ExcludedEgressDenied
        | ContextInclusionReason::ExcludedTokenBudget
        | ContextInclusionReason::ExcludedUnsupported => ContextScoreComponentKind::Other,
    }
}

fn context_score_missing_reason(
    item: &ContextItem,
    placement: Option<ContextPackPlacement>,
) -> Option<ContextScoreMissingReason> {
    if item.score.is_some() {
        return None;
    }
    if placement == Some(ContextPackPlacement::StablePrefix)
        || item.inclusion_reason == ContextInclusionReason::StablePrompt
    {
        return Some(ContextScoreMissingReason::StableContext);
    }
    if matches!(
        item.inclusion_reason,
        ContextInclusionReason::UserRequest
            | ContextInclusionReason::RecentTurn
            | ContextInclusionReason::ActiveFile
            | ContextInclusionReason::WorkspaceInstruction
            | ContextInclusionReason::VerificationState
            | ContextInclusionReason::RequiredEvidence
    ) {
        return Some(ContextScoreMissingReason::RequiredContext);
    }
    Some(ContextScoreMissingReason::SourceProvidedWithoutScore)
}

fn context_repo_revision(item: &ContextItem) -> Option<ContextRepoRevision> {
    item.repo_revision.clone().or_else(|| {
        context_source_prefers_repo_revision(&item.source)
            .then(|| UNKNOWN_CONTEXT_REPO_REVISION.to_owned())
    })
}

fn context_source_prefers_repo_revision(source: &ContextSource) -> bool {
    matches!(
        source,
        ContextSource::RepositoryFile
            | ContextSource::LspSymbol
            | ContextSource::LspDiagnostic
            | ContextSource::LspReference
            | ContextSource::CurrentDiff
    )
}

fn context_placement_missing_reason(
    item: &ContextItem,
    placement: Option<ContextPackPlacement>,
    rank: Option<usize>,
) -> Option<ContextPlacementMissingReason> {
    if placement.is_some() && rank.is_some() {
        return None;
    }
    if !item.inclusion_reason.is_included() {
        return Some(ContextPlacementMissingReason::ExcludedFromPrompt);
    }
    Some(ContextPlacementMissingReason::RuntimePayloadNotRanked)
}

fn context_quality_item_row(
    item: &ContextItem,
    placement: Option<ContextPackPlacement>,
    rank: Option<usize>,
    truncations: &BTreeMap<ContextItemId, ContextTruncation>,
) -> ContextQualityItemEvidence {
    let provenance =
        context_provenance_row_v1(item, placement, rank, truncations.get(&item.id).cloned());
    ContextQualityItemEvidence {
        id: provenance.item_id,
        source: provenance.source,
        source_ref: provenance.source_ref,
        trust_level: provenance.trust_level,
        sensitivity: provenance.sensitivity,
        egress_decision: provenance.egress_decision,
        repo_revision: provenance.repo_revision,
        token_cost: provenance.token_cost,
        score: provenance.score,
        score_breakdown: provenance.score_breakdown,
        score_missing_reason: provenance.score_missing_reason,
        inclusion_reason: provenance.inclusion_reason,
        why_included: provenance.why_included,
        why_excluded: provenance.why_excluded,
        placement: provenance.placement,
        rank: provenance.rank,
        placement_missing_reason: provenance.placement_missing_reason,
        truncation: provenance.truncation,
        body_ref: item.body_ref.clone(),
    }
}

fn context_quality_findings(packed: &PackedContext) -> Vec<ContextQualityFinding> {
    let mut findings = Vec::new();
    if packed.stable_prefix.is_empty()
        && packed.dynamic_suffix.is_empty()
        && packed.excluded.is_empty()
    {
        findings.push(ContextQualityFinding::new(
            ContextQualityFindingKind::RecallInsufficient,
            "no context candidates were recalled for this query",
            Vec::new(),
        ));
    } else if packed.stable_prefix.is_empty() && packed.dynamic_suffix.is_empty() {
        findings.push(ContextQualityFinding::new(
            ContextQualityFindingKind::RecallInsufficient,
            "all recalled context candidates were excluded before prompt assembly",
            packed.excluded.iter().map(|item| item.id.clone()).collect(),
        ));
    }

    let missing_scores = packed
        .dynamic_suffix
        .iter()
        .filter(|item| item.score.is_none())
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    if !missing_scores.is_empty() {
        findings.push(ContextQualityFinding::new(
            ContextQualityFindingKind::RankingInsufficient,
            "one or more dynamic context items have no retrieval score",
            missing_scores,
        ));
    }

    let budget_excluded = packed
        .excluded
        .iter()
        .filter(|item| item.inclusion_reason == ContextInclusionReason::ExcludedTokenBudget)
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    if !budget_excluded.is_empty() {
        findings.push(ContextQualityFinding::new(
            ContextQualityFindingKind::TokenBudgetPressure,
            "token budget excluded otherwise eligible context candidates",
            budget_excluded,
        ));
    }

    let safety_excluded = packed
        .excluded
        .iter()
        .filter(|item| {
            matches!(
                item.inclusion_reason,
                ContextInclusionReason::ExcludedSecret
                    | ContextInclusionReason::ExcludedEgressDenied
                    | ContextInclusionReason::ExcludedUntrustedWorkspace
            )
        })
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    if !safety_excluded.is_empty() {
        findings.push(ContextQualityFinding::new(
            ContextQualityFindingKind::SafetyExclusion,
            "safety policy excluded context from provider assembly",
            safety_excluded,
        ));
    }

    findings
}

fn serialized_label<T>(value: &T) -> String
where
    T: Serialize + std::fmt::Debug,
{
    match serde_json::to_value(value) {
        Ok(Value::String(label)) => label,
        _ => format!("{value:?}"),
    }
}

/// Packs context deterministically into stable-prefix and dynamic-suffix sections.
///
/// Stable prefix items are selected before dynamic retrieval hits to preserve provider prompt-cache
/// friendliness. Dynamic items are ordered by score descending and then stable id.
///
/// # Errors
///
/// Returns an error when an included item has invalid trust or egress labels after normalization.
pub fn pack_context_items(
    items: impl IntoIterator<Item = ContextItem>,
    options: ContextPackOptions,
) -> Result<PackedContext> {
    let mut stable_candidates = Vec::new();
    let mut dynamic_candidates = Vec::new();
    let mut excluded = Vec::new();

    for item in items {
        let item = normalize_context_item_for_pack(item);
        if !item.inclusion_reason.is_included() {
            excluded.push(item);
            continue;
        }
        item.validate()?;
        if context_pack_placement(&item) == ContextPackPlacement::StablePrefix {
            stable_candidates.push(item);
        } else {
            dynamic_candidates.push(item);
        }
    }

    stable_candidates.sort_by(stable_context_order);
    dynamic_candidates.sort_by(dynamic_context_order);

    let mut used_tokens = 0usize;
    let mut stable_prefix = Vec::new();
    let mut dynamic_suffix = Vec::new();
    for item in stable_candidates {
        pack_candidate(
            item,
            options.max_tokens,
            &mut used_tokens,
            &mut stable_prefix,
            &mut excluded,
        );
    }
    for item in dynamic_candidates {
        pack_candidate(
            item,
            options.max_tokens,
            &mut used_tokens,
            &mut dynamic_suffix,
            &mut excluded,
        );
    }

    excluded.sort_by(|left, right| left.id.cmp(&right.id));

    Ok(PackedContext {
        max_tokens: options.max_tokens,
        used_tokens,
        stable_prefix,
        dynamic_suffix,
        excluded,
    })
}

fn pack_candidate(
    mut item: ContextItem,
    max_tokens: usize,
    used_tokens: &mut usize,
    included: &mut Vec<ContextItem>,
    excluded: &mut Vec<ContextItem>,
) {
    if item.token_cost > max_tokens.saturating_sub(*used_tokens) {
        item.inclusion_reason = ContextInclusionReason::ExcludedTokenBudget;
        excluded.push(item);
        return;
    }
    *used_tokens += item.token_cost;
    included.push(item);
}

fn normalize_context_item_for_pack(mut item: ContextItem) -> ContextItem {
    if item.inclusion_reason.is_included()
        && matches!(
            item.sensitivity,
            ContextSensitivity::PotentialSecret | ContextSensitivity::Secret
        )
        && item.egress_decision.is_none()
    {
        item.inclusion_reason = ContextInclusionReason::ExcludedSecret;
    } else if item.inclusion_reason.is_included()
        && item.sensitivity == ContextSensitivity::External
        && item.egress_decision.is_none()
    {
        item.inclusion_reason = ContextInclusionReason::ExcludedEgressDenied;
    }
    item
}

fn context_pack_placement(item: &ContextItem) -> ContextPackPlacement {
    match (&item.source, &item.inclusion_reason) {
        (ContextSource::SystemPrompt, _) | (_, ContextInclusionReason::StablePrompt) => {
            ContextPackPlacement::StablePrefix
        }
        (ContextSource::WorkspaceInstruction, _)
        | (_, ContextInclusionReason::WorkspaceInstruction) => ContextPackPlacement::StablePrefix,
        _ => ContextPackPlacement::DynamicSuffix,
    }
}

fn stable_context_order(left: &ContextItem, right: &ContextItem) -> Ordering {
    stable_context_priority(left)
        .cmp(&stable_context_priority(right))
        .then_with(|| left.id.cmp(&right.id))
}

fn stable_context_priority(item: &ContextItem) -> u8 {
    match item.source {
        ContextSource::SystemPrompt => 0,
        ContextSource::WorkspaceInstruction => 1,
        _ => 2,
    }
}

fn dynamic_context_order(left: &ContextItem, right: &ContextItem) -> Ordering {
    let priority_cmp = dynamic_context_priority(left).cmp(&dynamic_context_priority(right));
    if priority_cmp != Ordering::Equal {
        return priority_cmp;
    }
    let score_cmp = right
        .score
        .unwrap_or_default()
        .partial_cmp(&left.score.unwrap_or_default())
        .unwrap_or(Ordering::Equal);
    score_cmp.then_with(|| left.id.cmp(&right.id))
}

fn dynamic_context_priority(item: &ContextItem) -> u8 {
    if item
        .score_breakdown
        .iter()
        .any(|component| component.kind == ContextScoreComponentKind::ExplicitPath)
    {
        return 0;
    }
    if item.inclusion_reason == ContextInclusionReason::ExactSymbolMatch
        || item
            .score_breakdown
            .iter()
            .any(|component| component.kind == ContextScoreComponentKind::ExactSymbol)
    {
        return 1;
    }
    if item.inclusion_reason == ContextInclusionReason::SourcePathMatch
        || item
            .score_breakdown
            .iter()
            .any(|component| component.kind == ContextScoreComponentKind::SourcePath)
    {
        return 2;
    }
    3
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextDigestTextKind {
    UserProvided,
    SystemDerived,
    ModelInferred,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ContextDigestText {
    pub text: String,
    pub kind: ContextDigestTextKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_receipt_id: Option<ReceiptId>,
}

impl ContextDigestText {
    #[must_use]
    pub fn user_provided(text: impl Into<String>, source_event_id: impl Into<EventId>) -> Self {
        Self {
            text: text.into(),
            kind: ContextDigestTextKind::UserProvided,
            source_event_id: Some(source_event_id.into()),
            source_receipt_id: None,
        }
    }

    #[must_use]
    pub fn system_derived(text: impl Into<String>, source_event_id: impl Into<EventId>) -> Self {
        Self {
            text: text.into(),
            kind: ContextDigestTextKind::SystemDerived,
            source_event_id: Some(source_event_id.into()),
            source_receipt_id: None,
        }
    }

    #[must_use]
    pub fn model_inferred(text: impl Into<String>, source_event_id: impl Into<EventId>) -> Self {
        Self {
            text: text.into(),
            kind: ContextDigestTextKind::ModelInferred,
            source_event_id: Some(source_event_id.into()),
            source_receipt_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ContextDigestV0 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<ContextDigestText>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_files: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_commands: Vec<ReceiptId>,
    pub verification_state: VerificationVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_receipt_id: Option<ReceiptId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<ContextDigestText>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_items: Vec<ContextItem>,
}

#[derive(Debug, Clone)]
pub struct ContextDigestV0Builder {
    objective: Option<ContextDigestText>,
    active_files: BTreeSet<PathBuf>,
    recent_commands: Vec<ReceiptId>,
    verification_state: VerificationVerdict,
    verification_receipt_id: Option<ReceiptId>,
    unresolved: Vec<ContextDigestText>,
    context_items: Vec<ContextItem>,
}

impl Default for ContextDigestV0Builder {
    fn default() -> Self {
        Self {
            objective: None,
            active_files: BTreeSet::new(),
            recent_commands: Vec::new(),
            verification_state: VerificationVerdict::NotEvaluated,
            verification_receipt_id: None,
            unresolved: Vec::new(),
            context_items: Vec::new(),
        }
    }
}

impl ContextDigestV0Builder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn objective(mut self, objective: ContextDigestText) -> Self {
        self.objective = Some(objective);
        self
    }

    #[must_use]
    pub fn active_file(mut self, path: impl AsRef<Path>) -> Self {
        self.active_files.insert(path.as_ref().to_path_buf());
        self
    }

    #[must_use]
    pub fn recent_command(mut self, receipt_id: impl Into<ReceiptId>) -> Self {
        let receipt_id = receipt_id.into();
        if !self.recent_commands.contains(&receipt_id) {
            self.recent_commands.push(receipt_id);
        }
        self
    }

    #[must_use]
    pub fn verification_state(
        mut self,
        verdict: VerificationVerdict,
        receipt_id: Option<ReceiptId>,
    ) -> Self {
        self.verification_state = verdict;
        self.verification_receipt_id = receipt_id;
        self
    }

    #[must_use]
    pub fn unresolved(mut self, item: ContextDigestText) -> Self {
        self.unresolved.push(item);
        self
    }

    pub fn context_item(mut self, item: ContextItem) -> Result<Self> {
        item.validate()?;
        self.context_items.push(item);
        Ok(self)
    }

    /// Builds a deterministic digest.
    ///
    /// # Errors
    ///
    /// Returns an error if the digest would claim passed verification without an existing
    /// verification receipt or if one attached context item has invalid trust/egress labels.
    pub fn build(self) -> Result<ContextDigestV0> {
        if self.verification_state == VerificationVerdict::Passed
            && self.verification_receipt_id.is_none()
        {
            bail!("context digest cannot claim passed verification without a receipt reference");
        }
        for item in &self.context_items {
            item.validate()?;
        }

        Ok(ContextDigestV0 {
            objective: self.objective,
            active_files: self.active_files.into_iter().collect(),
            recent_commands: self.recent_commands,
            verification_state: self.verification_state,
            verification_receipt_id: self.verification_receipt_id,
            unresolved: self.unresolved,
            context_items: self.context_items,
        })
    }
}

#[cfg(test)]
#[path = "tests/context_engine_tests.rs"]
mod tests;
