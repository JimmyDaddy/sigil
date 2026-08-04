use std::{
    collections::{BTreeSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use aho_corasick::AhoCorasick;
use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{JsonlSessionStore, session_id_for_path};
use crate::{
    MessageRole, ModelMessage, ToolError, ToolResult, safe_persistence_json_value,
    safe_persistence_text, stable_event_hash, tool::PreCapturedToolArtifact,
};

pub const TOOL_ARTIFACT_DESCRIPTOR_SCHEMA_VERSION: u16 = 1;
pub const TOOL_RESULT_RECORDED_SCHEMA_VERSION: u16 = 2;
/// RFC-0062 9.7: durable schema written by all new tool executions.
pub const TOOL_RESULT_RECORDED_V3_SCHEMA_VERSION: u16 = 3;
pub const TOOL_ARTIFACT_AVAILABILITY_CHANGED_SCHEMA_VERSION: u16 = 1;
pub const TOOL_EXECUTION_CAPTURE_PLAN_SCHEMA_VERSION: u16 = 1;
pub const TOOL_RESULT_TERMINAL_FALLBACK_SCHEMA_VERSION: u16 = 1;
pub const TOOL_ARTIFACT_READ_SCHEMA_VERSION: u16 = 1;
pub const TOOL_MODEL_VIEW_SCHEMA_VERSION: u16 = 1;
pub const TOOL_ARTIFACT_MAX_BYTES: usize = 16 * 1024 * 1024;
pub const TOOL_ARTIFACT_SESSION_BUDGET_BYTES: u64 = 256 * 1024 * 1024;
pub const TOOL_RESULT_EVENT_TARGET_BYTES: usize = 64 * 1024;
/// Inline adapters must use a policy-safe streaming sink before returning larger bodies.
pub const TOOL_RESULT_INLINE_CAPTURE_MAX_BYTES: usize = 256 * 1024;
pub const TOOL_MODEL_VIEW_MAX_BYTES: usize = 32 * 1024;
/// Maximum initial provider-visible preview for one ordinary tool result.
pub const TOOL_MODEL_VIEW_INITIAL_MAX_BYTES: usize = TOOL_MODEL_VIEW_MAX_BYTES / 2;
/// Smaller initial preview for tools whose results are commonly paged or searched.
pub const TOOL_MODEL_VIEW_HIGH_VOLUME_MAX_BYTES: usize = 8 * 1024;
pub const TOOL_DISPLAY_VIEW_MAX_BYTES: usize = 32 * 1024;
pub const TOOL_ARTIFACT_READ_MAX_BYTES: u32 = 16 * 1024;
pub const TOOL_ARTIFACT_READ_MAX_LINES: u32 = 200;
pub const TOOL_ARTIFACT_SEARCH_MAX_MATCHES: u16 = 20;
pub const TOOL_ARTIFACT_SEARCH_MAX_CONTEXT_LINES: u16 = 3;
pub const TOOL_ARTIFACT_READS_PER_TURN: u16 = 8;
pub const TOOL_ARTIFACT_READ_BYTES_PER_TURN: u64 = 64 * 1024;
pub const TOOL_ARTIFACT_ORPHAN_GRACE_MS: u64 = 24 * 60 * 60 * 1_000;

const TOOL_RESULT_FACTS_MAX_BYTES: usize = 8 * 1024;
/// RFC-0062 9.5: bounded error summaries never clone large bodies into facts or messages.
pub const TOOL_RESULT_ERROR_SUMMARY_MAX_BYTES: usize = 1024;
const TOOL_RESULT_FACT_PATH_LIMIT: usize = 128;
const TOOL_RESULT_FACT_PATH_MAX_BYTES: usize = 1024;
const TOOL_ARTIFACT_REF_PREFIX: &str = "ta1_";
const TOOL_ARTIFACT_MANIFEST_MAX_ENTRIES: usize = 100_000;

pub type ToolArtifactId = String;

/// Opaque, non-path reference for one session-scoped tool output artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolArtifactRefV1 {
    pub artifact_id: ToolArtifactId,
}

impl ToolArtifactRefV1 {
    fn random() -> Self {
        Self {
            artifact_id: format!("{TOOL_ARTIFACT_REF_PREFIX}{}", Uuid::new_v4().simple()),
        }
    }

    pub fn validate(&self) -> Result<()> {
        let suffix = self
            .artifact_id
            .strip_prefix(TOOL_ARTIFACT_REF_PREFIX)
            .context("tool artifact ref has an unsupported version")?;
        if suffix.len() != 32 || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("tool artifact ref is malformed");
        }
        Ok(())
    }
}

/// Encoding of the immutable persisted artifact bytes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolArtifactEncoding {
    Utf8,
    Binary,
}

/// Bounded extent retained when an output exceeds the artifact hard limit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolArtifactTruncationV1 {
    pub omitted_bytes: u64,
    pub retained_head_bytes: u64,
    pub retained_tail_bytes: u64,
}

/// Truthful completeness of the immutable persisted bytes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ToolArtifactCompleteness {
    Complete,
    PolicyRedacted {
        redaction_count: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        storage_truncation: Option<ToolArtifactTruncationV1>,
    },
    StorageTruncated(ToolArtifactTruncationV1),
    EphemeralUnavailableAfterRestart,
}

/// Persistence sensitivity does not grant authority or change provenance.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolArtifactSensitivity {
    Ordinary,
    SensitiveLocal,
    ExternalUntrusted,
}

/// Retention owner for a tool output artifact.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolArtifactRetentionClass {
    SessionBound,
    Pinned,
    Ephemeral,
}

/// Surfaces allowed to resolve an artifact ref.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolArtifactRetrievalPolicyV1 {
    ModelAndDisplay,
    DisplayOnly,
    Unavailable,
}

/// Explicit durable binding avoids conflating an unavailable artifact with an omitted field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ToolArtifactBindingV1 {
    Published {
        descriptor: ToolArtifactDescriptorV1,
    },
    Unavailable {
        unavailable: ToolArtifactUnavailableV1,
    },
}

impl ToolArtifactBindingV1 {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Published { descriptor } => descriptor.validate(),
            Self::Unavailable { unavailable } => unavailable.validate(),
        }
    }

    #[must_use]
    pub fn descriptor(&self) -> Option<&ToolArtifactDescriptorV1> {
        match self {
            Self::Published { descriptor } => Some(descriptor),
            Self::Unavailable { .. } => None,
        }
    }
}

/// Bounded durable explanation for a result whose raw body is not retrievable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolArtifactUnavailableV1 {
    pub availability: ToolArtifactAvailability,
    pub observed_bytes: u64,
    pub reason: String,
}

impl ToolArtifactUnavailableV1 {
    pub fn validate(&self) -> Result<()> {
        if self.availability == ToolArtifactAvailability::Available
            || self.reason.trim().is_empty()
            || self.reason.len() > 512
        {
            bail!("tool artifact unavailable binding is malformed");
        }
        Ok(())
    }
}

/// Durable, body-free identity and lifecycle metadata for one tool output artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolArtifactDescriptorV1 {
    pub schema_version: u16,
    pub artifact_ref: ToolArtifactRefV1,
    pub session_scope_id_hash: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub content_sha256: String,
    pub observed_bytes: u64,
    /// Byte length after persistence-policy redaction and before storage truncation.
    pub policy_projected_bytes: u64,
    pub persisted_bytes: u64,
    pub media_type: String,
    pub encoding: ToolArtifactEncoding,
    pub completeness: ToolArtifactCompleteness,
    pub sensitivity: ToolArtifactSensitivity,
    pub retention_class: ToolArtifactRetentionClass,
    pub retrieval_policy: ToolArtifactRetrievalPolicyV1,
}

impl ToolArtifactDescriptorV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != TOOL_ARTIFACT_DESCRIPTOR_SCHEMA_VERSION {
            bail!("tool artifact descriptor has an unsupported schema version");
        }
        self.artifact_ref.validate()?;
        if self.session_scope_id_hash.len() != 71
            || !self.session_scope_id_hash.starts_with("sha256:")
            || self.tool_call_id.trim().is_empty()
            || self.tool_name.trim().is_empty()
            || self.content_sha256.len() != 71
            || !self.content_sha256.starts_with("sha256:")
            || self.persisted_bytes > TOOL_ARTIFACT_MAX_BYTES as u64
            || self.media_type.trim().is_empty()
        {
            bail!("tool artifact descriptor is malformed");
        }
        match &self.completeness {
            ToolArtifactCompleteness::Complete => {
                if self.observed_bytes != self.policy_projected_bytes
                    || self.policy_projected_bytes != self.persisted_bytes
                {
                    bail!("complete tool artifact byte counts do not match");
                }
            }
            ToolArtifactCompleteness::PolicyRedacted {
                redaction_count,
                storage_truncation,
            } => {
                if *redaction_count == 0 {
                    bail!("redacted tool artifact must report at least one redaction");
                }
                if let Some(truncation) = storage_truncation {
                    validate_truncation(self, truncation)?;
                } else if self.policy_projected_bytes != self.persisted_bytes {
                    bail!("untruncated redacted tool artifact byte counts do not match");
                }
            }
            ToolArtifactCompleteness::StorageTruncated(truncation) => {
                validate_truncation(self, truncation)?;
            }
            ToolArtifactCompleteness::EphemeralUnavailableAfterRestart => {
                if self.policy_projected_bytes != 0
                    || self.persisted_bytes != 0
                    || self.retrieval_policy != ToolArtifactRetrievalPolicyV1::Unavailable
                {
                    bail!("ephemeral unavailable artifact cannot claim persisted retrieval");
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn retrieval_available(&self) -> bool {
        self.persisted_bytes > 0
            && self.retrieval_policy != ToolArtifactRetrievalPolicyV1::Unavailable
    }
}

fn validate_truncation(
    descriptor: &ToolArtifactDescriptorV1,
    truncation: &ToolArtifactTruncationV1,
) -> Result<()> {
    if truncation.omitted_bytes == 0
        || truncation
            .retained_head_bytes
            .saturating_add(truncation.retained_tail_bytes)
            != descriptor.persisted_bytes
        || descriptor
            .persisted_bytes
            .saturating_add(truncation.omitted_bytes)
            != descriptor.policy_projected_bytes
    {
        bail!("tool artifact truncation metadata is inconsistent");
    }
    Ok(())
}

/// Stable, bounded facts that survive model-view aging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolResultFactsV1 {
    pub status: String,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ToolError>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutation_receipt_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approval_receipt_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification_receipt_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_provenance_refs: Vec<String>,
    pub tool_specific: Value,
}

impl ToolResultFactsV1 {
    pub fn from_result(result: &ToolResult) -> Self {
        let error = match &result.status {
            crate::ToolResultStatus::Ok => None,
            crate::ToolResultStatus::Error(error) => {
                let mut error = error.clone();
                error.message = bounded_utf8(
                    &safe_persistence_text(&error.message),
                    TOOL_RESULT_ERROR_SUMMARY_MAX_BYTES,
                );
                error.details = safe_persistence_json_value(error.details);
                Some(error)
            }
        };
        let tool_specific = bounded_tool_specific(&result.metadata.details);
        let changed_files = result
            .metadata
            .changed_files
            .iter()
            .take(TOOL_RESULT_FACT_PATH_LIMIT)
            .map(|path| {
                let safe = safe_persistence_text(path);
                bounded_utf8(&safe, TOOL_RESULT_FACT_PATH_MAX_BYTES)
            })
            .collect();
        let mutation_receipt_refs = result
            .metadata
            .receipt
            .as_ref()
            .map(|receipt| {
                receipt
                    .mutation_operation_ids
                    .iter()
                    .map(|value| bounded_utf8(&safe_persistence_text(value), 256))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            status: if error.is_some() { "error" } else { "ok" }.to_owned(),
            exit_code: result.metadata.exit_code,
            duration_ms: result.metadata.duration_ms,
            changed_files,
            error,
            mutation_receipt_refs,
            approval_receipt_refs: Vec::new(),
            verification_receipt_refs: Vec::new(),
            external_provenance_refs: Vec::new(),
            tool_specific,
        }
    }

    pub(super) fn add_approval_receipt_ref(&mut self, receipt_ref: &str) {
        self.add_bounded_ref(ToolResultFactRefKind::Approval, receipt_ref);
    }

    pub(super) fn add_mutation_receipt_ref(&mut self, receipt_ref: &str) {
        self.add_bounded_ref(ToolResultFactRefKind::Mutation, receipt_ref);
    }

    pub(super) fn add_verification_receipt_ref(&mut self, receipt_ref: &str) {
        self.add_bounded_ref(ToolResultFactRefKind::Verification, receipt_ref);
    }

    pub(super) fn add_external_provenance_ref(&mut self, provenance_ref: &str) {
        self.add_bounded_ref(ToolResultFactRefKind::ExternalProvenance, provenance_ref);
    }

    pub(super) fn add_changed_file(&mut self, path: &str) {
        if self.changed_files.len() >= TOOL_RESULT_FACT_PATH_LIMIT {
            return;
        }
        let value = bounded_utf8(
            &safe_persistence_text(path),
            TOOL_RESULT_FACT_PATH_MAX_BYTES,
        );
        if value.is_empty() || self.changed_files.contains(&value) {
            return;
        }
        self.changed_files.push(value);
        if self.validate().is_err() {
            self.changed_files.pop();
        }
    }

    fn add_bounded_ref(&mut self, kind: ToolResultFactRefKind, receipt_ref: &str) {
        const REF_LIMIT: usize = 16;
        const REF_MAX_BYTES: usize = 256;

        let value = bounded_utf8(&safe_persistence_text(receipt_ref), REF_MAX_BYTES);
        if value.is_empty() {
            return;
        }
        let target = match kind {
            ToolResultFactRefKind::Approval => &mut self.approval_receipt_refs,
            ToolResultFactRefKind::Mutation => &mut self.mutation_receipt_refs,
            ToolResultFactRefKind::Verification => &mut self.verification_receipt_refs,
            ToolResultFactRefKind::ExternalProvenance => &mut self.external_provenance_refs,
        };
        if target.len() >= REF_LIMIT || target.contains(&value) {
            return;
        }
        target.push(value);
        if self.validate().is_err() {
            let target = match kind {
                ToolResultFactRefKind::Approval => &mut self.approval_receipt_refs,
                ToolResultFactRefKind::Mutation => &mut self.mutation_receipt_refs,
                ToolResultFactRefKind::Verification => &mut self.verification_receipt_refs,
                ToolResultFactRefKind::ExternalProvenance => &mut self.external_provenance_refs,
            };
            target.pop();
        }
    }

    pub fn validate(&self) -> Result<()> {
        let encoded = serde_json::to_vec(self).context("failed to encode tool result facts")?;
        if encoded.len() > TOOL_RESULT_FACTS_MAX_BYTES {
            bail!("tool result facts exceed their byte limit");
        }
        if !matches!(self.status.as_str(), "ok" | "error") {
            bail!("tool result facts contain an unsupported status");
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum ToolResultFactRefKind {
    Approval,
    Mutation,
    Verification,
    ExternalProvenance,
}

fn bounded_tool_specific(value: &Value) -> Value {
    let safe = safe_persistence_json_value(value.clone());
    match serde_json::to_vec(&safe) {
        Ok(encoded) if encoded.len() <= TOOL_RESULT_FACTS_MAX_BYTES / 2 => safe,
        Ok(encoded) => json!({
            "projection": "truncated",
            "original_bytes": encoded.len(),
        }),
        Err(_) => json!({
            "projection": "unavailable",
        }),
    }
}

/// Why the model preview has its current shape.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPreviewKind {
    Complete,
    HeadTail,
    Structured,
    Aged,
    Unavailable,
}

/// Provider-facing bounded representation for one tool result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolModelViewV1 {
    pub preview: String,
    pub preview_kind: ToolPreviewKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<ToolArtifactRefV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_hint: Option<String>,
    pub token_upper_bound: u64,
    pub projection_version: u16,
}

impl ToolModelViewV1 {
    pub fn canonical_content(&self) -> Result<String> {
        self.validate()?;
        serde_json::to_string(self).context("failed to encode tool model view")
    }

    pub fn validate(&self) -> Result<()> {
        if self.projection_version != TOOL_MODEL_VIEW_SCHEMA_VERSION {
            bail!("tool model view has an unsupported projection version");
        }
        if self.preview.len() > TOOL_MODEL_VIEW_MAX_BYTES {
            bail!("tool model view preview exceeds its byte limit");
        }
        if let Some(reference) = &self.artifact_ref {
            reference.validate()?;
        }
        let encoded = serde_json::to_vec(self).context("failed to encode tool model view")?;
        if encoded.len() > TOOL_RESULT_EVENT_TARGET_BYTES / 2 {
            bail!("tool model view exceeds its durable event budget");
        }
        Ok(())
    }
}

/// UI-facing bounded representation, independent from provider request material.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolDisplayViewV1 {
    pub status_label: String,
    pub summary: String,
    pub preview: String,
    pub observed_bytes: u64,
    pub persisted_bytes: u64,
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<ToolArtifactRefV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub display_capabilities: Vec<ToolDisplayCapability>,
    /// RFC-0062 15: whether the displayed preview is shorter than the persisted output.
    pub preview_truncated: bool,
    /// RFC-0062 9.7/15: why the preview is truncated (shared surface vocabulary).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation_reason: Option<ToolPreviewTruncationReasonV1>,
    /// RFC-0062 9.3/15: immutable source/policy/storage capture completeness shared by every
    /// surface; absent only when no capture evidence exists (legacy projections).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_completeness: Option<ToolResultCaptureCompletenessV1>,
}

impl ToolDisplayViewV1 {
    pub fn validate(&self) -> Result<()> {
        if self.preview.len() > TOOL_DISPLAY_VIEW_MAX_BYTES
            || self.summary.len() > 2048
            || self.status_label.len() > 64
        {
            bail!("tool display view exceeds its byte limit");
        }
        if let Some(reference) = &self.artifact_ref {
            reference.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolDisplayCapability {
    ReadNextPage,
    SearchLiteral,
    CopySummary,
}

/// Complete in-process three-view result after artifact capture.
#[derive(Debug, Clone)]
pub struct ToolResultViewsV2 {
    pub artifact: ToolArtifactBindingV1,
    pub facts: ToolResultFactsV1,
    pub model: ToolModelViewV1,
    pub display: ToolDisplayViewV1,
}

/// Capture path used for a durable tool-result record.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultCapturePathV1 {
    PreCapturedArtifact,
    InlineCapture,
    TypedRetrievalReceipt,
}

/// Body-free telemetry for finding adapters that still depend on inline capture.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolResultCaptureTelemetryV1 {
    pub capture_path: ToolResultCapturePathV1,
    pub observed_inline_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hard_guard_bytes: Option<u64>,
    pub hard_guard_exceeded: bool,
    pub inline_projection_truncated: bool,
}

impl ToolResultCaptureTelemetryV1 {
    fn validate(&self) -> Result<()> {
        match self.capture_path {
            ToolResultCapturePathV1::InlineCapture => {
                if self.hard_guard_bytes != Some(TOOL_RESULT_INLINE_CAPTURE_MAX_BYTES as u64)
                    || self.hard_guard_exceeded
                        != (self.observed_inline_bytes
                            > TOOL_RESULT_INLINE_CAPTURE_MAX_BYTES as u64)
                {
                    bail!("inline capture telemetry is inconsistent");
                }
            }
            ToolResultCapturePathV1::PreCapturedArtifact
            | ToolResultCapturePathV1::TypedRetrievalReceipt => {
                if self.hard_guard_bytes.is_some() || self.hard_guard_exceeded {
                    bail!("streaming capture telemetry contains an inline hard guard");
                }
            }
        }
        if self.hard_guard_exceeded && !self.inline_projection_truncated {
            bail!("hard-guarded inline capture must report a truncated projection");
        }
        Ok(())
    }
}

/// RFC-0062 9.5: typed tool outcome is the only authority for provider wire error mapping.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultOutcomeV1 {
    Success,
    ToolError,
}

/// RFC-0062 9.5: provider-neutral wire semantics attached to every V3 tool result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolResultWireSemanticsV1 {
    pub outcome: ToolResultOutcomeV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<crate::ToolErrorKind>,
}

impl ToolResultWireSemanticsV1 {
    pub fn from_result(result: &crate::ToolResult) -> Self {
        match &result.status {
            crate::ToolResultStatus::Ok => Self {
                outcome: ToolResultOutcomeV1::Success,
                error_kind: None,
            },
            crate::ToolResultStatus::Error(error) => Self {
                outcome: ToolResultOutcomeV1::ToolError,
                error_kind: Some(error.kind),
            },
        }
    }
}

/// RFC-0062 9.1: capture layout of process-backed streams. Ordinary pipes never claim a global
/// cross-stream order; only a PTY may declare one ordered combined stream.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputStreamLayoutV1 {
    SeparatePipesNoCrossStreamOrder,
    PtyOrdered,
    SingleStream,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputStreamV1 {
    Stdout,
    Stderr,
    Combined,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolSourceCompletenessV1 {
    Complete,
    Interrupted,
    ResourceLimited,
    ReaderFailed,
}

impl ToolSourceCompletenessV1 {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Interrupted => "interrupted",
            Self::ResourceLimited => "resource_limited",
            Self::ReaderFailed => "reader_failed",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicyCompletenessV1 {
    Preserved,
    Redacted,
    EphemeralOnly,
    Rejected,
}

impl ToolPolicyCompletenessV1 {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preserved => "preserved",
            Self::Redacted => "redacted",
            Self::EphemeralOnly => "ephemeral_only",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolStorageCompletenessV1 {
    Complete,
    TruncatedAtLimit,
    Unavailable,
}

impl ToolStorageCompletenessV1 {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::TruncatedAtLimit => "truncated_at_limit",
            Self::Unavailable => "unavailable",
        }
    }
}

/// RFC-0062 9.3: immutable capture completeness frozen at tool settlement.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolResultCaptureCompletenessV1 {
    pub source: ToolSourceCompletenessV1,
    pub policy: ToolPolicyCompletenessV1,
    pub storage: ToolStorageCompletenessV1,
}

/// RFC-0062 9.2: one canonical persisted output segment. Ordinary pipes persist at most one
/// contiguous segment per stream in stdout-then-stderr storage order.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolOutputSegmentV1 {
    pub stream: ToolOutputStreamV1,
    pub artifact_offset: u64,
    pub persisted_bytes: u64,
    pub eligible_bytes: u64,
    pub observed_bytes: u64,
    pub preview_bytes: u64,
    pub preview_truncated: bool,
    pub storage: ToolStorageCompletenessV1,
}

