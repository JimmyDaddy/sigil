use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ArtifactId, EventId, ReceiptId, VerificationVerdict};

pub type ContextItemId = String;
pub type ContextEgressDecisionId = String;
pub type ContextRepoRevision = String;
pub type SessionArchiveEntryId = String;

pub const DEFAULT_SESSION_ARCHIVE_MAX_INDEX_BYTES: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextSource {
    SystemPrompt,
    UserMessage,
    WorkspaceInstruction,
    RepositoryFile,
    ToolObservation,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextInclusionReason {
    StablePrompt,
    UserRequest,
    RecentTurn,
    ActiveFile,
    WorkspaceInstruction,
    VerificationState,
    RetrievalHit,
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
    pub inclusion_reason: ContextInclusionReason,
    pub body_ref: ContextBodyRef,
}

impl ContextItem {
    /// Validates trust and egress labels before an item can be attached to a digest.
    ///
    /// # Errors
    ///
    /// Returns an error when a trusted workspace instruction is mislabeled or when an included
    /// secret-like item lacks an egress decision.
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
            && self.sensitivity == ContextSensitivity::Secret
            && self.egress_decision.is_none()
        {
            bail!("included secret context requires an egress decision");
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
            let inclusion_reason = if entry.sensitivity == ContextSensitivity::Secret
                && entry.egress_decision.is_none()
            {
                ContextInclusionReason::ExcludedSecret
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
                inclusion_reason,
                body_ref: ContextBodyRef::inline(&indexed_body),
            };
            hits.push(SessionArchiveSearchHit {
                item,
                snippet: context_snippet(&indexed_body, 160),
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
        && item.sensitivity == ContextSensitivity::Secret
        && item.egress_decision.is_none()
    {
        item.inclusion_reason = ContextInclusionReason::ExcludedSecret;
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
    let score_cmp = right
        .score
        .unwrap_or_default()
        .partial_cmp(&left.score.unwrap_or_default())
        .unwrap_or(Ordering::Equal);
    score_cmp.then_with(|| left.id.cmp(&right.id))
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

pub fn estimate_context_token_cost(text: &str) -> usize {
    text.split_whitespace().count().max(1)
}

fn tokenize_context_text(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch.to_ascii_lowercase());
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn term_counts(tokens: &[String]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for token in tokens {
        *counts.entry(token.clone()).or_default() += 1;
    }
    counts
}

fn bm25_score(
    query_terms: &[String],
    term_counts: &BTreeMap<String, usize>,
    doc_len: usize,
    average_doc_len: f32,
    doc_count: f32,
    document_frequency: &BTreeMap<String, usize>,
) -> f32 {
    const K1: f32 = 1.2;
    const B: f32 = 0.75;

    let doc_len = doc_len.max(1) as f32;
    let average_doc_len = average_doc_len.max(1.0);
    let mut score = 0.0;
    for term in query_terms {
        let Some(term_frequency) = term_counts.get(term).copied() else {
            continue;
        };
        let document_frequency = document_frequency.get(term).copied().unwrap_or_default() as f32;
        let idf = ((doc_count - document_frequency + 0.5) / (document_frequency + 0.5) + 1.0).ln();
        let term_frequency = term_frequency as f32;
        let denominator = term_frequency + K1 * (1.0 - B + B * (doc_len / average_doc_len));
        score += idf * (term_frequency * (K1 + 1.0)) / denominator;
    }
    score
}

fn truncate_context_body(body: &str, max_bytes: usize) -> (String, ContextTruncation) {
    if body.len() <= max_bytes {
        return (body.to_owned(), ContextTruncation::none(body.len()));
    }

    let mut end = max_bytes.min(body.len());
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    let indexed_body = body[..end].to_owned();
    (
        indexed_body,
        ContextTruncation {
            original_byte_len: body.len(),
            indexed_byte_len: end,
            truncated: true,
        },
    )
}

fn context_snippet(body: &str, max_chars: usize) -> String {
    let mut snippet = String::new();
    for ch in body.chars().take(max_chars) {
        snippet.push(ch);
    }
    if body.chars().count() > max_chars {
        snippet.push_str("...");
    }
    snippet
}

#[cfg(test)]
#[path = "tests/context_engine_tests.rs"]
mod tests;