/// RFC-0062 9.4: append-only retrieval availability state. Terminal states never return to
/// Available in V1; recovery requires a future republish contract.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ToolArtifactAvailabilityStateV1 {
    Available,
    DisabledPendingDelete,
    Expired,
    Missing,
    HashMismatch,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolArtifactAvailabilityReasonV1 {
    GcDisable,
    GcExpired,
    ReaderDetectedMissing,
    ReaderDetectedHashMismatch,
    PolicyRevoked,
    CaptureFallback,
}

/// RFC-0062 9.4: generation-guarded availability transition. Projections only apply events whose
/// expected_generation matches; stale or duplicate transitions fail closed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolArtifactAvailabilityChangedV1 {
    pub schema_version: u16,
    pub artifact_ref: ToolArtifactRefV1,
    pub expected_generation: u64,
    pub generation: u64,
    pub previous: ToolArtifactAvailabilityStateV1,
    pub next: ToolArtifactAvailabilityStateV1,
    pub reason: ToolArtifactAvailabilityReasonV1,
    pub changed_at_ms: u64,
}

impl ToolArtifactAvailabilityChangedV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != TOOL_ARTIFACT_AVAILABILITY_CHANGED_SCHEMA_VERSION {
            bail!("tool artifact availability change schema version is unsupported");
        }
        self.artifact_ref.validate()?;
        if self.expected_generation.saturating_add(1) != self.generation
            || self.previous == self.next
        {
            bail!("tool artifact availability transition is not generation-ordered");
        }
        let allowed = matches!(
            (self.previous, self.next),
            (
                ToolArtifactAvailabilityStateV1::Available,
                ToolArtifactAvailabilityStateV1::DisabledPendingDelete
            ) | (
                ToolArtifactAvailabilityStateV1::Available,
                ToolArtifactAvailabilityStateV1::Missing
            ) | (
                ToolArtifactAvailabilityStateV1::Available,
                ToolArtifactAvailabilityStateV1::HashMismatch
            ) | (
                ToolArtifactAvailabilityStateV1::Available,
                ToolArtifactAvailabilityStateV1::Unavailable
            ) | (
                ToolArtifactAvailabilityStateV1::DisabledPendingDelete,
                ToolArtifactAvailabilityStateV1::Expired
            )
        );
        if !allowed {
            bail!("tool artifact availability transition is not in the allowed state machine");
        }
        Ok(())
    }
}

/// RFC-0062 9.1: session-aware capture plan frozen before spawn. Execution backends derive a
/// process-local config from it and receive an opaque sink; they never see session authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolExecutionCapturePlanV1 {
    pub schema_version: u16,
    pub session_scope_id_hash: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub media_type: String,
    pub stream_layout: ToolOutputStreamLayoutV1,
    pub preview_limit_bytes_per_stream: u64,
    pub artifact_limit_bytes_combined: u64,
    pub artifact_reservation_stdout_bytes: u64,
    pub artifact_reservation_stderr_bytes: u64,
    pub artifact_staging_limit_bytes_per_stream: u64,
    pub observed_limit_bytes_combined: u64,
    pub retention_class: ToolArtifactRetentionClass,
    pub persistence_policy: ToolOutputPersistencePolicy,
}

impl ToolExecutionCapturePlanV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != TOOL_EXECUTION_CAPTURE_PLAN_SCHEMA_VERSION {
            bail!("tool execution capture plan schema version is unsupported");
        }
        if self.session_scope_id_hash.is_empty()
            || self.tool_call_id.trim().is_empty()
            || self.tool_name.trim().is_empty()
        {
            bail!("tool execution capture plan identity is incomplete");
        }
        if matches!(
            self.stream_layout,
            ToolOutputStreamLayoutV1::SeparatePipesNoCrossStreamOrder
        ) && self
            .artifact_reservation_stdout_bytes
            .saturating_add(self.artifact_reservation_stderr_bytes)
            != self.artifact_limit_bytes_combined
        {
            bail!("separate-pipe reservations must sum to the combined artifact payload cap");
        }
        if self.artifact_reservation_stdout_bytes > self.artifact_staging_limit_bytes_per_stream
            || self.artifact_reservation_stderr_bytes > self.artifact_staging_limit_bytes_per_stream
        {
            bail!("artifact reservation exceeds its per-stream staging bound");
        }
        Ok(())
    }

    #[must_use]
    pub fn canonical_hash(&self) -> String {
        stable_event_hash(serde_json::to_vec(self).unwrap_or_default().as_slice())
    }

    #[must_use]
    pub fn process_capture_config(&self) -> ProcessStreamCaptureConfigV1 {
        ProcessStreamCaptureConfigV1 {
            stream_layout: self.stream_layout,
            preview_limit_bytes_per_stream: self.preview_limit_bytes_per_stream,
            artifact_payload_limit_bytes_combined: self.artifact_limit_bytes_combined,
            artifact_reservation_stdout_bytes: self.artifact_reservation_stdout_bytes,
            artifact_reservation_stderr_bytes: self.artifact_reservation_stderr_bytes,
            artifact_staging_limit_bytes_per_stream: self.artifact_staging_limit_bytes_per_stream,
            observed_limit_bytes_combined: self.observed_limit_bytes_combined,
        }
    }
}

/// RFC-0062 9.1: session-free capture configuration derived by the execution backend.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProcessStreamCaptureConfigV1 {
    pub stream_layout: ToolOutputStreamLayoutV1,
    pub preview_limit_bytes_per_stream: u64,
    pub artifact_payload_limit_bytes_combined: u64,
    pub artifact_reservation_stdout_bytes: u64,
    pub artifact_reservation_stderr_bytes: u64,
    pub artifact_staging_limit_bytes_per_stream: u64,
    pub observed_limit_bytes_combined: u64,
}

/// RFC-0062 9.1: what the persistence policy allows into the durable artifact.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutputPersistencePolicy {
    Preserve,
    Redact,
    EphemeralOnly,
    Reject,
}

/// RFC-0062 9.5: bounded error summary; the message cap is UTF-8 safe and bodies never copy
/// stderr or artifact content into the message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolErrorSummaryV1 {
    pub kind: crate::ToolErrorKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    pub retryable: bool,
}

impl ToolErrorSummaryV1 {
    pub fn validate(&self) -> Result<()> {
        if self.message.len() > TOOL_RESULT_ERROR_SUMMARY_MAX_BYTES {
            bail!("tool error summary exceeds its byte limit");
        }
        Ok(())
    }
}

/// RFC-0062 10.5: stage that failed while settling a tool result into a terminal fallback.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultFailureStageV1 {
    FactsProjection,
    PreviewProjection,
    ArtifactFinalize,
    DescriptorAppend,
}

/// RFC-0062 10.5: bounded terminal fallback persisted when a capture stage fails; only a dead
/// session writer escalates to the control plane.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolResultTerminalFallbackV1 {
    pub schema_version: u16,
    pub tool_call_id: String,
    pub outcome: ToolResultOutcomeV1,
    pub failure_stage: ToolResultFailureStageV1,
    pub summary: String,
    pub artifact_published_at_settlement: bool,
}

impl ToolResultTerminalFallbackV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != TOOL_RESULT_TERMINAL_FALLBACK_SCHEMA_VERSION
            || self.tool_call_id.trim().is_empty()
            || self.summary.len() > TOOL_RESULT_ERROR_SUMMARY_MAX_BYTES
        {
            bail!("tool result terminal fallback is malformed");
        }
        Ok(())
    }
}

/// RFC-0062 9.7: why the initial model preview is shorter than the persisted output.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPreviewTruncationReasonV1 {
    InitialCap,
    BatchBudget,
    BinaryOnly,
    Fallback,
}

impl ToolPreviewTruncationReasonV1 {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InitialCap => "initial_cap",
            Self::BatchBudget => "batch_budget",
            Self::BinaryOnly => "binary_only",
            Self::Fallback => "fallback",
        }
    }
}

/// RFC-0062 9.6: provider-facing typed tool result payload; adapters pattern-match this and
/// never parse the output JSON to guess the outcome.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderToolResultMessageV1 {
    pub call_id: String,
    pub output: String,
    pub wire_semantics: ToolResultWireSemanticsV1,
}

/// RFC-0062 9.6: typed message payload carried by provider request materialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelMessagePayloadV1 {
    Text {
        content: String,
    },
    AssistantToolCalls {
        content: String,
        tool_calls: Vec<crate::provider::ToolCall>,
    },
    ToolResult(ProviderToolResultMessageV1),
}

/// RFC-0062 9.7: durable V3 tool result. New executions write only V3; sessions containing V2
/// tool-result events are rejected as unsupported schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolResultRecordedV3 {
    pub schema_version: u16,
    pub message_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub artifact: ToolArtifactBindingV1,
    pub facts: ToolResultFactsV1,
    pub initial_model_view: ToolModelViewV1,
    pub initial_model_view_sha256: String,
    pub capture_telemetry: ToolResultCaptureTelemetryV1,
    pub recorded_at_ms: u64,
    pub wire_semantics: ToolResultWireSemanticsV1,
    pub stream_layout: ToolOutputStreamLayoutV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<ToolOutputSegmentV1>,
    pub capture_completeness: ToolResultCaptureCompletenessV1,
    pub initial_availability: ToolArtifactAvailabilityStateV1,
    pub preview_actual_bytes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_truncation_reason: Option<ToolPreviewTruncationReasonV1>,
    pub capture_plan_hash: String,
    pub artifact_hash: String,
}

/// New durable session payload for a provider-visible tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolResultRecordedV2 {
    pub schema_version: u16,
    pub message_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub artifact: ToolArtifactBindingV1,
    pub facts: ToolResultFactsV1,
    pub initial_model_view: ToolModelViewV1,
    pub initial_model_view_sha256: String,
    pub capture_telemetry: ToolResultCaptureTelemetryV1,
    pub recorded_at_ms: u64,
}

impl ToolResultRecordedV3 {
    /// Builds a bounded terminal fallback V3 record when the regular projection fails
    /// (RFC-0062 10.5); only a dead session writer may escalate to the control plane.
    #[cfg(test)]
    fn terminal_fallback(
        result: &ToolResult,
        sensitivity: ToolArtifactSensitivity,
        capture_error: &anyhow::Error,
    ) -> Result<(Self, ToolDisplayViewV1)> {
        let (v2, display) =
            ToolResultRecordedV2::terminal_fallback(result, sensitivity, capture_error)?;
        let plan =
            ToolExecutionCapturePlanV1::inline_default(String::new(), &v2.call_id, &v2.tool_name);
        Ok((Self::from_v2_projection(v2, &plan), display))
    }

    /// Captures an in-process result into the durable V3 contract.
    pub fn capture(
        result: &ToolResult,
        store: Option<&ToolArtifactStore>,
        sensitivity: ToolArtifactSensitivity,
    ) -> Result<(Self, ToolDisplayViewV1)> {
        Self::capture_with_model_preview_limit(
            result,
            store,
            sensitivity,
            tool_model_view_initial_limit(&result.tool_name),
            None,
        )
    }

    pub(crate) fn capture_with_model_preview_limit(
        result: &ToolResult,
        store: Option<&ToolArtifactStore>,
        sensitivity: ToolArtifactSensitivity,
        model_preview_limit: usize,
        capture_plan: Option<&ToolExecutionCapturePlanV1>,
    ) -> Result<(Self, ToolDisplayViewV1)> {
        let (v2, display) = ToolResultRecordedV2::capture_with_model_preview_limit(
            result,
            store,
            sensitivity,
            model_preview_limit,
        )?;
        let plan = match capture_plan {
            Some(plan) => plan.clone(),
            None => ToolExecutionCapturePlanV1::inline_default(
                store
                    .map(|s| s.session_scope_id_hash.clone())
                    .unwrap_or_default(),
                &result.call_id,
                &result.tool_name,
            ),
        };
        Ok((Self::from_v2_projection(v2, &plan), display))
    }

    /// Lifts a V2 projection into the V3 durable record by adding the RFC-0062 fields. This is
    /// the single projection path for non-process-backed tools; process backends publish their
    /// own capture evidence through the harness spool (R62.2).
    fn from_v2_projection(v2: ToolResultRecordedV2, plan: &ToolExecutionCapturePlanV1) -> Self {
        let (storage, policy, initial_availability, artifact_hash) = match &v2.artifact {
            ToolArtifactBindingV1::Published { descriptor } => {
                let (storage, policy) = match &descriptor.completeness {
                    ToolArtifactCompleteness::Complete => (
                        ToolStorageCompletenessV1::Complete,
                        ToolPolicyCompletenessV1::Preserved,
                    ),
                    ToolArtifactCompleteness::PolicyRedacted { .. } => (
                        ToolStorageCompletenessV1::Complete,
                        ToolPolicyCompletenessV1::Redacted,
                    ),
                    ToolArtifactCompleteness::StorageTruncated(_) => (
                        ToolStorageCompletenessV1::TruncatedAtLimit,
                        ToolPolicyCompletenessV1::Preserved,
                    ),
                    ToolArtifactCompleteness::EphemeralUnavailableAfterRestart => (
                        ToolStorageCompletenessV1::Unavailable,
                        ToolPolicyCompletenessV1::EphemeralOnly,
                    ),
                };
                (
                    storage,
                    policy,
                    ToolArtifactAvailabilityStateV1::Available,
                    descriptor.content_sha256.clone(),
                )
            }
            ToolArtifactBindingV1::Unavailable { .. } => (
                ToolStorageCompletenessV1::Unavailable,
                ToolPolicyCompletenessV1::Preserved,
                ToolArtifactAvailabilityStateV1::Unavailable,
                stable_event_hash(b""),
            ),
        };
        let preview_bytes = v2.initial_model_view.preview.len();
        let truncation_reason = if v2.capture_telemetry.inline_projection_truncated
            || v2.initial_model_view.preview_kind == ToolPreviewKind::HeadTail
        {
            Some(ToolPreviewTruncationReasonV1::InitialCap)
        } else {
            None
        };
        let wire_semantics = ToolResultWireSemanticsV1 {
            outcome: if v2.facts.status == "ok" {
                ToolResultOutcomeV1::Success
            } else {
                ToolResultOutcomeV1::ToolError
            },
            error_kind: v2.facts.error.as_ref().map(|error| error.kind),
        };
        Self {
            schema_version: TOOL_RESULT_RECORDED_V3_SCHEMA_VERSION,
            message_id: v2.message_id,
            call_id: v2.call_id,
            tool_name: v2.tool_name,
            artifact: v2.artifact,
            facts: v2.facts,
            initial_model_view: v2.initial_model_view,
            initial_model_view_sha256: v2.initial_model_view_sha256,
            capture_telemetry: v2.capture_telemetry,
            recorded_at_ms: v2.recorded_at_ms,
            wire_semantics,
            stream_layout: plan.stream_layout,
            segments: Vec::new(),
            capture_completeness: ToolResultCaptureCompletenessV1 {
                source: ToolSourceCompletenessV1::Complete,
                policy,
                storage,
            },
            initial_availability,
            preview_actual_bytes: preview_bytes as u32,
            preview_truncation_reason: truncation_reason,
            capture_plan_hash: plan.canonical_hash(),
            artifact_hash,
        }
    }

    /// RFC-0062 8/9.2: builds the durable V3 record from harness-owned process capture evidence.
    /// The artifact was already published by the execution backend's sink; no body is re-captured.
    pub fn from_process_capture(
        result: &ToolResult,
        descriptor: ToolArtifactDescriptorV1,
        plan: &ToolExecutionCapturePlanV1,
        segments: Vec<ToolOutputSegmentV1>,
        completeness: ToolResultCaptureCompletenessV1,
        model_preview_limit: usize,
    ) -> Result<(Self, ToolDisplayViewV1)> {
        if descriptor.tool_call_id != result.call_id || descriptor.tool_name != result.tool_name {
            bail!("process capture descriptor does not belong to its result");
        }
        let model_preview_limit =
            model_preview_limit.min(tool_model_view_initial_limit(&result.tool_name));
        let safe_content = safe_persistence_text(&result.content);
        let (preview, preview_kind) = bounded_model_preview(&safe_content, model_preview_limit);
        let artifact_ref = Some(descriptor.artifact_ref.clone());
        let retrieval_hint = Some(
            "preview may be partial; use read_tool_artifact with the opaque artifact_ref and a line_page or search_literal selector"
                .to_owned(),
        );
        let model = ToolModelViewV1 {
            token_upper_bound: preview.len().div_ceil(4) as u64,
            preview,
            preview_kind,
            artifact_ref: artifact_ref.clone(),
            retrieval_hint,
            projection_version: TOOL_MODEL_VIEW_SCHEMA_VERSION,
        };
        let facts = ToolResultFactsV1::from_result(result);
        let available = descriptor.retrieval_available();
        let status_label = if result.is_error() { "error" } else { "ok" }.to_owned();
        let display_preview = bounded_utf8(
            &safe_content,
            TOOL_DISPLAY_VIEW_MAX_BYTES.saturating_sub(1024),
        );
        let display = ToolDisplayViewV1 {
            status_label: status_label.clone(),
            summary: format!(
                "{} {} ({} observed bytes, {} persisted bytes)",
                result.tool_name,
                status_label,
                descriptor.observed_bytes,
                descriptor.persisted_bytes
            ),
            preview: display_preview.clone(),
            observed_bytes: descriptor.observed_bytes,
            persisted_bytes: descriptor.persisted_bytes,
            has_more: available && descriptor.persisted_bytes > display_preview.len() as u64,
            artifact_ref,
            display_capabilities: if available {
                vec![
                    ToolDisplayCapability::ReadNextPage,
                    ToolDisplayCapability::SearchLiteral,
                    ToolDisplayCapability::CopySummary,
                ]
            } else {
                vec![ToolDisplayCapability::CopySummary]
            },
            preview_truncated: preview_kind == ToolPreviewKind::HeadTail,
            truncation_reason: (preview_kind == ToolPreviewKind::HeadTail)
                .then_some(ToolPreviewTruncationReasonV1::InitialCap),
            capture_completeness: Some(completeness),
        };
        let preview_bytes = model.preview.len();
        let truncation_reason = if preview_kind == ToolPreviewKind::HeadTail {
            Some(ToolPreviewTruncationReasonV1::InitialCap)
        } else {
            None
        };
        let mut record = Self {
            schema_version: TOOL_RESULT_RECORDED_V3_SCHEMA_VERSION,
            message_id: ModelMessage::tool(result.call_id.clone(), "").id,
            call_id: result.call_id.clone(),
            tool_name: result.tool_name.clone(),
            artifact: ToolArtifactBindingV1::Published { descriptor },
            facts,
            initial_model_view: model,
            initial_model_view_sha256: String::new(),
            capture_telemetry: ToolResultCaptureTelemetryV1 {
                capture_path: ToolResultCapturePathV1::PreCapturedArtifact,
                observed_inline_bytes: result.content.len() as u64,
                hard_guard_bytes: None,
                hard_guard_exceeded: false,
                inline_projection_truncated: preview_kind == ToolPreviewKind::HeadTail,
            },
            recorded_at_ms: current_unix_ms(),
            wire_semantics: ToolResultWireSemanticsV1::from_result(result),
            stream_layout: plan.stream_layout,
            segments,
            capture_completeness: completeness,
            initial_availability: ToolArtifactAvailabilityStateV1::Available,
            preview_actual_bytes: preview_bytes as u32,
            preview_truncation_reason: truncation_reason,
            capture_plan_hash: plan.canonical_hash(),
            artifact_hash: stable_event_hash(b""),
        };
        record.artifact_hash = record
            .artifact
            .descriptor()
            .map(|d| d.content_sha256.clone())
            .unwrap_or_else(|| stable_event_hash(b""));
        record.initial_model_view_sha256 = stable_event_hash(record.model_content()?.as_bytes());
        record.validate()?;
        Ok((record, display))
    }

    /// RFC-0062 11.2: re-projects the initial model preview against a batch-allocated limit and
    /// recomputes the canonical hash. Used when a pre-settled process capture projection must
    /// honor a smaller per-result preview budget than the tool's own cap.
    pub fn reproject_preview(
        &mut self,
        model_preview_limit: usize,
        truncation_reason: ToolPreviewTruncationReasonV1,
    ) -> Result<()> {
        let model_preview_limit =
            model_preview_limit.min(tool_model_view_initial_limit(&self.tool_name));
        if self.initial_model_view.preview.len() <= model_preview_limit {
            // The batch budget does not shrink the already-bounded preview: keep the original
            // preview kind, truncation reason and canonical hash so the durable facts that the
            // model first saw are not erased.
            return self.validate();
        }
        let safe_content = safe_persistence_text(self.initial_model_view.preview.as_str());
        let (preview, preview_kind) = bounded_model_preview(&safe_content, model_preview_limit);
        self.initial_model_view.preview = preview;
        self.initial_model_view.preview_kind = preview_kind;
        self.initial_model_view.token_upper_bound =
            self.initial_model_view.preview.len().div_ceil(4) as u64;
        self.preview_actual_bytes = self.initial_model_view.preview.len() as u32;
        self.preview_truncation_reason =
            (preview_kind == ToolPreviewKind::HeadTail).then_some(truncation_reason);
        self.initial_model_view_sha256 = stable_event_hash(self.model_content()?.as_bytes());
        self.validate()
    }

    pub fn model_message(&self) -> Result<ModelMessage> {
        self.model_message_with_view(&self.initial_model_view)
    }

    pub fn model_message_with_view(&self, view: &ToolModelViewV1) -> Result<ModelMessage> {
        self.validate()?;
        view.validate()?;
        let output = serde_json::to_string(&ToolModelEnvelopeV1 {
            facts: &self.facts,
            projection: view,
        })
        .context("failed to encode tool model envelope")?;
        Ok(ModelMessage {
            id: self.message_id.clone(),
            role: MessageRole::Tool,
            content: Some(output.clone()),
            tool_calls: Vec::new(),
            tool_call_id: Some(self.call_id.clone()),
            assistant_kind: None,
            image_attachments: Vec::new(),
            tool_result_payload: Some(ProviderToolResultMessageV1 {
                call_id: self.call_id.clone(),
                output,
                wire_semantics: self.wire_semantics.clone(),
            }),
        })
    }

    pub fn model_content(&self) -> Result<String> {
        self.validate_shape()?;
        serde_json::to_string(&ToolModelEnvelopeV1 {
            facts: &self.facts,
            projection: &self.initial_model_view,
        })
        .context("failed to encode tool model envelope")
    }

    pub fn validate(&self) -> Result<()> {
        self.validate_shape()?;
        let model_hash = stable_event_hash(self.model_content()?.as_bytes());
        if model_hash != self.initial_model_view_sha256 {
            bail!("tool result initial model view hash mismatch");
        }
        if self.wire_semantics.outcome == ToolResultOutcomeV1::ToolError
            && self.facts.status == "ok"
        {
            bail!("tool result wire semantics contradict a successful facts status");
        }
        if self.wire_semantics.outcome == ToolResultOutcomeV1::Success
            && self.facts.status == "error"
        {
            bail!("tool result wire semantics contradict an error facts status");
        }
        let encoded = serde_json::to_vec(self).context("failed to encode tool result V3 record")?;
        if encoded.len() > TOOL_RESULT_EVENT_TARGET_BYTES {
            bail!("tool result V3 record exceeds its event target");
        }
        Ok(())
    }

    fn validate_shape(&self) -> Result<()> {
        if self.schema_version != TOOL_RESULT_RECORDED_V3_SCHEMA_VERSION
            || self.message_id.trim().is_empty()
            || self.call_id.trim().is_empty()
            || self.tool_name.trim().is_empty()
        {
            bail!("tool result V3 record is malformed");
        }
        if let Some(artifact) = self.artifact.descriptor()
            && (artifact.tool_call_id != self.call_id || artifact.tool_name != self.tool_name)
        {
            bail!("tool result artifact does not belong to its result");
        }
        self.artifact.validate()?;
        self.facts.validate()?;
        self.initial_model_view.validate()?;
        self.capture_telemetry.validate()?;
        if !self.capture_plan_hash.starts_with("sha256:")
            || !self.artifact_hash.starts_with("sha256:")
        {
            bail!("tool result V3 hashes are malformed");
        }
        for segment in &self.segments {
            if segment.persisted_bytes > segment.eligible_bytes
                || segment.eligible_bytes > segment.observed_bytes
                || segment.preview_bytes > segment.persisted_bytes
            {
                bail!("tool result V3 segment accounting is inconsistent");
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn display_view(&self) -> ToolDisplayViewV1 {
        let (observed_bytes, persisted_bytes, artifact_ref, available) = match &self.artifact {
            ToolArtifactBindingV1::Published { descriptor } => (
                descriptor.observed_bytes,
                descriptor.persisted_bytes,
                Some(descriptor.artifact_ref.clone()),
                descriptor.retrieval_available()
                    && self.initial_availability == ToolArtifactAvailabilityStateV1::Available,
            ),
            ToolArtifactBindingV1::Unavailable { unavailable } => {
                (unavailable.observed_bytes, 0, None, false)
            }
        };
        let display_preview = bounded_utf8(
            &self.initial_model_view.preview,
            TOOL_DISPLAY_VIEW_MAX_BYTES.saturating_sub(1024),
        );
        ToolDisplayViewV1 {
            status_label: self.facts.status.clone(),
            summary: format!(
                "{} {} ({} observed bytes, {} persisted bytes)",
                self.tool_name, self.facts.status, observed_bytes, persisted_bytes
            ),
            preview: display_preview.clone(),
            observed_bytes,
            persisted_bytes,
            has_more: available
                && persisted_bytes > display_preview.len() as u64
                && self.initial_availability == ToolArtifactAvailabilityStateV1::Available,
            artifact_ref,
            display_capabilities: if available {
                vec![
                    ToolDisplayCapability::ReadNextPage,
                    ToolDisplayCapability::SearchLiteral,
                    ToolDisplayCapability::CopySummary,
                ]
            } else {
                vec![ToolDisplayCapability::CopySummary]
            },
            preview_truncated: persisted_bytes > display_preview.len() as u64
                || self.initial_model_view.preview_kind == ToolPreviewKind::HeadTail,
            truncation_reason: self.preview_truncation_reason,
            capture_completeness: Some(self.capture_completeness),
        }
    }
}

impl ToolExecutionCapturePlanV1 {
    /// Default capture plan for tools that do not spawn a process (inline or pre-captured
    /// adapters). Process backends construct their own plan before spawn.
    pub fn inline_default(
        session_scope_id_hash: impl Into<String>,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: TOOL_EXECUTION_CAPTURE_PLAN_SCHEMA_VERSION,
            session_scope_id_hash: session_scope_id_hash.into(),
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            media_type: "text/plain; charset=utf-8".to_owned(),
            stream_layout: ToolOutputStreamLayoutV1::SingleStream,
            preview_limit_bytes_per_stream: TOOL_MODEL_VIEW_INITIAL_MAX_BYTES as u64,
            artifact_limit_bytes_combined: TOOL_ARTIFACT_MAX_BYTES as u64,
            artifact_reservation_stdout_bytes: TOOL_ARTIFACT_MAX_BYTES as u64,
            artifact_reservation_stderr_bytes: 0,
            artifact_staging_limit_bytes_per_stream: TOOL_ARTIFACT_MAX_BYTES as u64,
            observed_limit_bytes_combined: TOOL_ARTIFACT_SESSION_BUDGET_BYTES,
            retention_class: ToolArtifactRetentionClass::SessionBound,
            persistence_policy: ToolOutputPersistencePolicy::Preserve,
        }
    }

    /// Process-backed defaults per RFC-0062 10.3.
    pub fn process_defaults(
        session_scope_id_hash: impl Into<String>,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: TOOL_EXECUTION_CAPTURE_PLAN_SCHEMA_VERSION,
            session_scope_id_hash: session_scope_id_hash.into(),
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            media_type: "text/plain; charset=utf-8".to_owned(),
            stream_layout: ToolOutputStreamLayoutV1::SeparatePipesNoCrossStreamOrder,
            preview_limit_bytes_per_stream: 64 * 1024,
            artifact_limit_bytes_combined: TOOL_ARTIFACT_MAX_BYTES as u64,
            artifact_reservation_stdout_bytes: TOOL_ARTIFACT_MAX_BYTES as u64 / 2,
            artifact_reservation_stderr_bytes: TOOL_ARTIFACT_MAX_BYTES as u64 / 2,
            artifact_staging_limit_bytes_per_stream: TOOL_ARTIFACT_MAX_BYTES as u64,
            observed_limit_bytes_combined: 128 * 1024 * 1024,
            retention_class: ToolArtifactRetentionClass::SessionBound,
            persistence_policy: ToolOutputPersistencePolicy::Preserve,
        }
    }
}

impl ToolResultRecordedV2 {
    /// Captures an in-process result into the durable three-view contract.
    ///
    /// Artifact publication failures are represented explicitly and never cause the tool to be
    /// rerun or its terminal facts to be dropped.
    pub fn capture(
        result: &ToolResult,
        store: Option<&ToolArtifactStore>,
        sensitivity: ToolArtifactSensitivity,
    ) -> Result<(Self, ToolDisplayViewV1)> {
        Self::capture_with_model_preview_limit(
            result,
            store,
            sensitivity,
            tool_model_view_initial_limit(&result.tool_name),
        )
    }

    pub(crate) fn capture_with_model_preview_limit(
        result: &ToolResult,
        store: Option<&ToolArtifactStore>,
        sensitivity: ToolArtifactSensitivity,
        model_preview_limit: usize,
    ) -> Result<(Self, ToolDisplayViewV1)> {
        match Self::capture_with_model_preview_limit_inner(
            result,
            store,
            sensitivity,
            model_preview_limit,
        ) {
            Ok(captured) => Ok(captured),
            Err(capture_error) => {
                let fallback = Self::terminal_fallback(result, sensitivity, &capture_error)?;
                Ok(fallback)
            }
        }
    }

    /// Builds a bounded terminal fallback record when the regular projection fails.
    ///
    /// RFC-0062 10.5: only a dead session writer may escalate capture failure into a
    /// control-plane failure; ordinary tool results always settle with bounded facts and an
    /// explicit `Unavailable` artifact state instead of aborting the agent run.
    fn terminal_fallback(
        result: &ToolResult,
        _sensitivity: ToolArtifactSensitivity,
        capture_error: &anyhow::Error,
    ) -> Result<(Self, ToolDisplayViewV1)> {
        let observed_bytes = result.content.len() as u64;
        let mut facts = ToolResultFactsV1::from_result(result);
        if let Some(error) = facts.error.as_mut() {
            error.message = bounded_utf8(
                &safe_persistence_text(&error.message),
                TOOL_RESULT_ERROR_SUMMARY_MAX_BYTES,
            );
        }
        let model = ToolModelViewV1 {
            token_upper_bound: 0,
            preview: String::new(),
            preview_kind: ToolPreviewKind::HeadTail,
            artifact_ref: None,
            retrieval_hint: None,
            projection_version: TOOL_MODEL_VIEW_SCHEMA_VERSION,
        };
        let artifact = ToolArtifactBindingV1::Unavailable {
            unavailable: ToolArtifactUnavailableV1 {
                availability: ToolArtifactAvailability::Unavailable,
                observed_bytes,
                reason: "tool result capture failed and settled a bounded terminal fallback"
                    .to_owned(),
            },
        };
        let display = ToolDisplayViewV1 {
            status_label: if result.is_error() { "error" } else { "ok" }.to_owned(),
            summary: format!(
                "{} {} ({} observed bytes, 0 persisted bytes; capture failed: {})",
                result.tool_name,
                if result.is_error() { "error" } else { "ok" },
                observed_bytes,
                bounded_utf8(&capture_error.to_string(), 256)
            ),
            preview: String::new(),
            observed_bytes,
            persisted_bytes: 0,
            has_more: false,
            artifact_ref: None,
            display_capabilities: vec![ToolDisplayCapability::CopySummary],
            preview_truncated: observed_bytes > 0,
            truncation_reason: Some(ToolPreviewTruncationReasonV1::Fallback),
            capture_completeness: Some(ToolResultCaptureCompletenessV1 {
                source: ToolSourceCompletenessV1::Interrupted,
                policy: ToolPolicyCompletenessV1::Preserved,
                storage: ToolStorageCompletenessV1::Unavailable,
            }),
        };
        let mut record = Self {
            schema_version: TOOL_RESULT_RECORDED_SCHEMA_VERSION,
            message_id: ModelMessage::tool(result.call_id.clone(), "").id,
            call_id: result.call_id.clone(),
            tool_name: result.tool_name.clone(),
            artifact,
            facts,
            initial_model_view: model,
            initial_model_view_sha256: String::new(),
            capture_telemetry: ToolResultCaptureTelemetryV1 {
                capture_path: ToolResultCapturePathV1::InlineCapture,
                observed_inline_bytes: observed_bytes,
                hard_guard_bytes: Some(TOOL_RESULT_INLINE_CAPTURE_MAX_BYTES as u64),
                hard_guard_exceeded: observed_bytes > TOOL_RESULT_INLINE_CAPTURE_MAX_BYTES as u64,
                inline_projection_truncated: true,
            },
            recorded_at_ms: current_unix_ms(),
        };
        record.initial_model_view_sha256 = stable_event_hash(record.model_content()?.as_bytes());
        record.validate()?;
        Ok((record, display))
    }

    fn capture_with_model_preview_limit_inner(
        result: &ToolResult,
        store: Option<&ToolArtifactStore>,
        sensitivity: ToolArtifactSensitivity,
        model_preview_limit: usize,
    ) -> Result<(Self, ToolDisplayViewV1)> {
        let model_preview_limit =
            model_preview_limit.min(tool_model_view_initial_limit(&result.tool_name));
        let capture_path = if result.tool_name == "read_tool_artifact" {
            ToolResultCapturePathV1::TypedRetrievalReceipt
        } else if result.pre_captured_artifact().is_some() {
            ToolResultCapturePathV1::PreCapturedArtifact
        } else {
            ToolResultCapturePathV1::InlineCapture
        };
        let inline_guard_exceeded = capture_path == ToolResultCapturePathV1::InlineCapture
            && result.content.len() > TOOL_RESULT_INLINE_CAPTURE_MAX_BYTES;
        // Never apply persistence redaction to an unbounded inline body. This bounds both the
        // transient allocation and the durable projection while retaining useful head/tail
        // diagnostics. Pre-captured adapters are also projected defensively if they accidentally
        // return an oversized inline view.
        let inline_projection_truncated =
            result.content.len() > TOOL_RESULT_INLINE_CAPTURE_MAX_BYTES;
        let guarded_inline_content = inline_projection_truncated
            .then(|| bounded_model_preview(&result.content, TOOL_MODEL_VIEW_INITIAL_MAX_BYTES).0);
        let safe_content = safe_persistence_text(
            guarded_inline_content
                .as_deref()
                .unwrap_or(result.content.as_str()),
        );
        let artifact = if result.tool_name == "read_tool_artifact" {
            ToolArtifactBindingV1::Unavailable {
                unavailable: ToolArtifactUnavailableV1 {
                    availability: ToolArtifactAvailability::Unavailable,
                    observed_bytes: result.content.len() as u64,
                    reason: "typed retrieval receipts do not externalize another artifact"
                        .to_owned(),
                },
            }
        } else if let Some(pre_captured) = result.pre_captured_artifact() {
            match (pre_captured, store) {
                (PreCapturedToolArtifact::Published(pre_captured), Some(store))
                    if pre_captured.tool_call_id == result.call_id
                        && pre_captured.tool_name == result.tool_name
                        && store.availability(pre_captured)
                            == ToolArtifactAvailability::Available =>
                {
                    ToolArtifactBindingV1::Published {
                        descriptor: pre_captured.as_ref().clone(),
                    }
                }
                (PreCapturedToolArtifact::Published(pre_captured), Some(_)) => {
                    ToolArtifactBindingV1::Unavailable {
                        unavailable: ToolArtifactUnavailableV1 {
                            availability: ToolArtifactAvailability::PolicyRevoked,
                            observed_bytes: pre_captured.observed_bytes,
                            reason:
                                "pre-captured artifact failed session, identity, or hash validation"
                                    .to_owned(),
                        },
                    }
                }
                (PreCapturedToolArtifact::Published(pre_captured), None) => {
                    ToolArtifactBindingV1::Unavailable {
                        unavailable: ToolArtifactUnavailableV1 {
                            availability: ToolArtifactAvailability::Unavailable,
                            observed_bytes: pre_captured.observed_bytes,
                            reason: "session has no durable artifact store".to_owned(),
                        },
                    }
                }
                (PreCapturedToolArtifact::Unavailable { observed_bytes }, Some(_) | None) => {
                    ToolArtifactBindingV1::Unavailable {
                        unavailable: ToolArtifactUnavailableV1 {
                            availability: ToolArtifactAvailability::Missing,
                            observed_bytes: *observed_bytes,
                            reason: "streaming artifact publication failed; raw body unavailable"
                                .to_owned(),
                        },
                    }
                }
            }
        } else if inline_guard_exceeded {
            ToolArtifactBindingV1::Unavailable {
                unavailable: ToolArtifactUnavailableV1 {
                    availability: ToolArtifactAvailability::Unavailable,
                    observed_bytes: result.content.len() as u64,
                    reason: "inline content exceeded the hard guard; streaming artifact capture is required"
                        .to_owned(),
                },
            }
        } else {
            match store {
                Some(store) => match store.capture_policy_safe_bytes(
                    &result.call_id,
                    &result.tool_name,
                    safe_content.as_bytes(),
                    result.content.len() as u64,
                    "text/plain; charset=utf-8",
                    ToolArtifactEncoding::Utf8,
                    sensitivity,
                    u32::from(safe_content != result.content),
                ) {
                    Ok(descriptor) => ToolArtifactBindingV1::Published { descriptor },
                    Err(_error) => ToolArtifactBindingV1::Unavailable {
                        unavailable: ToolArtifactUnavailableV1 {
                            availability: ToolArtifactAvailability::Missing,
                            observed_bytes: result.content.len() as u64,
                            reason: "artifact publication failed; raw body unavailable".to_owned(),
                        },
                    },
                },
                None => ToolArtifactBindingV1::Unavailable {
                    unavailable: ToolArtifactUnavailableV1 {
                        availability: ToolArtifactAvailability::Unavailable,
                        observed_bytes: result.content.len() as u64,
                        reason: "session has no durable artifact store".to_owned(),
                    },
                },
            }
        };
        let facts = ToolResultFactsV1::from_result(result);
        let (preview, mut preview_kind) = bounded_model_preview(&safe_content, model_preview_limit);
        if inline_projection_truncated {
            preview_kind = ToolPreviewKind::HeadTail;
        }
        let artifact_ref = artifact
            .descriptor()
            .map(|descriptor| descriptor.artifact_ref.clone());
        let retrieval_hint = artifact_ref
            .as_ref()
            .map(|_| {
                "preview may be partial; use read_tool_artifact with the opaque artifact_ref and a line_page or search_literal selector"
                    .to_owned()
            });
        let model = ToolModelViewV1 {
            token_upper_bound: preview.len().div_ceil(4) as u64,
            preview,
            preview_kind,
            artifact_ref: artifact_ref.clone(),
            retrieval_hint,
            projection_version: TOOL_MODEL_VIEW_SCHEMA_VERSION,
        };
        let (observed_bytes, persisted_bytes, available) = match &artifact {
            ToolArtifactBindingV1::Published { descriptor } => (
                descriptor.observed_bytes,
                descriptor.persisted_bytes,
                descriptor.retrieval_available(),
            ),
            ToolArtifactBindingV1::Unavailable { unavailable } => {
                (unavailable.observed_bytes, 0, false)
            }
        };
        let status_label = if result.is_error() { "error" } else { "ok" }.to_owned();
        let display_preview = bounded_utf8(
            &safe_content,
            TOOL_DISPLAY_VIEW_MAX_BYTES.saturating_sub(1024),
        );
        let display = ToolDisplayViewV1 {
            status_label: status_label.clone(),
            summary: format!(
                "{} {} ({} observed bytes, {} persisted bytes)",
                result.tool_name, status_label, observed_bytes, persisted_bytes
            ),
            preview: display_preview.clone(),
            observed_bytes,
            persisted_bytes,
            has_more: available && persisted_bytes > display_preview.len() as u64,
            artifact_ref,
            display_capabilities: if available {
                vec![
                    ToolDisplayCapability::ReadNextPage,
                    ToolDisplayCapability::SearchLiteral,
                    ToolDisplayCapability::CopySummary,
                ]
            } else {
                vec![ToolDisplayCapability::CopySummary]
            },
            preview_truncated: preview_kind == ToolPreviewKind::HeadTail
                || persisted_bytes > display_preview.len() as u64,
            truncation_reason: (preview_kind == ToolPreviewKind::HeadTail)
                .then_some(ToolPreviewTruncationReasonV1::InitialCap),
            capture_completeness: display_capture_completeness(&artifact),
        };
        display.validate()?;
        let mut record = Self {
            schema_version: TOOL_RESULT_RECORDED_SCHEMA_VERSION,
            message_id: ModelMessage::tool(result.call_id.clone(), "").id,
            call_id: result.call_id.clone(),
            tool_name: result.tool_name.clone(),
            artifact,
            facts,
            initial_model_view: model,
            initial_model_view_sha256: String::new(),
            capture_telemetry: ToolResultCaptureTelemetryV1 {
                capture_path,
                observed_inline_bytes: result.content.len() as u64,
                hard_guard_bytes: (capture_path == ToolResultCapturePathV1::InlineCapture)
                    .then_some(TOOL_RESULT_INLINE_CAPTURE_MAX_BYTES as u64),
                hard_guard_exceeded: inline_guard_exceeded,
                inline_projection_truncated,
            },
            recorded_at_ms: current_unix_ms(),
        };
        record.initial_model_view_sha256 = stable_event_hash(record.model_content()?.as_bytes());
        record.validate()?;
        Ok((record, display))
    }

    #[must_use]
    pub fn display_view(&self) -> ToolDisplayViewV1 {
        let (observed_bytes, persisted_bytes, artifact_ref, available) = match &self.artifact {
            ToolArtifactBindingV1::Published { descriptor } => (
                descriptor.observed_bytes,
                descriptor.persisted_bytes,
                Some(descriptor.artifact_ref.clone()),
                descriptor.retrieval_available(),
            ),
            ToolArtifactBindingV1::Unavailable { unavailable } => {
                (unavailable.observed_bytes, 0, None, false)
            }
        };
        let display_preview = bounded_utf8(
            &self.initial_model_view.preview,
            TOOL_DISPLAY_VIEW_MAX_BYTES.saturating_sub(1024),
        );
        ToolDisplayViewV1 {
            status_label: self.facts.status.clone(),
            summary: format!(
                "{} {} ({} observed bytes, {} persisted bytes)",
                self.tool_name, self.facts.status, observed_bytes, persisted_bytes
            ),
            preview: display_preview.clone(),
            observed_bytes,
            persisted_bytes,
            has_more: available && persisted_bytes > display_preview.len() as u64,
            artifact_ref,
            display_capabilities: if available {
                vec![
                    ToolDisplayCapability::ReadNextPage,
                    ToolDisplayCapability::SearchLiteral,
                    ToolDisplayCapability::CopySummary,
                ]
            } else {
                vec![ToolDisplayCapability::CopySummary]
            },
            preview_truncated: persisted_bytes > display_preview.len() as u64
                || self.initial_model_view.preview_kind == ToolPreviewKind::HeadTail,
            truncation_reason: (self.initial_model_view.preview_kind == ToolPreviewKind::HeadTail)
                .then_some(ToolPreviewTruncationReasonV1::InitialCap),
            capture_completeness: display_capture_completeness(&self.artifact),
        }
    }

    pub fn model_content(&self) -> Result<String> {
        self.validate_shape()?;
        serde_json::to_string(&ToolModelEnvelopeV1 {
            facts: &self.facts,
            projection: &self.initial_model_view,
        })
        .context("failed to encode tool model envelope")
    }

    pub fn model_message(&self) -> Result<ModelMessage> {
        self.model_message_with_view(&self.initial_model_view)
    }

    pub fn model_message_with_view(&self, view: &ToolModelViewV1) -> Result<ModelMessage> {
        self.validate()?;
        view.validate()?;
        Ok(ModelMessage {
            id: self.message_id.clone(),
            role: MessageRole::Tool,
            content: Some(
                serde_json::to_string(&ToolModelEnvelopeV1 {
                    facts: &self.facts,
                    projection: view,
                })
                .context("failed to encode tool model envelope")?,
            ),
            tool_calls: Vec::new(),
            tool_call_id: Some(self.call_id.clone()),
            assistant_kind: None,
            image_attachments: Vec::new(),
            tool_result_payload: None,
        })
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != TOOL_RESULT_RECORDED_SCHEMA_VERSION
            || self.message_id.trim().is_empty()
            || self.call_id.trim().is_empty()
            || self.tool_name.trim().is_empty()
        {
            bail!("tool result V2 record is malformed");
        }
        if let Some(artifact) = self.artifact.descriptor()
            && (artifact.tool_call_id != self.call_id || artifact.tool_name != self.tool_name)
        {
            bail!("tool result artifact does not belong to its result");
        }
        self.artifact.validate()?;
        self.facts.validate()?;
        self.initial_model_view.validate()?;
        self.capture_telemetry.validate()?;
        let model_hash = stable_event_hash(self.model_content()?.as_bytes());
        if model_hash != self.initial_model_view_sha256 {
            bail!("tool result initial model view hash mismatch");
        }
        let encoded = serde_json::to_vec(self).context("failed to encode tool result V2 record")?;
        if encoded.len() > TOOL_RESULT_EVENT_TARGET_BYTES {
            bail!("tool result V2 record exceeds its event target");
        }
        Ok(())
    }

    fn validate_shape(&self) -> Result<()> {
        if self.schema_version != TOOL_RESULT_RECORDED_SCHEMA_VERSION
            || self.message_id.trim().is_empty()
            || self.call_id.trim().is_empty()
            || self.tool_name.trim().is_empty()
        {
            bail!("tool result V2 record is malformed");
        }
        self.artifact.validate()?;
        self.facts.validate()?;
        self.initial_model_view.validate()?;
        self.capture_telemetry.validate()
    }
}

/// RFC-0062 11.2: minimum preview floor for one safe non-empty current-batch result.
pub const TOOL_MODEL_VIEW_BATCH_FLOOR_BYTES: usize = 512;
/// RFC-0062 11.2: total initial-preview cap for one assistant tool-call batch.
pub const TOOL_MODEL_VIEW_BATCH_BUDGET_BYTES: usize = 64 * 1024;
/// RFC-0062 11.2: hard limit on results in one assistant tool-call batch.
pub const TOOL_MODEL_VIEW_BATCH_MAX_RESULTS: usize = 128;

/// RFC-0062 11.2: deterministic two-phase allocation of the assistant-batch preview budget.
///
/// Phase 1 gives every safe non-empty candidate its `min(candidate, 512 B)` floor in declaration
/// order; phase 2 distributes the remaining budget in declaration order up to each candidate's own
/// cap. Only the final actual UTF-8 preview bytes are charged by the caller.
pub fn allocate_batch_preview_limits(
    declaration_order: &[String],
    candidate_bytes: &std::collections::BTreeMap<String, usize>,
) -> std::collections::BTreeMap<String, usize> {
    let mut limits = std::collections::BTreeMap::new();
    if declaration_order.len() > TOOL_MODEL_VIEW_BATCH_MAX_RESULTS {
        return limits;
    }
    let mut remaining = TOOL_MODEL_VIEW_BATCH_BUDGET_BYTES;
    for call_id in declaration_order {
        let candidate = candidate_bytes.get(call_id).copied().unwrap_or(0);
        if candidate == 0 {
            limits.insert(call_id.clone(), 0);
            continue;
        }
        let floor = candidate.min(TOOL_MODEL_VIEW_BATCH_FLOOR_BYTES);
        let awarded = floor.min(remaining);
        limits.insert(call_id.clone(), awarded);
        remaining = remaining.saturating_sub(awarded);
    }
    for call_id in declaration_order {
        let candidate = candidate_bytes.get(call_id).copied().unwrap_or(0);
        let floor = candidate.min(TOOL_MODEL_VIEW_BATCH_FLOOR_BYTES);
        if candidate <= floor || remaining == 0 {
            continue;
        }
        let awarded = limits.get(call_id).copied().unwrap_or(0);
        let extra = (candidate - floor).min(remaining);
        limits.insert(call_id.clone(), awarded + extra);
        remaining = remaining.saturating_sub(extra);
    }
    limits
}

/// RFC-0062 9.3: derives the display completeness from an artifact binding for legacy
/// projections that predate the V3 capture evidence.
fn display_capture_completeness(
    artifact: &ToolArtifactBindingV1,
) -> Option<ToolResultCaptureCompletenessV1> {
    match artifact {
        ToolArtifactBindingV1::Published { descriptor } => {
            let (policy, storage) = match &descriptor.completeness {
                ToolArtifactCompleteness::Complete => (
                    ToolPolicyCompletenessV1::Preserved,
                    ToolStorageCompletenessV1::Complete,
                ),
                ToolArtifactCompleteness::PolicyRedacted { .. } => (
                    ToolPolicyCompletenessV1::Redacted,
                    ToolStorageCompletenessV1::Complete,
                ),
                ToolArtifactCompleteness::StorageTruncated(_) => (
                    ToolPolicyCompletenessV1::Preserved,
                    ToolStorageCompletenessV1::TruncatedAtLimit,
                ),
                ToolArtifactCompleteness::EphemeralUnavailableAfterRestart => (
                    ToolPolicyCompletenessV1::EphemeralOnly,
                    ToolStorageCompletenessV1::Unavailable,
                ),
            };
            Some(ToolResultCaptureCompletenessV1 {
                source: ToolSourceCompletenessV1::Complete,
                policy,
                storage,
            })
        }
        ToolArtifactBindingV1::Unavailable { .. } => Some(ToolResultCaptureCompletenessV1 {
            source: ToolSourceCompletenessV1::Interrupted,
            policy: ToolPolicyCompletenessV1::Preserved,
            storage: ToolStorageCompletenessV1::Unavailable,
        }),
    }
}

pub fn tool_model_view_initial_limit(tool_name: &str) -> usize {
    let base_name = tool_name.rsplit("__").next().unwrap_or(tool_name);
    if matches!(base_name, "read_file" | "grep" | "glob" | "ls" | "search") {
        TOOL_MODEL_VIEW_HIGH_VOLUME_MAX_BYTES
    } else {
        TOOL_MODEL_VIEW_INITIAL_MAX_BYTES
    }
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
struct ToolModelEnvelopeV1<'a> {
    facts: &'a ToolResultFactsV1,
    projection: &'a ToolModelViewV1,
}

/// Typed bounded selector for model/display artifact retrieval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ToolArtifactSelectorV1 {
    ByteSlice {
        offset: u64,
        limit: u32,
    },
    LinePage {
        start_line: u64,
        line_count: u32,
    },
    SearchLiteral {
        query: String,
        start_offset: u64,
        max_matches: u16,
        context_lines: u16,
    },
}

impl ToolArtifactSelectorV1 {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::ByteSlice { limit, .. }
                if *limit > 0 && *limit <= TOOL_ARTIFACT_READ_MAX_BYTES => {}
            Self::LinePage { line_count, .. }
                if *line_count > 0 && *line_count <= TOOL_ARTIFACT_READ_MAX_LINES => {}
            Self::SearchLiteral {
                query,
                max_matches,
                context_lines,
                ..
            } if !query.is_empty()
                && query.len() <= 512
                && *max_matches > 0
                && *max_matches <= TOOL_ARTIFACT_SEARCH_MAX_MATCHES
                && *context_lines <= TOOL_ARTIFACT_SEARCH_MAX_CONTEXT_LINES => {}
            _ => bail!("tool artifact selector exceeds its bounded policy"),
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolArtifactReadOutcome {
    Returned,
    Unchanged,
    Unavailable,
    Rejected,
    Corrupt,
}

/// Encoding of one bounded retrieval page body.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolArtifactPageEncoding {
    Utf8,
    Base64,
}

/// One bounded typed retrieval response. It is transient and never embedded in the read receipt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolArtifactPageV1 {
    pub artifact_ref: ToolArtifactRefV1,
    pub selector: ToolArtifactSelectorV1,
    pub body: String,
    pub body_encoding: ToolArtifactPageEncoding,
    pub returned_bytes: u32,
    pub page_sha256: String,
    pub artifact_sha256: String,
    pub eof: bool,
    pub match_count: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_selector: Option<ToolArtifactSelectorV1>,
}

impl ToolArtifactPageV1 {
    pub fn validate(&self) -> Result<()> {
        self.artifact_ref.validate()?;
        self.selector.validate()?;
        if self.returned_bytes > TOOL_ARTIFACT_READ_MAX_BYTES
            || !self.page_sha256.starts_with("sha256:")
            || !self.artifact_sha256.starts_with("sha256:")
            || self.match_count > TOOL_ARTIFACT_SEARCH_MAX_MATCHES
        {
            bail!("tool artifact page is malformed");
        }
        if let Some(next) = &self.next_selector {
            next.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct ToolArtifactReadBudgetState {
    reads: u16,
    bytes: u64,
    delivered_pages: std::collections::BTreeMap<String, (String, ToolArtifactPageV1)>,
}

/// Root-owned per-turn retrieval budget shared by cloned and parallel tool contexts.
#[derive(Debug, Clone, Default)]
pub struct ToolArtifactReadBudgetV1 {
    state: Arc<Mutex<ToolArtifactReadBudgetState>>,
}

impl ToolArtifactReadBudgetV1 {
    /// Reads a page once per immutable ref/selector pair inside the same context epoch and
    /// reports repeat delivery explicitly. After an epoch rotation the provider is not assumed to
    /// remember the previously injected page, so the page body is delivered again.
    pub fn read_page_for_call(
        &self,
        store: &ToolArtifactStore,
        artifact_ref: &ToolArtifactRefV1,
        selector: ToolArtifactSelectorV1,
        call_id: &str,
        active_epoch_id: &str,
    ) -> Result<ToolArtifactBudgetedReadV1> {
        if call_id.trim().is_empty() || call_id.len() > 512 {
            bail!("tool artifact read call id is malformed");
        }
        if active_epoch_id.trim().is_empty() || active_epoch_id.len() > 512 {
            bail!("tool artifact read epoch id is malformed");
        }
        artifact_ref.validate()?;
        selector.validate()?;
        let dedupe_key = serde_json::to_string(&(artifact_ref, &selector, active_epoch_id))
            .context("failed to encode tool artifact read dedupe key")?;
        if let Some((original_call_id, page)) = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("tool artifact read budget lock poisoned"))?
            .delivered_pages
            .get(&dedupe_key)
            .cloned()
        {
            return Ok(ToolArtifactBudgetedReadV1 {
                page,
                deduplicated_from_call_id: Some(original_call_id),
            });
        }
        let page = self.read_page(store, artifact_ref, selector)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("tool artifact read budget lock poisoned"))?;
        if let Some((original_call_id, original_page)) =
            state.delivered_pages.get(&dedupe_key).cloned()
        {
            return Ok(ToolArtifactBudgetedReadV1 {
                page: original_page,
                deduplicated_from_call_id: Some(original_call_id),
            });
        }
        state
            .delivered_pages
            .insert(dedupe_key, (call_id.to_owned(), page.clone()));
        Ok(ToolArtifactBudgetedReadV1 {
            page,
            deduplicated_from_call_id: None,
        })
    }

    pub fn read_page(
        &self,
        store: &ToolArtifactStore,
        artifact_ref: &ToolArtifactRefV1,
        selector: ToolArtifactSelectorV1,
    ) -> Result<ToolArtifactPageV1> {
        let reserved_bytes = selector_reserved_bytes(&selector);
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("tool artifact read budget lock poisoned"))?;
            if state.reads >= TOOL_ARTIFACT_READS_PER_TURN
                || state.bytes.saturating_add(reserved_bytes) > TOOL_ARTIFACT_READ_BYTES_PER_TURN
            {
                bail!("tool artifact per-turn read budget exhausted");
            }
            state.reads = state.reads.saturating_add(1);
            state.bytes = state.bytes.saturating_add(reserved_bytes);
        }
        let result = store.read_page(artifact_ref, selector);
        let actual_bytes = result.as_ref().map_or(0, |page| page.returned_bytes as u64);
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("tool artifact read budget lock poisoned"))?;
        state.bytes = state
            .bytes
            .saturating_sub(reserved_bytes.saturating_sub(actual_bytes));
        result
    }

    #[must_use]
    pub fn usage(&self) -> (u16, u64) {
        self.state
            .lock()
            .map(|state| (state.reads, state.bytes))
            .unwrap_or((
                TOOL_ARTIFACT_READS_PER_TURN,
                TOOL_ARTIFACT_READ_BYTES_PER_TURN,
            ))
    }
}

/// Process-local read result; a repeated immutable page does not need another model-body injection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolArtifactBudgetedReadV1 {
    pub page: ToolArtifactPageV1,
    pub deduplicated_from_call_id: Option<String>,
}

/// Durable body-free audit receipt for one artifact page read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolArtifactReadRecordedV1 {
    pub schema_version: u16,
    pub call_id: String,
    pub artifact_ref: ToolArtifactRefV1,
    pub source_descriptor_event_id: String,
    pub active_epoch_id: String,
    pub selector: ToolArtifactSelectorV1,
    pub returned_bytes: u32,
    pub page_sha256: String,
    pub artifact_sha256: String,
    pub outcome: ToolArtifactReadOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deduplicated_from_call_id: Option<String>,
}

impl ToolArtifactReadRecordedV1 {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != TOOL_ARTIFACT_READ_SCHEMA_VERSION
            || self.call_id.trim().is_empty()
            || self.source_descriptor_event_id.trim().is_empty()
            || self.active_epoch_id.trim().is_empty()
            || self.returned_bytes > TOOL_ARTIFACT_READ_MAX_BYTES
            || !self.page_sha256.starts_with("sha256:")
            || !self.artifact_sha256.starts_with("sha256:")
        {
            bail!("tool artifact read receipt is malformed");
        }
        self.artifact_ref.validate()?;
        self.selector.validate()?;
        match self.outcome {
            ToolArtifactReadOutcome::Returned if self.deduplicated_from_call_id.is_none() => {}
            ToolArtifactReadOutcome::Unchanged
                if self
                    .deduplicated_from_call_id
                    .as_deref()
                    .is_some_and(|call_id| {
                        !call_id.trim().is_empty() && call_id != self.call_id
                    }) => {}
            ToolArtifactReadOutcome::Unavailable
            | ToolArtifactReadOutcome::Rejected
            | ToolArtifactReadOutcome::Corrupt
                if self.deduplicated_from_call_id.is_none() => {}
            ToolArtifactReadOutcome::Returned
            | ToolArtifactReadOutcome::Unchanged
            | ToolArtifactReadOutcome::Unavailable
            | ToolArtifactReadOutcome::Rejected
            | ToolArtifactReadOutcome::Corrupt => {
                bail!("tool artifact read receipt dedupe state is inconsistent");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolArtifactAvailability {
    Available,
    Expired,
    Missing,
    HashMismatch,
    PolicyRevoked,
    Unavailable,
}

/// Manifest-only store inventory used by incremental reachability projection and GC.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolArtifactManifestEntryV1 {
    pub descriptor: ToolArtifactDescriptorV1,
    pub manifest_modified_at_unix_ms: u64,
}

/// Explicit mark roots supplied by durable projections and lifecycle owners.
///
/// GC consumes this bounded manifest projection and never scans the session JSONL.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolArtifactGcRootsV1 {
    pub active_result_refs: BTreeSet<ToolArtifactRefV1>,
    pub context_epoch_refs: BTreeSet<ToolArtifactRefV1>,
    pub unresolved_read_refs: BTreeSet<ToolArtifactRefV1>,
    pub fork_export_pins: BTreeSet<ToolArtifactRefV1>,
    pub verification_review_pins: BTreeSet<ToolArtifactRefV1>,
    pub explicit_holds: BTreeSet<ToolArtifactRefV1>,
}

impl ToolArtifactGcRootsV1 {
    /// Validates every opaque root reference.
    ///
    /// # Errors
    ///
    /// Returns an error when any root uses an unsupported or malformed ref.
    pub fn validate(&self) -> Result<()> {
        for artifact_ref in self.iter() {
            artifact_ref.validate()?;
        }
        Ok(())
    }

    #[must_use]
    pub fn contains(&self, artifact_ref: &ToolArtifactRefV1) -> bool {
        self.iter().any(|candidate| candidate == artifact_ref)
    }

    fn iter(&self) -> impl Iterator<Item = &ToolArtifactRefV1> {
        self.active_result_refs
            .iter()
            .chain(&self.context_epoch_refs)
            .chain(&self.unresolved_read_refs)
            .chain(&self.fork_export_pins)
            .chain(&self.verification_review_pins)
            .chain(&self.explicit_holds)
    }
}

/// Bounded outcome of one manifest-based mark-and-sweep pass.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolArtifactGcReportV1 {
    pub tombstone_id: String,
    pub scanned_manifests: usize,
    pub retained_manifests: usize,
    pub tombstoned_manifests: usize,
    pub tombstoned_blobs: usize,
    pub tombstoned_orphan_blobs: usize,
    pub tombstoned_staging_files: usize,
    pub tombstoned_bytes: u64,
    pub skipped_active_reads: usize,
    /// RFC-0062 9.4: artifacts actually moved to trash; callers append the durable
    /// DisabledPendingDelete -> Expired availability transitions for these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tombstoned_refs: Vec<ToolArtifactRefV1>,
}

/// Outcome of unlinking artifact-GC trash after a second grace boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolArtifactTrashPruneReportV1 {
    pub removed_tombstones: usize,
    pub removed_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct ToolArtifactBlobUsageLedgerV1 {
    schema_version: u16,
    bytes: u64,
    dirty: bool,
}

const TOOL_ARTIFACT_BLOB_USAGE_LEDGER_SCHEMA_VERSION: u16 = 1;

/// Local session-owned immutable artifact backend.
#[derive(Debug, Clone)]
pub struct ToolArtifactStore {
    session_scope_id: String,
    session_scope_id_hash: String,
    root: PathBuf,
}

impl ToolArtifactStore {
    /// RFC-0062 9.1: exposes the opaque session scope hash for capture plan construction.
    #[must_use]
    pub fn session_scope_id_hash(&self) -> &str {
        &self.session_scope_id_hash
    }

    /// Path of the session JSONL this artifact store belongs to. The artifact root lives under
    /// `<session-dir>/<stem>/artifacts`, so the JSONL is `<session-dir>/<stem>.jsonl`.
    #[must_use]
    pub fn session_log_path(&self) -> std::path::PathBuf {
        let stem_dir = self.root.parent();
        let session_dir = stem_dir.and_then(|dir| dir.parent());
        match (session_dir, stem_dir.and_then(|dir| dir.file_name())) {
            (Some(session_dir), Some(stem)) => {
                session_dir.join(format!("{}.jsonl", stem.to_string_lossy()))
            }
            _ => self.root.join("session.jsonl"),
        }
    }

    #[must_use]
    pub fn for_session_store(store: &JsonlSessionStore) -> Self {
        Self::for_session_path(store.path())
    }

    #[must_use]
    pub fn for_session_path(session_path: &Path) -> Self {
        let normalized_path = if session_path.exists() {
            fs::canonicalize(session_path).unwrap_or_else(|_| session_path.to_path_buf())
        } else {
            session_path
                .parent()
                .and_then(|parent| fs::canonicalize(parent).ok())
                .and_then(|parent| session_path.file_name().map(|name| parent.join(name)))
                .unwrap_or_else(|| session_path.to_path_buf())
        };
        let session_path = normalized_path.as_path();
        let session_scope_id = session_id_for_path(session_path);
        let stem = session_path
            .file_stem()
            .filter(|stem| !stem.is_empty())
            .unwrap_or_else(|| std::ffi::OsStr::new("session"));
        let root = session_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(stem)
            .join("artifacts");
        Self {
            session_scope_id_hash: stable_event_hash(session_scope_id.as_bytes()),
            session_scope_id,
            root,
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn session_scope_id(&self) -> &str {
        &self.session_scope_id
    }

    pub fn capture_text(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        text: &str,
        sensitivity: ToolArtifactSensitivity,
    ) -> Result<ToolArtifactDescriptorV1> {
        let safe = safe_persistence_text(text);
        let redaction_count = u32::from(safe != text);
        self.capture_policy_safe_bytes(
            tool_call_id,
            tool_name,
            safe.as_bytes(),
            text.len() as u64,
            "text/plain; charset=utf-8",
            ToolArtifactEncoding::Utf8,
            sensitivity,
            redaction_count,
        )
    }

    pub fn capture_policy_safe_bytes(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        bytes: &[u8],
        observed_bytes: u64,
        media_type: &str,
        encoding: ToolArtifactEncoding,
        sensitivity: ToolArtifactSensitivity,
        redaction_count: u32,
    ) -> Result<ToolArtifactDescriptorV1> {
        if tool_call_id.trim().is_empty()
            || tool_name.trim().is_empty()
            || media_type.trim().is_empty()
        {
            bail!("tool artifact capture identity is malformed");
        }
        let bounded = bounded_artifact_bytes(bytes);
        self.publish_descriptor(
            tool_call_id,
            tool_name,
            bounded,
            observed_bytes,
            bytes.len() as u64,
            media_type,
            encoding,
            sensitivity,
            redaction_count,
        )
    }

    /// Copies one immutable source artifact into this session with a new opaque logical ref.
    ///
    /// The source and destination descriptors retain the same content binding and completeness,
    /// but the destination receives its own session scope and cannot resolve the source ref.
    ///
    /// # Errors
    ///
    /// Returns an error for a same-session fork, unavailable/corrupt source, or failed publication.
    pub fn fork_descriptor_from(
        &self,
        source_store: &ToolArtifactStore,
        source: &ToolArtifactDescriptorV1,
    ) -> Result<ToolArtifactDescriptorV1> {
        source_store.validate_session_descriptor(source)?;
        if source_store.session_scope_id_hash == self.session_scope_id_hash {
            bail!("tool artifact fork requires a distinct destination session scope");
        }
        let bytes = source_store.read_all(source)?;
        if bytes.len() as u64 != source.persisted_bytes
            || stable_event_hash(&bytes) != source.content_sha256
        {
            bail!("tool artifact source changed while fork was being prepared");
        }
        self.publish_blob(&source.content_sha256, &bytes)?;
        let descriptor = ToolArtifactDescriptorV1 {
            schema_version: TOOL_ARTIFACT_DESCRIPTOR_SCHEMA_VERSION,
            artifact_ref: ToolArtifactRefV1::random(),
            session_scope_id_hash: self.session_scope_id_hash.clone(),
            tool_call_id: source.tool_call_id.clone(),
            tool_name: source.tool_name.clone(),
            content_sha256: source.content_sha256.clone(),
            observed_bytes: source.observed_bytes,
            policy_projected_bytes: source.policy_projected_bytes,
            persisted_bytes: source.persisted_bytes,
            media_type: source.media_type.clone(),
            encoding: source.encoding,
            completeness: source.completeness.clone(),
            sensitivity: source.sensitivity,
            retention_class: ToolArtifactRetentionClass::SessionBound,
            retrieval_policy: source.retrieval_policy,
        };
        descriptor.validate()?;
        self.publish_descriptor_manifest(&descriptor)?;
        Ok(descriptor)
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_descriptor(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        bounded: BoundedArtifactBytes,
        observed_bytes: u64,
        policy_projected_bytes: u64,
        media_type: &str,
        encoding: ToolArtifactEncoding,
        sensitivity: ToolArtifactSensitivity,
        redaction_count: u32,
    ) -> Result<ToolArtifactDescriptorV1> {
        if tool_call_id.trim().is_empty()
            || tool_name.trim().is_empty()
            || media_type.trim().is_empty()
        {
            bail!("tool artifact capture identity is malformed");
        }
        let content_sha256 = stable_event_hash(&bounded.bytes);
        self.publish_blob(&content_sha256, &bounded.bytes)?;
        let truncation = bounded.truncation(policy_projected_bytes);
        let completeness = match (redaction_count, truncation) {
            (0, None) => ToolArtifactCompleteness::Complete,
            (count, None) => ToolArtifactCompleteness::PolicyRedacted {
                redaction_count: count,
                storage_truncation: None,
            },
            (0, Some(truncation)) => ToolArtifactCompleteness::StorageTruncated(truncation),
            (count, Some(truncation)) => ToolArtifactCompleteness::PolicyRedacted {
                redaction_count: count,
                storage_truncation: Some(truncation),
            },
        };
        let descriptor = ToolArtifactDescriptorV1 {
            schema_version: TOOL_ARTIFACT_DESCRIPTOR_SCHEMA_VERSION,
            artifact_ref: ToolArtifactRefV1::random(),
            session_scope_id_hash: self.session_scope_id_hash.clone(),
            tool_call_id: safe_persistence_text(tool_call_id),
            tool_name: safe_persistence_text(tool_name),
            content_sha256,
            observed_bytes,
            policy_projected_bytes,
            persisted_bytes: bounded.bytes.len() as u64,
            media_type: media_type.to_owned(),
            encoding,
            completeness,
            sensitivity,
            retention_class: ToolArtifactRetentionClass::SessionBound,
            retrieval_policy: if encoding == ToolArtifactEncoding::Utf8 {
                ToolArtifactRetrievalPolicyV1::ModelAndDisplay
            } else {
                ToolArtifactRetrievalPolicyV1::DisplayOnly
            },
        };
        descriptor.validate()?;
        self.publish_descriptor_manifest(&descriptor)?;
        Ok(descriptor)
    }

    /// Starts a bounded sink for output that has already passed persistence policy.
    ///
    /// The sink retains at most the artifact hard limit while counting all observed bytes.
    #[must_use]
    pub fn begin_policy_safe_capture(
        &self,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        media_type: impl Into<String>,
        encoding: ToolArtifactEncoding,
        sensitivity: ToolArtifactSensitivity,
    ) -> ToolArtifactCaptureSink {
        ToolArtifactCaptureSink {
            store: self.clone(),
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            media_type: media_type.into(),
            encoding,
            sensitivity,
            head: Vec::with_capacity(TOOL_ARTIFACT_MAX_BYTES / 2),
            tail: VecDeque::with_capacity(TOOL_ARTIFACT_MAX_BYTES / 2),
            observed_bytes: 0,
            process_write_failed: false,
            process: None,
        }
    }

    pub fn read_all(&self, descriptor: &ToolArtifactDescriptorV1) -> Result<Vec<u8>> {
        self.validate_session_descriptor(descriptor)?;
        let read_lock = self.open_ref_lock(&descriptor.artifact_ref)?;
        read_lock
            .try_lock_shared()
            .context("tool artifact is being retired")?;
        let path = self.blob_path(&descriptor.content_sha256)?;
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("failed to inspect tool artifact {}", path.display()))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            bail!("tool artifact blob is not a plain file");
        }
        if metadata.len() > TOOL_ARTIFACT_MAX_BYTES as u64 {
            bail!("tool artifact blob exceeds its hard limit");
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        File::open(&path)
            .with_context(|| format!("failed to open tool artifact {}", path.display()))?
            .take(TOOL_ARTIFACT_MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed to read tool artifact {}", path.display()))?;
        if stable_event_hash(&bytes) != descriptor.content_sha256 {
            bail!("tool artifact content hash mismatch");
        }
        Ok(bytes)
    }

    /// Resolves an opaque session-scoped reference without consulting or scanning JSONL.
    pub fn resolve(&self, artifact_ref: &ToolArtifactRefV1) -> Result<ToolArtifactDescriptorV1> {
        artifact_ref.validate()?;
        let path = self.ref_path(artifact_ref)?;
        let metadata = fs::symlink_metadata(&path).with_context(|| {
            format!(
                "failed to inspect tool artifact ref {}",
                artifact_ref.artifact_id
            )
        })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 16 * 1024 {
            bail!("tool artifact ref manifest is not a bounded plain file");
        }
        let bytes = fs::read(&path).with_context(|| {
            format!(
                "failed to read tool artifact ref {}",
                artifact_ref.artifact_id
            )
        })?;
        let descriptor: ToolArtifactDescriptorV1 = serde_json::from_slice(&bytes)
            .context("failed to decode tool artifact ref manifest")?;
        self.validate_session_descriptor(&descriptor)?;
        if descriptor.artifact_ref != *artifact_ref {
            bail!("tool artifact ref manifest identity mismatch");
        }
        Ok(descriptor)
    }

    /// Reads the bounded descriptor manifest inventory without opening the session JSONL.
    ///
    /// # Errors
    ///
    /// Returns an error for an oversized, malformed, symlinked, or cross-session manifest.
    pub fn manifest_inventory(&self) -> Result<Vec<ToolArtifactManifestEntryV1>> {
        let refs_dir = self.root.join("refs");
        let entries = match fs::read_dir(&refs_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read {}", refs_dir.display()));
            }
        };
        let mut manifests = Vec::new();
        for entry in entries {
            let entry = entry.with_context(|| format!("failed to read {}", refs_dir.display()))?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            if manifests.len() == TOOL_ARTIFACT_MANIFEST_MAX_ENTRIES {
                bail!("tool artifact manifest inventory exceeds its entry limit");
            }
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("failed to inspect {}", path.display()))?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() > 16 * 1024
            {
                bail!("tool artifact inventory contains an unsafe manifest");
            }
            let bytes =
                fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
            let descriptor: ToolArtifactDescriptorV1 = serde_json::from_slice(&bytes)
                .context("failed to decode tool artifact inventory manifest")?;
            self.validate_session_descriptor(&descriptor)?;
            let expected_path = self.ref_path(&descriptor.artifact_ref)?;
            if expected_path != path {
                bail!("tool artifact inventory manifest path does not match its identity");
            }
            manifests.push(ToolArtifactManifestEntryV1 {
                descriptor,
                manifest_modified_at_unix_ms: metadata_modified_at_unix_ms(&metadata),
            });
        }
        manifests.sort_by(|left, right| {
            left.descriptor
                .artifact_ref
                .cmp(&right.descriptor.artifact_ref)
        });
        Ok(manifests)
    }

    /// Moves unreachable, grace-expired manifests and blobs into immutable trash.
    ///
    /// The caller supplies roots from incremental projections. This function never reads JSONL and
    /// skips refs that are concurrently held by an artifact reader.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid roots, a grace shorter than 24 hours, unsafe store entries, or
    /// a failed tombstone move.
    /// RFC-0062 9.4: returns the artifacts whose manifests are unreferenced and past the grace
    /// period, so callers can append durable `Available -> DisabledPendingDelete` transitions
    /// BEFORE the body is deleted. Pure computation; never mutates the store.
    pub fn grace_expired_artifact_refs(
        &self,
        roots: &ToolArtifactGcRootsV1,
        now_unix_ms: u64,
        orphan_grace_ms: u64,
    ) -> Result<Vec<ToolArtifactRefV1>> {
        roots.validate()?;
        if orphan_grace_ms < TOOL_ARTIFACT_ORPHAN_GRACE_MS {
            bail!("tool artifact orphan grace must be at least 24 hours");
        }
        let inventory = self.manifest_inventory()?;
        let mut expired = Vec::new();
        for entry in &inventory {
            let descriptor = &entry.descriptor;
            let protected = roots.contains(&descriptor.artifact_ref)
                || descriptor.retention_class == ToolArtifactRetentionClass::Pinned;
            let grace_elapsed =
                now_unix_ms.saturating_sub(entry.manifest_modified_at_unix_ms) >= orphan_grace_ms;
            if !protected && grace_elapsed {
                expired.push(descriptor.artifact_ref.clone());
            }
        }
        Ok(expired)
    }

    pub fn garbage_collect(
        &self,
        roots: &ToolArtifactGcRootsV1,
        now_unix_ms: u64,
        orphan_grace_ms: u64,
    ) -> Result<ToolArtifactGcReportV1> {
        roots.validate()?;
        if orphan_grace_ms < TOOL_ARTIFACT_ORPHAN_GRACE_MS {
            bail!("tool artifact orphan grace must be at least 24 hours");
        }
        let inventory = self.manifest_inventory()?;
        let tombstone_id = format!("tool-artifact-gc-{}", Uuid::new_v4().simple());
        let mut candidates = Vec::new();
        let mut retained_manifests = 0usize;
        for entry in &inventory {
            let descriptor = &entry.descriptor;
            let protected = roots.contains(&descriptor.artifact_ref)
                || descriptor.retention_class == ToolArtifactRetentionClass::Pinned;
            let grace_elapsed =
                now_unix_ms.saturating_sub(entry.manifest_modified_at_unix_ms) >= orphan_grace_ms;
            if protected || !grace_elapsed {
                retained_manifests = retained_manifests.saturating_add(1);
            } else {
                candidates.push(entry.clone());
            }
        }
        let trash_root = self.root.join("trash").join(&tombstone_id);
        let trash_refs = trash_root.join("refs");
        let trash_blobs = trash_root.join("blobs");
        let trash_staging = trash_root.join("staging");
        self.ensure_root_dir()?;
        create_private_dir(&self.root.join("trash"))?;
        create_private_dir(&self.root.join("refs"))?;
        create_private_dir(&trash_root)?;
        create_private_dir(&trash_refs)?;
        create_private_dir(&trash_blobs)?;
        create_private_dir(&trash_staging)?;

        let mut tombstoned_manifests = 0usize;
        let mut tombstoned_refs = Vec::new();
        let mut skipped_active_reads = 0usize;
        for entry in candidates {
            let artifact_ref = &entry.descriptor.artifact_ref;
            let lock = self.open_ref_lock(artifact_ref)?;
            match lock.try_lock() {
                Ok(()) => {}
                Err(fs::TryLockError::WouldBlock) => {
                    skipped_active_reads = skipped_active_reads.saturating_add(1);
                    retained_manifests = retained_manifests.saturating_add(1);
                    continue;
                }
                Err(fs::TryLockError::Error(error)) => {
                    return Err(error).context("failed to lock tool artifact for GC");
                }
            }
            let source = self.ref_path(artifact_ref)?;
            let destination = trash_refs.join(
                source
                    .file_name()
                    .context("tool artifact manifest has no file name")?,
            );
            fs::rename(&source, &destination).with_context(|| {
                format!(
                    "failed to tombstone tool artifact manifest {}",
                    artifact_ref.artifact_id
                )
            })?;
            let source_binding = self
                .root
                .join("refs")
                .join(format!("{}.event", artifact_ref.artifact_id));
            if source_binding.exists() {
                let destination_binding =
                    trash_refs.join(format!("{}.event", artifact_ref.artifact_id));
                fs::rename(&source_binding, &destination_binding).with_context(|| {
                    format!(
                        "failed to tombstone tool artifact source binding {}",
                        artifact_ref.artifact_id
                    )
                })?;
            }
            tombstoned_manifests = tombstoned_manifests.saturating_add(1);
            tombstoned_refs.push(entry.descriptor.artifact_ref.clone());
        }
        sync_dir(&self.root.join("refs"))?;
        sync_dir(&trash_refs)?;

        let usage_lock = self.open_blob_usage_lock()?;
        usage_lock
            .lock()
            .context("failed to lock tool artifact blob usage ledger for GC")?;
        let usage_before_gc = self.load_or_reconcile_blob_usage()?;
        self.persist_blob_usage_ledger(ToolArtifactBlobUsageLedgerV1 {
            schema_version: TOOL_ARTIFACT_BLOB_USAGE_LEDGER_SCHEMA_VERSION,
            bytes: usage_before_gc,
            dirty: true,
        })?;

        let live_hashes = self
            .manifest_inventory()?
            .into_iter()
            .map(|entry| entry.descriptor.content_sha256)
            .collect::<BTreeSet<_>>();
        let mut tombstoned_blobs = 0usize;
        let mut tombstoned_orphan_blobs = 0usize;
        let mut tombstoned_staging_files = 0usize;
        let mut tombstoned_bytes = 0u64;
        for (source, bytes) in
            self.grace_expired_orphan_blobs(&live_hashes, now_unix_ms, orphan_grace_ms)?
        {
            let destination = trash_blobs.join(
                source
                    .file_name()
                    .context("tool artifact blob has no file name")?,
            );
            fs::rename(&source, &destination)
                .with_context(|| format!("failed to tombstone {}", source.display()))?;
            tombstoned_blobs = tombstoned_blobs.saturating_add(1);
            tombstoned_orphan_blobs = tombstoned_orphan_blobs.saturating_add(1);
            tombstoned_bytes = tombstoned_bytes.saturating_add(bytes);
        }
        for (source, bytes) in self.grace_expired_staging_files(now_unix_ms, orphan_grace_ms)? {
            let destination = trash_staging.join(
                source
                    .file_name()
                    .context("tool artifact staging file has no file name")?,
            );
            fs::rename(&source, &destination)
                .with_context(|| format!("failed to tombstone {}", source.display()))?;
            tombstoned_staging_files = tombstoned_staging_files.saturating_add(1);
            tombstoned_bytes = tombstoned_bytes.saturating_add(bytes);
        }
        sync_dir(&trash_blobs)?;
        sync_dir(&trash_staging)?;
        sync_dir(&trash_root)?;
        let usage_after_gc = directory_file_bytes(&self.root.join("blobs"))?;
        self.persist_blob_usage_ledger(ToolArtifactBlobUsageLedgerV1 {
            schema_version: TOOL_ARTIFACT_BLOB_USAGE_LEDGER_SCHEMA_VERSION,
            bytes: usage_after_gc,
            dirty: false,
        })?;
        if tombstoned_manifests == 0 && tombstoned_blobs == 0 && tombstoned_staging_files == 0 {
            fs::remove_dir_all(&trash_root)
                .with_context(|| format!("failed to remove empty {}", trash_root.display()))?;
            sync_dir(&self.root.join("trash"))?;
        }
        Ok(ToolArtifactGcReportV1 {
            tombstone_id,
            scanned_manifests: inventory.len(),
            retained_manifests,
            tombstoned_manifests,
            tombstoned_blobs,
            tombstoned_orphan_blobs,
            tombstoned_staging_files,
            tombstoned_bytes,
            skipped_active_reads,
            tombstoned_refs,
        })
    }

    /// Permanently unlinks GC trash only after the mandatory grace period.
    ///
    /// # Errors
    ///
    /// Returns an error for a shorter grace, unsafe trash entries, or an I/O failure.
    pub fn prune_garbage_trash(
        &self,
        now_unix_ms: u64,
        trash_grace_ms: u64,
    ) -> Result<ToolArtifactTrashPruneReportV1> {
        if trash_grace_ms < TOOL_ARTIFACT_ORPHAN_GRACE_MS {
            bail!("tool artifact trash grace must be at least 24 hours");
        }
        let trash = self.root.join("trash");
        let entries = match fs::read_dir(&trash) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ToolArtifactTrashPruneReportV1 {
                    removed_tombstones: 0,
                    removed_bytes: 0,
                });
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", trash.display()));
            }
        };
        let mut removed_tombstones = 0usize;
        let mut removed_bytes = 0u64;
        for entry in entries {
            let entry = entry.with_context(|| format!("failed to read {}", trash.display()))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("failed to inspect {}", path.display()))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("tool artifact trash contains an unsafe entry");
            }
            if now_unix_ms.saturating_sub(metadata_modified_at_unix_ms(&metadata)) < trash_grace_ms
            {
                continue;
            }
            let bytes = safe_directory_file_bytes(&path)?;
            fs::remove_dir_all(&path)
                .with_context(|| format!("failed to prune {}", path.display()))?;
            removed_tombstones = removed_tombstones.saturating_add(1);
            removed_bytes = removed_bytes.saturating_add(bytes);
        }
        sync_dir(&trash)?;
        Ok(ToolArtifactTrashPruneReportV1 {
            removed_tombstones,
            removed_bytes,
        })
    }

    /// Writes a rebuildable lifecycle cache for fork/GC bookkeeping.
    ///
    /// This sidecar is not retrieval authority. Model, TUI, HTTP, and Desktop reads must be
    /// authorized by the active durable pressure projection, including descriptor hash, byte
    /// count, call identity, tool identity, and availability.
    pub fn bind_source_event(
        &self,
        artifact_ref: &ToolArtifactRefV1,
        source_event_id: &str,
    ) -> Result<()> {
        artifact_ref.validate()?;
        if source_event_id.trim().is_empty() || source_event_id.len() > 256 {
            bail!("tool artifact source event id is malformed");
        }
        let refs_dir = self.root.join("refs");
        self.ensure_root_dir()?;
        create_private_dir(&refs_dir)?;
        let path = refs_dir.join(format!("{}.event", artifact_ref.artifact_id));
        publish_private_noclobber(&refs_dir, &path, source_event_id.as_bytes())?;
        sync_dir(&refs_dir)
    }

    /// Reads the rebuildable lifecycle binding used by fork/GC code.
    ///
    /// Callers must not treat the returned event id as sufficient authorization to read a blob.
    pub fn source_event_id(&self, artifact_ref: &ToolArtifactRefV1) -> Result<String> {
        artifact_ref.validate()?;
        let path = self
            .root
            .join("refs")
            .join(format!("{}.event", artifact_ref.artifact_id));
        let metadata = fs::symlink_metadata(&path).with_context(|| {
            format!(
                "failed to inspect source binding {}",
                artifact_ref.artifact_id
            )
        })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 256 {
            bail!("tool artifact source binding is not a bounded plain file");
        }
        let value = fs::read_to_string(&path).with_context(|| {
            format!("failed to read source binding {}", artifact_ref.artifact_id)
        })?;
        if value.trim().is_empty() {
            bail!("tool artifact source binding is empty");
        }
        Ok(value)
    }

    /// Reads one bounded typed page by opaque reference.
    pub fn read_page(
        &self,
        artifact_ref: &ToolArtifactRefV1,
        selector: ToolArtifactSelectorV1,
    ) -> Result<ToolArtifactPageV1> {
        selector.validate()?;
        let read_lock = self.open_ref_lock(artifact_ref)?;
        read_lock
            .try_lock_shared()
            .context("tool artifact is being retired")?;
        let descriptor = self.resolve(artifact_ref)?;
        let path = self.blob_path(&descriptor.content_sha256)?;
        let blob_hash = hash_file(&path, TOOL_ARTIFACT_MAX_BYTES as u64)
            .with_context(|| format!("failed to verify tool artifact {}", path.display()))?;
        if blob_hash != descriptor.content_sha256 {
            bail!("tool artifact content hash mismatch");
        }
        let selected = match &selector {
            ToolArtifactSelectorV1::ByteSlice { offset, limit } => {
                read_byte_slice(&path, *offset, *limit)?
            }
            ToolArtifactSelectorV1::LinePage {
                start_line,
                line_count,
            } => {
                if descriptor.encoding != ToolArtifactEncoding::Utf8 {
                    bail!("line paging requires a UTF-8 tool artifact");
                }
                read_line_page(&path, *start_line, *line_count)?
            }
            ToolArtifactSelectorV1::SearchLiteral {
                query,
                start_offset,
                max_matches,
                context_lines,
            } => {
                if descriptor.encoding != ToolArtifactEncoding::Utf8 {
                    bail!("literal search requires a UTF-8 tool artifact");
                }
                read_literal_search(&path, query, *start_offset, *max_matches, *context_lines)?
            }
        };
        let (body, body_encoding) = if descriptor.encoding == ToolArtifactEncoding::Utf8 {
            match String::from_utf8(selected.bytes.clone()) {
                Ok(body) => (body, ToolArtifactPageEncoding::Utf8),
                Err(_) => (
                    BASE64_STANDARD.encode(&selected.bytes),
                    ToolArtifactPageEncoding::Base64,
                ),
            }
        } else {
            (
                BASE64_STANDARD.encode(&selected.bytes),
                ToolArtifactPageEncoding::Base64,
            )
        };
        let page = ToolArtifactPageV1 {
            artifact_ref: artifact_ref.clone(),
            selector,
            body,
            body_encoding,
            returned_bytes: selected.bytes.len() as u32,
            page_sha256: stable_event_hash(&selected.bytes),
            artifact_sha256: descriptor.content_sha256,
            eof: selected.eof,
            match_count: selected.match_count,
            next_selector: selected.next_selector,
        };
        page.validate()?;
        Ok(page)
    }

    pub fn availability(&self, descriptor: &ToolArtifactDescriptorV1) -> ToolArtifactAvailability {
        if descriptor.validate().is_err()
            || descriptor.session_scope_id_hash != self.session_scope_id_hash
        {
            return ToolArtifactAvailability::PolicyRevoked;
        }
        let Ok(path) = self.blob_path(&descriptor.content_sha256) else {
            return ToolArtifactAvailability::PolicyRevoked;
        };
        if !path.exists() {
            return ToolArtifactAvailability::Missing;
        }
        match hash_file(&path, TOOL_ARTIFACT_MAX_BYTES as u64) {
            Ok(hash) if hash == descriptor.content_sha256 => ToolArtifactAvailability::Available,
            Ok(_) => ToolArtifactAvailability::HashMismatch,
            Err(_) => ToolArtifactAvailability::HashMismatch,
        }
    }

    fn validate_session_descriptor(&self, descriptor: &ToolArtifactDescriptorV1) -> Result<()> {
        descriptor.validate()?;
        if descriptor.session_scope_id_hash != self.session_scope_id_hash {
            bail!("tool artifact belongs to a different session scope");
        }
        if !descriptor.retrieval_available() {
            bail!("tool artifact is unavailable for retrieval");
        }
        Ok(())
    }

    fn ensure_root_dir(&self) -> Result<()> {
        let session_dir = self
            .root
            .parent()
            .context("tool artifact root has no session directory")?;
        create_private_dir(session_dir)?;
        create_private_dir(&self.root)
    }

    fn open_ref_lock(&self, artifact_ref: &ToolArtifactRefV1) -> Result<File> {
        artifact_ref.validate()?;
        let locks_dir = self.root.join("locks");
        self.ensure_root_dir()?;
        create_private_dir(&locks_dir)?;
        let path = locks_dir.join(format!("{}.lock", artifact_ref.artifact_id));
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(&path)
            .with_context(|| format!("failed to open tool artifact lock {}", path.display()))?;
        let metadata = file
            .metadata()
            .with_context(|| format!("failed to inspect tool artifact lock {}", path.display()))?;
        if !metadata.is_file() {
            bail!("tool artifact lock is not a plain file");
        }
        harden_private_file(&path)?;
        Ok(file)
    }

    fn publish_blob(&self, content_sha256: &str, bytes: &[u8]) -> Result<()> {
        if bytes.len() > TOOL_ARTIFACT_MAX_BYTES {
            bail!("tool artifact blob exceeds its hard limit");
        }
        let blob_path = self.blob_path(content_sha256)?;
        let blob_dir = blob_path
            .parent()
            .context("tool artifact blob path has no parent")?;
        let staging_dir = self.root.join("staging");
        self.ensure_root_dir()?;
        create_private_dir(&self.root.join("blobs"))?;
        create_private_dir(blob_dir)?;
        create_private_dir(&staging_dir)?;
        let usage_lock = self.open_blob_usage_lock()?;
        usage_lock
            .lock()
            .context("failed to lock tool artifact blob usage ledger")?;
        if blob_path.exists() {
            if hash_file(&blob_path, TOOL_ARTIFACT_MAX_BYTES as u64)? != content_sha256 {
                bail!("existing tool artifact blob hash mismatch");
            }
            return Ok(());
        }
        let current_usage = self.load_or_reconcile_blob_usage()?;
        if current_usage.saturating_add(bytes.len() as u64) > TOOL_ARTIFACT_SESSION_BUDGET_BYTES {
            bail!(
                "tool artifact session budget exceeded: {} + {} > {}",
                current_usage,
                bytes.len(),
                TOOL_ARTIFACT_SESSION_BUDGET_BYTES
            );
        }
        let reserved_usage = current_usage.saturating_add(bytes.len() as u64);
        // Reserve before publishing. A process crash may conservatively over-count until the
        // event-driven GC/recovery reconciliation, but can never under-count committed bytes.
        // Ordinary returned failures roll this reservation back under the same exclusive lock.
        self.persist_blob_usage_ledger(ToolArtifactBlobUsageLedgerV1 {
            schema_version: TOOL_ARTIFACT_BLOB_USAGE_LEDGER_SCHEMA_VERSION,
            bytes: reserved_usage,
            dirty: false,
        })?;
        let publish_result = (|| -> Result<()> {
            let staging_path = staging_dir.join(format!("{}.part", Uuid::new_v4().simple()));
            let mut staging = create_private_file(&staging_path)?;
            staging
                .write_all(bytes)
                .with_context(|| format!("failed to write {}", staging_path.display()))?;
            staging
                .sync_all()
                .with_context(|| format!("failed to sync {}", staging_path.display()))?;
            drop(staging);
            match fs::hard_link(&staging_path, &blob_path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if hash_file(&blob_path, TOOL_ARTIFACT_MAX_BYTES as u64)? != content_sha256 {
                        let _ = fs::remove_file(&staging_path);
                        bail!("racing tool artifact blob hash mismatch");
                    }
                }
                Err(error) => {
                    let _ = fs::remove_file(&staging_path);
                    return Err(error).with_context(|| {
                        format!("failed to publish tool artifact {}", blob_path.display())
                    });
                }
            }
            let _ = fs::remove_file(&staging_path);
            harden_private_file(&blob_path)?;
            sync_dir(blob_dir)
        })();
        if let Err(error) = publish_result {
            let rollback = self.persist_blob_usage_ledger(ToolArtifactBlobUsageLedgerV1 {
                schema_version: TOOL_ARTIFACT_BLOB_USAGE_LEDGER_SCHEMA_VERSION,
                bytes: current_usage,
                dirty: false,
            });
            return match rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(error.context(format!(
                    "tool artifact quota rollback also failed: {rollback_error:#}"
                ))),
            };
        }
        Ok(())
    }

    fn open_blob_usage_lock(&self) -> Result<File> {
        self.ensure_root_dir()?;
        let path = self.root.join("usage.lock");
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        let metadata = file
            .metadata()
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if !metadata.is_file() {
            bail!("tool artifact blob usage lock is not a plain file");
        }
        harden_private_file(&path)?;
        Ok(file)
    }

    fn load_or_reconcile_blob_usage(&self) -> Result<u64> {
        let path = self.root.join("usage.json");
        let ledger = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || metadata.len() > 1_024
                {
                    bail!("tool artifact blob usage ledger is not a bounded plain file");
                }
                fs::read(&path)
                    .ok()
                    .and_then(|bytes| {
                        serde_json::from_slice::<ToolArtifactBlobUsageLedgerV1>(&bytes).ok()
                    })
                    .filter(|ledger| {
                        ledger.schema_version == TOOL_ARTIFACT_BLOB_USAGE_LEDGER_SCHEMA_VERSION
                    })
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
            }
        };
        if let Some(ledger) = ledger.filter(|ledger| !ledger.dirty) {
            return Ok(ledger.bytes);
        }
        let bytes = directory_file_bytes(&self.root.join("blobs"))?;
        self.persist_blob_usage_ledger(ToolArtifactBlobUsageLedgerV1 {
            schema_version: TOOL_ARTIFACT_BLOB_USAGE_LEDGER_SCHEMA_VERSION,
            bytes,
            dirty: false,
        })?;
        Ok(bytes)
    }

    fn persist_blob_usage_ledger(&self, ledger: ToolArtifactBlobUsageLedgerV1) -> Result<()> {
        let bytes =
            serde_json::to_vec(&ledger).context("failed to encode tool artifact blob usage")?;
        crate::atomic_publish_private_file(&self.root.join("usage.json"), &bytes)
            .context("failed to publish tool artifact blob usage ledger")
    }

    fn publish_descriptor_manifest(&self, descriptor: &ToolArtifactDescriptorV1) -> Result<()> {
        let path = self.ref_path(&descriptor.artifact_ref)?;
        let refs_dir = path
            .parent()
            .context("tool artifact ref path has no parent")?;
        self.ensure_root_dir()?;
        create_private_dir(refs_dir)?;
        let bytes =
            serde_json::to_vec(descriptor).context("failed to encode tool artifact manifest")?;
        publish_private_noclobber(refs_dir, &path, &bytes)?;
        sync_dir(refs_dir)
    }

    fn ref_path(&self, artifact_ref: &ToolArtifactRefV1) -> Result<PathBuf> {
        artifact_ref.validate()?;
        Ok(self
            .root
            .join("refs")
            .join(format!("{}.json", artifact_ref.artifact_id)))
    }

    fn blob_path(&self, content_sha256: &str) -> Result<PathBuf> {
        let digest = content_sha256
            .strip_prefix("sha256:")
            .context("tool artifact content hash has an unsupported format")?;
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("tool artifact content hash is malformed");
        }
        Ok(self
            .root
            .join("blobs")
            .join(&digest[..2])
            .join(format!("{digest}.blob")))
    }

    fn grace_expired_orphan_blobs(
        &self,
        live_hashes: &BTreeSet<String>,
        now_unix_ms: u64,
        orphan_grace_ms: u64,
    ) -> Result<Vec<(PathBuf, u64)>> {
        let blobs_root = self.root.join("blobs");
        let prefix_entries = match fs::read_dir(&blobs_root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to scan {}", blobs_root.display()));
            }
        };
        let mut candidates = Vec::new();
        let mut entries_seen = 0usize;
        for prefix_entry in prefix_entries {
            let prefix_entry =
                prefix_entry.with_context(|| format!("failed to scan {}", blobs_root.display()))?;
            let prefix_path = prefix_entry.path();
            let prefix_metadata = fs::symlink_metadata(&prefix_path)
                .with_context(|| format!("failed to inspect {}", prefix_path.display()))?;
            if prefix_metadata.file_type().is_symlink() || !prefix_metadata.is_dir() {
                bail!("tool artifact blob store contains an unsafe prefix entry");
            }
            for blob_entry in fs::read_dir(&prefix_path)
                .with_context(|| format!("failed to scan {}", prefix_path.display()))?
            {
                let blob_entry = blob_entry
                    .with_context(|| format!("failed to scan {}", prefix_path.display()))?;
                entries_seen = entries_seen.saturating_add(1);
                if entries_seen > TOOL_ARTIFACT_MANIFEST_MAX_ENTRIES {
                    bail!("tool artifact blob inventory exceeds its entry limit");
                }
                let path = blob_entry.path();
                let metadata = fs::symlink_metadata(&path)
                    .with_context(|| format!("failed to inspect {}", path.display()))?;
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || metadata.len() > TOOL_ARTIFACT_MAX_BYTES as u64
                {
                    bail!("tool artifact blob inventory contains an unsafe file");
                }
                let digest = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .context("tool artifact blob has a non-UTF-8 digest")?;
                let content_sha256 = format!("sha256:{digest}");
                if self.blob_path(&content_sha256)? != path {
                    bail!("tool artifact blob path does not match its digest");
                }
                if live_hashes.contains(&content_sha256)
                    || now_unix_ms.saturating_sub(metadata_modified_at_unix_ms(&metadata))
                        < orphan_grace_ms
                {
                    continue;
                }
                candidates.push((path, metadata.len()));
            }
        }
        Ok(candidates)
    }

    fn grace_expired_staging_files(
        &self,
        now_unix_ms: u64,
        orphan_grace_ms: u64,
    ) -> Result<Vec<(PathBuf, u64)>> {
        let staging_root = self.root.join("staging");
        let entries = match fs::read_dir(&staging_root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to scan {}", staging_root.display()));
            }
        };
        let mut candidates = Vec::new();
        for (index, entry) in entries.enumerate() {
            if index >= TOOL_ARTIFACT_MANIFEST_MAX_ENTRIES {
                bail!("tool artifact staging inventory exceeds its entry limit");
            }
            let entry =
                entry.with_context(|| format!("failed to scan {}", staging_root.display()))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("failed to inspect {}", path.display()))?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("part")
                || metadata.len() > TOOL_ARTIFACT_MAX_BYTES as u64
            {
                bail!("tool artifact staging inventory contains an unsafe file");
            }
            if now_unix_ms.saturating_sub(metadata_modified_at_unix_ms(&metadata))
                >= orphan_grace_ms
            {
                candidates.push((path, metadata.len()));
            }
        }
        Ok(candidates)
    }
}

/// Streaming bounded capture for policy-safe bytes.
pub struct ToolArtifactCaptureSink {
    store: ToolArtifactStore,
    tool_call_id: String,
    tool_name: String,
    media_type: String,
    encoding: ToolArtifactEncoding,
    sensitivity: ToolArtifactSensitivity,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    observed_bytes: u64,
    /// RFC-0062 10.2/16.2: set when a staging write failed; settlement must mark storage
    /// Unavailable instead of claiming a complete artifact.
    process_write_failed: bool,
    /// RFC-0062 9.2: process-backed dual-stream staging. Each stream gets its own bounded
    /// staging file so the canonical artifact is stdout-then-stderr regardless of reader
    /// scheduling; the sink only enters this mode through `begin_process_capture`.
    process: Option<ProcessCaptureState>,
}

/// RFC-0062 9.2: per-stream staging for harness-owned process capture.
struct ProcessCaptureState {
    config: ProcessStreamCaptureConfigV1,
    stdout_staging: Option<std::fs::File>,
    stderr_staging: Option<std::fs::File>,
    stdout_staging_path: Option<std::path::PathBuf>,
    stderr_staging_path: Option<std::path::PathBuf>,
    stdout_bytes: u64,
    stderr_bytes: u64,
    stdout_truncated: bool,
    stderr_truncated: bool,
    staged_bytes: u64,
}

impl std::fmt::Debug for ToolArtifactCaptureSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolArtifactCaptureSink")
            .field("tool_call_id", &self.tool_call_id)
            .field("tool_name", &self.tool_name)
            .field("observed_bytes", &self.observed_bytes)
            .finish_non_exhaustive()
    }
}

impl Drop for ToolArtifactCaptureSink {
    fn drop(&mut self) {
        // RFC-0062 16.2: staging files must never survive, including when the sink is dropped
        // without finalize (cancelled execution, supervisor error, mutex poison, partial
        // staging creation). Close the descriptors first so removal also works on platforms
        // that cannot delete an open file, then best-effort remove the tracked paths. On unix
        // the entries were already unlinked at creation, so this is a no-op safety net.
        if let Some(state) = self.process.as_mut() {
            state.stdout_staging.take();
            state.stderr_staging.take();
            for path in [&state.stdout_staging_path, &state.stderr_staging_path]
                .into_iter()
                .flatten()
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

impl PartialEq for ToolArtifactCaptureSink {
    fn eq(&self, other: &Self) -> bool {
        self.tool_call_id == other.tool_call_id
            && self.tool_name == other.tool_name
            && self.media_type == other.media_type
            && self.encoding == other.encoding
            && self.sensitivity == other.sensitivity
            && self.observed_bytes == other.observed_bytes
            && self.process_write_failed == other.process_write_failed
    }
}

/// RFC-0062 16.2: RAII removal of process staging files on every finalize path (success or
/// error); the sink Drop covers sinks that never reach finalize.
struct StagingCleanupGuard {
    paths: Vec<std::path::PathBuf>,
}

impl StagingCleanupGuard {
    fn new(stdout: Option<std::path::PathBuf>, stderr: Option<std::path::PathBuf>) -> Self {
        Self {
            paths: stdout.into_iter().chain(stderr).collect(),
        }
    }
}

impl Drop for StagingCleanupGuard {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl ToolArtifactCaptureSink {
    /// RFC-0062 8.1: enters process capture mode before spawn. Two bounded owner-only
    /// files are created inside the store staging namespace; their paths never enter the child
    /// environment, model, session, UI, or logs.
    pub fn begin_process_capture(&self, config: ProcessStreamCaptureConfigV1) -> Result<Self> {
        let mut sink = self.store.begin_policy_safe_capture(
            &self.tool_call_id,
            &self.tool_name,
            &self.media_type,
            self.encoding,
            self.sensitivity,
        );
        self.store.ensure_root_dir()?;
        let staging_dir = self.store.root.join("staging");
        create_private_dir(&staging_dir)?;
        let stdout_path = staging_dir.join(format!("{}.stdout.part", Uuid::new_v4().simple()));
        let stderr_path = staging_dir.join(format!("{}.stderr.part", Uuid::new_v4().simple()));
        let stdout_staging = open_read_write_private_file(&stdout_path)?;
        // RFC-0062 16.2: unlink-after-open makes staging crash-safe on unix — the directory
        // entry disappears immediately, so process crash / kill -9 / power loss cannot leave
        // policy-unredacted raw bytes on disk; the open descriptor keeps the file alive for
        // the capture lifetime. On platforms that cannot unlink an open file the paths stay
        // tracked and are removed by finalize/Drop/grace GC instead.
        #[cfg(unix)]
        if let Err(error) = std::fs::remove_file(&stdout_path) {
            return Err(error).with_context(|| {
                format!("failed to unlink staged capture {}", stdout_path.display())
            });
        }
        let stderr_staging = match open_read_write_private_file(&stderr_path) {
            Ok(file) => file,
            Err(error) => {
                #[cfg(unix)]
                let _ = std::fs::remove_file(&stderr_path);
                return Err(error);
            }
        };
        #[cfg(unix)]
        if let Err(error) = std::fs::remove_file(&stderr_path) {
            return Err(error).with_context(|| {
                format!("failed to unlink staged capture {}", stderr_path.display())
            });
        }
        // RFC-0062 16.2: on Windows the staging directory inherits the parent DACL unless we
        // apply the shared private-permission contract; delete-on-close removes the entry when
        // the last handle closes, and this explicit DACL keeps the on-disk window owner-only.
        #[cfg(windows)]
        {
            crate::secure_private_path_permissions(&staging_dir)?;
            crate::secure_private_path_permissions(&stdout_path)?;
            crate::secure_private_path_permissions(&stderr_path)?;
        }
        sink.process = Some(ProcessCaptureState {
            config,
            stdout_staging: Some(stdout_staging),
            stderr_staging: Some(stderr_staging),
            stdout_staging_path: Some(stdout_path),
            stderr_staging_path: Some(stderr_path),
            stdout_bytes: 0,
            stderr_bytes: 0,
            stdout_truncated: false,
            stderr_truncated: false,
            staged_bytes: 0,
        });
        Ok(sink)
    }

    /// RFC-0062 10.2: marks the capture as storage-failed so settlement reports Unavailable.
    pub fn mark_process_write_failed(&mut self) {
        self.process_write_failed = true;
    }

    /// RFC-0062 8.2: writes one stream chunk into its bounded staging file. Once a stream's
    /// staging bound is reached, later chunks are counted but not persisted (storage truncation);
    /// the child keeps running because this cap is independent of the observed resource meter.
    pub fn write_stream(&mut self, stream: ToolOutputStreamV1, bytes: &[u8]) -> Result<()> {
        let Some(state) = self.process.as_mut() else {
            return Ok(());
        };
        let limit = state.config.artifact_staging_limit_bytes_per_stream;
        let (file, stream_bytes, truncated) = match stream {
            ToolOutputStreamV1::Stdout => (
                state.stdout_staging.as_mut(),
                &mut state.stdout_bytes,
                &mut state.stdout_truncated,
            ),
            ToolOutputStreamV1::Stderr => (
                state.stderr_staging.as_mut(),
                &mut state.stderr_bytes,
                &mut state.stderr_truncated,
            ),
            ToolOutputStreamV1::Combined => return Ok(()),
        };
        let Some(file) = file else {
            // A missing staging handle means the capture cannot persist this stream; record the
            // failure so settlement marks storage Unavailable instead of claiming completeness.
            self.process_write_failed = true;
            return Ok(());
        };
        let before = *stream_bytes;
        *stream_bytes = stream_bytes.saturating_add(bytes.len() as u64);
        if before >= limit {
            *truncated = true;
            return Ok(());
        }
        let allowed = (limit.saturating_sub(before) as usize).min(bytes.len());
        use std::io::Write as _;
        if let Err(error) = file.write_all(&bytes[..allowed]) {
            self.process_write_failed = true;
            return Err(error).with_context(|| format!("failed to stage {stream:?} capture bytes"));
        }
        state.staged_bytes = state.staged_bytes.saturating_add(allowed as u64);
        if allowed < bytes.len() {
            *truncated = true;
        }
        Ok(())
    }

    /// RFC-0062 9.2/9.3: finalizes the canonical dual-segment artifact (stdout segment then
    /// stderr segment, both contiguous) and returns the descriptor plus immutable capture
    /// evidence. Staging files are removed regardless of publication outcome.
    pub fn finish_process_capture(
        mut self,
        source_observed_bytes: u64,
        redaction_count: u32,
        source: ToolSourceCompletenessV1,
    ) -> Result<(
        ToolArtifactDescriptorV1,
        Vec<ToolOutputSegmentV1>,
        ToolResultCaptureCompletenessV1,
    )> {
        let Some(mut state) = self.process.take() else {
            bail!("finish_process_capture requires process capture mode");
        };
        // Remove staging files on every path out of this function, including success. The sink
        // Drop is the safety net for sinks that never reach finalize.
        let _staging_cleanup = StagingCleanupGuard::new(
            state.stdout_staging_path.take(),
            state.stderr_staging_path.take(),
        );
        let mut stdout_bytes = Vec::new();
        let mut stderr_bytes = Vec::new();
        if let Some(mut file) = state.stdout_staging.take() {
            use std::io::{Read as _, Seek as _, SeekFrom};
            file.seek(SeekFrom::Start(0))?;
            file.read_to_end(&mut stdout_bytes)?;
        }
        if let Some(mut file) = state.stderr_staging.take() {
            use std::io::{Read as _, Seek as _, SeekFrom};
            file.seek(SeekFrom::Start(0))?;
            file.read_to_end(&mut stderr_bytes)?;
        }
        // RFC-0062 10.2/9.2: redact EACH stream first so policy-eligible sizes are exact; then
        // apply the reservation-reclaim settlement on the redacted (eligible) bytes so segments
        // and the canonical body always describe the true persisted layout. A secret split
        // across chunks within one stream is still caught because the stream is redacted as a
        // whole; cross-stream secret splitting is not a supported persistence boundary.
        let redaction_count = if self.encoding == ToolArtifactEncoding::Utf8 {
            let stdout_redacted = safe_persistence_text(&String::from_utf8_lossy(&stdout_bytes));
            let stderr_redacted = safe_persistence_text(&String::from_utf8_lossy(&stderr_bytes));
            let stdout_changed = stdout_redacted.as_bytes() != stdout_bytes.as_slice();
            let stderr_changed = stderr_redacted.as_bytes() != stderr_bytes.as_slice();
            stdout_bytes = stdout_redacted.into_bytes();
            stderr_bytes = stderr_redacted.into_bytes();
            redaction_count.saturating_add(u32::from(stdout_changed || stderr_changed))
        } else {
            redaction_count
        };
        // RFC-0062 9.2: the three-axis ledger is computed on POLICY-SAFE sizes. eligible is the
        // full post-redaction length (redaction may shorten OR lengthen, e.g. token=x ->
        // token=[redacted]); policy_projected is the sum of eligible bytes; persisted is the
        // reservation-reclaim settlement result; truncation is persisted < eligible, or the raw
        // staging cap having dropped observed bytes.
        let stdout_reservation = state.config.artifact_reservation_stdout_bytes;
        let stderr_reservation = state.config.artifact_reservation_stderr_bytes;
        let stdout_eligible = stdout_bytes.len() as u64;
        let stderr_eligible = stderr_bytes.len() as u64;
        let unused_stdout =
            stdout_reservation.saturating_sub(stdout_eligible.min(stdout_reservation));
        let unused_stderr =
            stderr_reservation.saturating_sub(stderr_eligible.min(stderr_reservation));
        let stdout_persisted =
            stdout_eligible.min(stdout_reservation.saturating_add(unused_stderr));
        let stderr_persisted =
            stderr_eligible.min(stderr_reservation.saturating_add(unused_stdout));
        let stdout_truncated = state.stdout_truncated || stdout_persisted < stdout_eligible;
        let stderr_truncated = state.stderr_truncated || stderr_persisted < stderr_eligible;
        stdout_bytes.truncate(stdout_persisted as usize);
        stderr_bytes.truncate(stderr_persisted as usize);
        let stdout_final_len = stdout_bytes.len() as u64;
        let stderr_final_len = stderr_bytes.len() as u64;
        let policy_projected_bytes = stdout_eligible.saturating_add(stderr_eligible);
        let mut bytes = stdout_bytes;
        bytes.extend_from_slice(&stderr_bytes);
        let descriptor = self.store.publish_descriptor(
            &self.tool_call_id,
            &self.tool_name,
            BoundedArtifactBytes {
                bytes,
                retained_head_bytes: stdout_final_len,
                retained_tail_bytes: stderr_final_len,
            },
            source_observed_bytes,
            policy_projected_bytes,
            &self.media_type,
            self.encoding,
            self.sensitivity,
            redaction_count,
        )?;
        let write_failed = self.process_write_failed;
        let stdout_storage = if write_failed {
            ToolStorageCompletenessV1::Unavailable
        } else if stdout_truncated {
            ToolStorageCompletenessV1::TruncatedAtLimit
        } else {
            ToolStorageCompletenessV1::Complete
        };
        let stderr_storage = if write_failed {
            ToolStorageCompletenessV1::Unavailable
        } else if stderr_truncated {
            ToolStorageCompletenessV1::TruncatedAtLimit
        } else {
            ToolStorageCompletenessV1::Complete
        };
        let segments = vec![
            ToolOutputSegmentV1 {
                stream: ToolOutputStreamV1::Stdout,
                artifact_offset: 0,
                persisted_bytes: stdout_final_len,
                eligible_bytes: stdout_eligible,
                observed_bytes: state.stdout_bytes,
                preview_bytes: 0,
                preview_truncated: false,
                storage: stdout_storage,
            },
            ToolOutputSegmentV1 {
                stream: ToolOutputStreamV1::Stderr,
                artifact_offset: stdout_final_len,
                persisted_bytes: stderr_final_len,
                eligible_bytes: stderr_eligible,
                observed_bytes: state.stderr_bytes,
                preview_bytes: 0,
                preview_truncated: false,
                storage: stderr_storage,
            },
        ];
        let storage = if write_failed {
            ToolStorageCompletenessV1::Unavailable
        } else if stdout_truncated || stderr_truncated {
            ToolStorageCompletenessV1::TruncatedAtLimit
        } else {
            ToolStorageCompletenessV1::Complete
        };
        let policy = if redaction_count > 0 {
            ToolPolicyCompletenessV1::Redacted
        } else {
            ToolPolicyCompletenessV1::Preserved
        };
        Ok((
            descriptor,
            segments,
            ToolResultCaptureCompletenessV1 {
                source,
                policy,
                storage,
            },
        ))
    }

    pub fn finish(self) -> Result<ToolArtifactDescriptorV1> {
        let observed_bytes = self.observed_bytes;
        self.finish_with_source_evidence(observed_bytes, 0)
    }

    /// Publishes the policy-safe stream with source-byte and redaction evidence.
    ///
    /// `source_observed_bytes` counts bytes before policy projection. The sink's own observed
    /// count remains the policy-projected byte count used to distinguish policy loss from storage
    /// truncation.
    pub fn finish_with_source_evidence(
        self,
        source_observed_bytes: u64,
        redaction_count: u32,
    ) -> Result<ToolArtifactDescriptorV1> {
        let policy_projected_bytes = self.observed_bytes;
        self.finish_with_projection_evidence(
            source_observed_bytes,
            policy_projected_bytes,
            redaction_count,
        )
    }

    /// Publishes a stream that was already bounded before it reached this sink.
    ///
    /// `policy_projected_bytes` is the complete post-policy size observed by the upstream
    /// collector. Supplying it preserves truthful storage-truncation evidence even when this sink
    /// only receives that collector's retained head/tail bytes.
    pub fn finish_with_projection_evidence(
        mut self,
        source_observed_bytes: u64,
        policy_projected_bytes: u64,
        redaction_count: u32,
    ) -> Result<ToolArtifactDescriptorV1> {
        let retained_head_bytes = self.head.len() as u64;
        let retained_tail_bytes = self.tail.len() as u64;
        if policy_projected_bytes < self.observed_bytes {
            bail!("policy-projected artifact bytes cannot be smaller than captured bytes");
        }
        let mut bytes = std::mem::take(&mut self.head);
        bytes.extend(std::mem::take(&mut self.tail));
        self.store.publish_descriptor(
            &self.tool_call_id,
            &self.tool_name,
            BoundedArtifactBytes {
                bytes,
                retained_head_bytes,
                retained_tail_bytes,
            },
            source_observed_bytes,
            policy_projected_bytes,
            &self.media_type,
            self.encoding,
            self.sensitivity,
            redaction_count,
        )
    }

    #[must_use]
    pub fn observed_bytes(&self) -> u64 {
        self.observed_bytes
    }
}

impl Write for ToolArtifactCaptureSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.observed_bytes = self.observed_bytes.saturating_add(bytes.len() as u64);
        let half = TOOL_ARTIFACT_MAX_BYTES / 2;
        let head_remaining = half.saturating_sub(self.head.len());
        let head_len = head_remaining.min(bytes.len());
        self.head.extend_from_slice(&bytes[..head_len]);
        let remaining = &bytes[head_len..];
        if remaining.len() >= half {
            self.tail.clear();
            self.tail.extend(
                remaining[remaining.len().saturating_sub(half)..]
                    .iter()
                    .copied(),
            );
        } else if !remaining.is_empty() {
            let excess = self
                .tail
                .len()
                .saturating_add(remaining.len())
                .saturating_sub(half);
            if excess > 0 {
                self.tail.drain(..excess);
            }
            self.tail.extend(remaining.iter().copied());
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct BoundedArtifactBytes {
    bytes: Vec<u8>,
    retained_head_bytes: u64,
    retained_tail_bytes: u64,
}

impl BoundedArtifactBytes {
    fn truncation(&self, observed_bytes: u64) -> Option<ToolArtifactTruncationV1> {
        let persisted_bytes = self.bytes.len() as u64;
        (observed_bytes > persisted_bytes).then(|| ToolArtifactTruncationV1 {
            omitted_bytes: observed_bytes - persisted_bytes,
            retained_head_bytes: self.retained_head_bytes,
            retained_tail_bytes: self.retained_tail_bytes,
        })
    }
}

fn bounded_artifact_bytes(bytes: &[u8]) -> BoundedArtifactBytes {
    if bytes.len() <= TOOL_ARTIFACT_MAX_BYTES {
        return BoundedArtifactBytes {
            bytes: bytes.to_vec(),
            retained_head_bytes: bytes.len() as u64,
            retained_tail_bytes: 0,
        };
    }
    let head_bytes = TOOL_ARTIFACT_MAX_BYTES / 2;
    let tail_bytes = TOOL_ARTIFACT_MAX_BYTES - head_bytes;
    let mut bounded = Vec::with_capacity(TOOL_ARTIFACT_MAX_BYTES);
    bounded.extend_from_slice(&bytes[..head_bytes]);
    bounded.extend_from_slice(&bytes[bytes.len() - tail_bytes..]);
    BoundedArtifactBytes {
        bytes: bounded,
        retained_head_bytes: head_bytes as u64,
        retained_tail_bytes: tail_bytes as u64,
    }
}

fn hash_file(path: &Path, max_bytes: u64) -> Result<String> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect tool artifact {}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > max_bytes {
        bail!("tool artifact blob is not a valid bounded plain file");
    }
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .with_context(|| format!("failed to hash {}", path.display()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn directory_file_bytes(root: &Path) -> Result<u64> {
    let mut pending = vec![root.to_path_buf()];
    let mut total = 0_u64;
    let mut entries_seen = 0_usize;
    while let Some(path) = pending.pop() {
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("failed to scan {}", path.display()));
            }
        };
        for entry in entries {
            let entry = entry.with_context(|| format!("failed to scan {}", path.display()))?;
            entries_seen = entries_seen.saturating_add(1);
            if entries_seen > TOOL_ARTIFACT_MANIFEST_MAX_ENTRIES.saturating_mul(2) {
                bail!("tool artifact blob inventory exceeds its entry limit");
            }
            let metadata = fs::symlink_metadata(entry.path())
                .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
            if metadata.file_type().is_symlink() {
                bail!("tool artifact blob inventory must not contain symlinks");
            } else if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            } else {
                bail!("tool artifact blob inventory contains a non-file entry");
            }
        }
    }
    Ok(total)
}

fn safe_directory_file_bytes(root: &Path) -> Result<u64> {
    let mut pending = vec![root.to_path_buf()];
    let mut total = 0u64;
    let mut entries_seen = 0usize;
    while let Some(path) = pending.pop() {
        let entries =
            fs::read_dir(&path).with_context(|| format!("failed to scan {}", path.display()))?;
        for entry in entries {
            let entry = entry.with_context(|| format!("failed to scan {}", path.display()))?;
            entries_seen = entries_seen.saturating_add(1);
            if entries_seen > TOOL_ARTIFACT_MANIFEST_MAX_ENTRIES.saturating_mul(4) {
                bail!("tool artifact trash exceeds its entry limit");
            }
            let entry_path = entry.path();
            let metadata = fs::symlink_metadata(&entry_path)
                .with_context(|| format!("failed to inspect {}", entry_path.display()))?;
            if metadata.file_type().is_symlink() {
                bail!("tool artifact trash must not contain symlinks");
            }
            if metadata.is_dir() {
                pending.push(entry_path);
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            } else {
                bail!("tool artifact trash contains a non-file entry");
            }
        }
    }
    Ok(total)
}

struct SelectedArtifactBytes {
    bytes: Vec<u8>,
    eof: bool,
    match_count: u16,
    next_selector: Option<ToolArtifactSelectorV1>,
}

fn read_byte_slice(path: &Path, offset: u64, limit: u32) -> Result<SelectedArtifactBytes> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let artifact_bytes = file
        .metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))?
        .len();
    let start = offset.min(artifact_bytes);
    file.seek(SeekFrom::Start(start))
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let mut bytes = Vec::with_capacity(limit as usize);
    file.take(limit as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let next_offset = start.saturating_add(bytes.len() as u64);
    let eof = next_offset >= artifact_bytes;
    Ok(SelectedArtifactBytes {
        bytes,
        eof,
        match_count: 0,
        next_selector: (!eof).then_some(ToolArtifactSelectorV1::ByteSlice {
            offset: next_offset,
            limit,
        }),
    })
}

fn read_line_page(path: &Path, start_line: u64, line_count: u32) -> Result<SelectedArtifactBytes> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut line_index = 0_u64;
    let mut byte_offset = 0_u64;
    let mut returned_lines = 0_u32;
    let mut bytes = Vec::new();
    loop {
        line.clear();
        let line_start = byte_offset;
        let count = reader
            .read_until(b'\n', &mut line)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if count == 0 {
            break;
        }
        byte_offset = byte_offset.saturating_add(count as u64);
        if line_index < start_line {
            line_index = line_index.saturating_add(1);
            continue;
        }
        if returned_lines == line_count {
            return Ok(SelectedArtifactBytes {
                bytes,
                eof: false,
                match_count: 0,
                next_selector: Some(ToolArtifactSelectorV1::LinePage {
                    start_line: line_index,
                    line_count,
                }),
            });
        }
        let remaining = TOOL_ARTIFACT_READ_MAX_BYTES as usize - bytes.len();
        if line.len() > remaining {
            if bytes.is_empty() {
                bytes.extend_from_slice(&line[..remaining]);
                return Ok(SelectedArtifactBytes {
                    bytes,
                    eof: false,
                    match_count: 0,
                    next_selector: Some(ToolArtifactSelectorV1::ByteSlice {
                        offset: line_start.saturating_add(remaining as u64),
                        limit: TOOL_ARTIFACT_READ_MAX_BYTES,
                    }),
                });
            }
            return Ok(SelectedArtifactBytes {
                bytes,
                eof: false,
                match_count: 0,
                next_selector: Some(ToolArtifactSelectorV1::LinePage {
                    start_line: line_index,
                    line_count,
                }),
            });
        }
        bytes.extend_from_slice(&line);
        returned_lines = returned_lines.saturating_add(1);
        line_index = line_index.saturating_add(1);
    }
    Ok(SelectedArtifactBytes {
        bytes,
        eof: true,
        match_count: 0,
        next_selector: None,
    })
}

fn read_literal_search(
    path: &Path,
    query: &str,
    start_offset: u64,
    max_matches: u16,
    context_lines: u16,
) -> Result<SelectedArtifactBytes> {
    let query = query.as_bytes();
    let matcher = AhoCorasick::new([query]).context("failed to build bounded literal matcher")?;
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let artifact_bytes = file
        .metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))?
        .len();
    let start = start_offset.min(artifact_bytes);
    file.seek(SeekFrom::Start(start))
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut cursor = start;
    if start > 0 {
        let mut partial = Vec::new();
        let skipped = reader
            .read_until(b'\n', &mut partial)
            .with_context(|| format!("failed to align search in {}", path.display()))?;
        if skipped == 0 {
            return Ok(SelectedArtifactBytes {
                bytes: Vec::new(),
                eof: true,
                match_count: 0,
                next_selector: None,
            });
        }
        cursor = cursor.saturating_add(skipped as u64);
    }
    let mut prior = VecDeque::<Vec<u8>>::with_capacity(context_lines as usize);
    let mut line = Vec::new();
    let mut body = Vec::new();
    let mut match_count = 0_u16;
    let mut trailing_context = 0_u16;
    let mut reached_match_limit = false;
    let mut eof = false;
    loop {
        if reached_match_limit && trailing_context == 0 {
            break;
        }
        line.clear();
        let line_start = cursor;
        let count = reader
            .read_until(b'\n', &mut line)
            .with_context(|| format!("failed to search {}", path.display()))?;
        if count == 0 {
            eof = true;
            break;
        }
        cursor = cursor.saturating_add(count as u64);
        let occurrences = if reached_match_limit {
            0
        } else {
            matcher
                .find_overlapping_iter(&line)
                .take((max_matches - match_count) as usize)
                .count() as u16
        };
        if occurrences > 0 {
            let required = prior.iter().map(Vec::len).sum::<usize>() + line.len();
            if body.len().saturating_add(required) > TOOL_ARTIFACT_READ_MAX_BYTES as usize {
                return Ok(search_selection_with_next(
                    body,
                    match_count,
                    false,
                    query,
                    line_start,
                    max_matches,
                    context_lines,
                ));
            }
            for prior_line in prior.drain(..) {
                body.extend_from_slice(&prior_line);
            }
            body.extend_from_slice(&line);
            match_count = match_count.saturating_add(occurrences);
            trailing_context = context_lines;
            reached_match_limit = match_count == max_matches;
        } else if trailing_context > 0 {
            if body.len().saturating_add(line.len()) > TOOL_ARTIFACT_READ_MAX_BYTES as usize {
                return Ok(search_selection_with_next(
                    body,
                    match_count,
                    false,
                    query,
                    line_start,
                    max_matches,
                    context_lines,
                ));
            }
            body.extend_from_slice(&line);
            trailing_context -= 1;
        } else if context_lines > 0 {
            if prior.len() == context_lines as usize {
                prior.pop_front();
            }
            prior.push_back(line.clone());
        }
    }
    let eof = eof || cursor >= artifact_bytes;
    Ok(search_selection_with_next(
        body,
        match_count,
        eof,
        query,
        cursor,
        max_matches,
        context_lines,
    ))
}

fn search_selection_with_next(
    bytes: Vec<u8>,
    match_count: u16,
    eof: bool,
    query: &[u8],
    next_offset: u64,
    max_matches: u16,
    context_lines: u16,
) -> SelectedArtifactBytes {
    SelectedArtifactBytes {
        bytes,
        eof,
        match_count,
        next_selector: (!eof).then(|| ToolArtifactSelectorV1::SearchLiteral {
            query: String::from_utf8_lossy(query).into_owned(),
            start_offset: next_offset,
            max_matches,
            context_lines,
        }),
    }
}

fn selector_reserved_bytes(selector: &ToolArtifactSelectorV1) -> u64 {
    match selector {
        ToolArtifactSelectorV1::ByteSlice { limit, .. } => *limit as u64,
        ToolArtifactSelectorV1::LinePage { .. } | ToolArtifactSelectorV1::SearchLiteral { .. } => {
            TOOL_ARTIFACT_READ_MAX_BYTES as u64
        }
    }
}

fn publish_private_noclobber(dir: &Path, destination: &Path, bytes: &[u8]) -> Result<()> {
    let staging = dir.join(format!(".{}.part", Uuid::new_v4().simple()));
    let mut file = create_private_file(&staging)?;
    file.write_all(bytes)
        .with_context(|| format!("failed to write {}", staging.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", staging.display()))?;
    drop(file);
    match fs::hard_link(&staging, destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = fs::read(destination)
                .with_context(|| format!("failed to read {}", destination.display()))?;
            if existing != bytes {
                let _ = fs::remove_file(&staging);
                bail!("immutable tool artifact manifest collision");
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&staging);
            return Err(error)
                .with_context(|| format!("failed to publish {}", destination.display()));
        }
    }
    let _ = fs::remove_file(&staging);
    harden_private_file(destination)
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

fn metadata_modified_at_unix_ms(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().try_into().unwrap_or(u64::MAX)
        })
}

pub(super) fn bounded_model_preview(value: &str, max_bytes: usize) -> (String, ToolPreviewKind) {
    if value.len() <= max_bytes {
        return (value.to_owned(), ToolPreviewKind::Complete);
    }
    let marker = "\n[sigil: bounded tool output; use read_tool_artifact for omitted content]\n";
    if max_bytes <= marker.len() {
        return (bounded_utf8(value, max_bytes), ToolPreviewKind::HeadTail);
    }
    let available = max_bytes.saturating_sub(marker.len());
    let head_limit = available.saturating_mul(2) / 3;
    let tail_limit = available.saturating_sub(head_limit);
    let head_end = previous_char_boundary(value, head_limit);
    let tail_start =
        next_char_boundary(value, value.len().saturating_sub(tail_limit)).max(head_end);
    let mut preview = String::with_capacity(max_bytes);
    preview.push_str(&value[..head_end]);
    preview.push_str(marker);
    preview.push_str(&value[tail_start..]);
    (preview, ToolPreviewKind::HeadTail)
}

fn bounded_utf8(value: &str, max_bytes: usize) -> String {
    let end = previous_char_boundary(value, max_bytes);
    value[..end].to_owned()
}

fn previous_char_boundary(value: &str, max_index: usize) -> usize {
    let mut index = max_index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn next_char_boundary(value: &str, min_index: usize) -> usize {
    let mut index = min_index.min(value.len());
    while index < value.len() && !value.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn create_private_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => bail!(
            "tool artifact path is not a plain directory: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).with_context(|| {
                format!("failed to create tool artifact dir {}", path.display())
            })?;
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to inspect tool artifact dir {}", path.display())
            });
        }
    }
    harden_private_dir(path)
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("failed to create private tool artifact {}", path.display()))
}

/// Owner-only read+write staging file used by harness-owned process capture.
#[cfg(unix)]
fn open_read_write_private_file(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("failed to create private tool artifact {}", path.display()))
}

#[cfg(not(unix))]
fn open_read_write_private_file(path: &Path) -> Result<File> {
    #[cfg(windows)]
    {
        // RFC-0062 16.2: Windows delete-on-close via OpenOptionsExt. The OS removes the
        // directory entry when the last handle closes, so crash / TerminateProcess cannot leave
        // policy-unredacted raw bytes in a .part file; FILE_SHARE_DELETE is required for
        // FILE_FLAG_DELETE_ON_CLOSE to behave correctly. The OS does not guarantee deletion
        // after sudden power loss, so grace GC remains the cross-platform fallback.
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_NORMAL, FILE_FLAG_DELETE_ON_CLOSE, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE,
        };
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_ATTRIBUTE_NORMAL | FILE_FLAG_DELETE_ON_CLOSE)
            .open(path)
            .with_context(|| format!("failed to create private tool artifact {}", path.display()))
    }
    #[cfg(not(windows))]
    {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| {
                format!("failed to create private tool artifact {}", path.display())
            })?;
        Ok(file)
    }
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("failed to create private tool artifact {}", path.display()))?;
    harden_private_file(path)?;
    Ok(file)
}

#[cfg(unix)]
fn harden_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to set permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn harden_private_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn harden_private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to set permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn harden_private_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("failed to open tool artifact dir {}", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync tool artifact dir {}", path.display()))
}

#[cfg(not(unix))]
fn sync_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
#[path = "tests/tool_artifact_tests.rs"]
mod tests;
