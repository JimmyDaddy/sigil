use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ApprovalMode, NetworkEffect, ToolAccess, ToolOperation, ToolSubject,
    event::canonical_json_content_hash,
};

pub const TOOL_PERMISSION_PLAN_SCHEMA_VERSION: u32 = 2;
pub const MAX_TOOL_PERMISSION_SUBJECTS: usize = 64;
pub const MAX_TOOL_PERMISSION_SUBJECT_BYTES: usize = 64 * 1024;
pub const MAX_TOOL_PERMISSION_SUBJECT_TOTAL_BYTES: usize = 128 * 1024;
pub const MAX_TOOL_ANALYSIS_REASONS: usize = 16;
pub const MAX_TOOL_ANALYSIS_REASON_DETAIL_BYTES: usize = 256;
pub const MAX_TOOL_SEMANTIC_QUALIFIERS: usize = 16;
pub const MAX_TOOL_SEMANTIC_FAMILY_BYTES: usize = 96;
pub const MAX_TOOL_SEMANTIC_QUALIFIER_KEY_BYTES: usize = 64;
pub const MAX_TOOL_SEMANTIC_QUALIFIER_VALUE_BYTES: usize = 256;
pub const MAX_TOOL_ANALYSIS_BINDINGS: usize = 24;
pub const MAX_TOOL_ANALYSIS_BINDING_KEY_BYTES: usize = 64;
pub const MAX_TOOL_ANALYSIS_BINDING_VALUE_BYTES: usize = 512;
pub const MAX_TOOL_PERMISSION_SUMMARY_TITLE_BYTES: usize = 160;
pub const MAX_TOOL_PERMISSION_SUMMARY_DETAIL_BYTES: usize = 768;

/// Generic, deterministic effects derived before permission policy is evaluated.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermissionEffect {
    FileRead,
    FileWrite,
    FileDelete,
    ExecuteTrustedBinary,
    ExecuteWorkspaceCode,
    ExecuteDynamicCode,
    NetworkRead,
    NetworkMutate,
    NetworkUnknown,
    /// Host-owned agent thread lifecycle without direct operating-system process authority.
    AgentLifecycle,
    ProcessControl,
    PrivilegeEscalation,
    PersistenceChange,
    RemoteMutation,
    CredentialAccess,
    ExternalApplicationControl,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ToolAnalysisReasonCode {
    UnknownProgram,
    DynamicCommand,
    UnsupportedSyntax,
    InvalidSyntax,
    AnalysisLimitExceeded,
    UnresolvedPath,
    UnresolvedExecutable,
    UnprovenContainment,
}

/// A bounded, non-secret explanation for incomplete deterministic analysis.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub struct ToolAnalysisReason {
    pub code: ToolAnalysisReasonCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ToolAnalysisReason {
    #[must_use]
    pub fn new(code: ToolAnalysisReasonCode, detail: impl Into<Option<String>>) -> Self {
        Self {
            code,
            detail: detail
                .into()
                .map(|detail| bounded_safe_text(&detail, MAX_TOOL_ANALYSIS_REASON_DETAIL_BYTES)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum ToolAnalysisStatus {
    Complete,
    Conservative { reasons: Vec<ToolAnalysisReason> },
    Unsupported { reason: ToolAnalysisReason },
    Invalid { reason: ToolAnalysisReason },
}

impl ToolAnalysisStatus {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemContainment {
    #[default]
    Unspecified,
    WorkspaceReadOnly,
    WorkspaceWrite,
    WorkspaceAndScratch,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkContainment {
    #[default]
    Unspecified,
    Deny,
    ReadOnly,
    Allow,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessContainment {
    #[default]
    Unspecified,
    OwnedTree,
    Isolated,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentContainment {
    #[default]
    Unspecified,
    Restricted,
    UserInherited,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ExecutionContainmentRequest {
    pub filesystem: FilesystemContainment,
    pub network: NetworkContainment,
    pub process: ProcessContainment,
    pub environment: EnvironmentContainment,
    pub persistent_process: bool,
}

/// Exact, policy-safe execution boundary used when revalidating durable session authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ExecutionContainmentBindingV2 {
    pub requested: ExecutionContainmentRequest,
    pub backend_identity_hash: String,
    pub backend_profile_hash: String,
    pub environment_binding_hash: String,
}

/// Stable semantic identity used by policy and bounded session grants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub struct ToolSemanticScope {
    pub family: String,
    pub version: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub qualifiers: BTreeMap<String, String>,
}

impl ToolSemanticScope {
    #[must_use]
    pub fn new(family: impl Into<String>, version: u32) -> Self {
        Self {
            family: family.into(),
            version,
            qualifiers: BTreeMap::new(),
        }
    }
}

/// Bounded text intended for approval surfaces; never contains raw output or credentials.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolPermissionSummary {
    pub title: String,
    pub detail: String,
    #[serde(default)]
    pub step_count: u32,
    #[serde(default)]
    pub workspace_code_steps: u32,
}

/// Tool-produced deterministic facts before the registry binds call and workspace identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolPermissionPlanDraft {
    pub access: ToolAccess,
    pub operation: ToolOperation,
    pub effects: BTreeSet<ToolPermissionEffect>,
    pub subjects: Vec<ToolSubject>,
    pub analysis: ToolAnalysisStatus,
    pub containment: ExecutionContainmentRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_scope: Option<ToolSemanticScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_default_mode: Option<ApprovalMode>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub analysis_bindings: BTreeMap<String, String>,
    pub safe_summary: ToolPermissionSummary,
}

/// Immutable permission input shared by policy, approval, execution and audit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolPermissionPlanV2 {
    pub schema_version: u32,
    pub tool_name: String,
    pub access: ToolAccess,
    pub operation: ToolOperation,
    pub effects: BTreeSet<ToolPermissionEffect>,
    pub subjects: Vec<ToolSubject>,
    pub analysis: ToolAnalysisStatus,
    pub containment: ExecutionContainmentRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_scope: Option<ToolSemanticScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_default_mode: Option<ApprovalMode>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub analysis_bindings: BTreeMap<String, String>,
    pub plan_hash: String,
    pub safe_summary: ToolPermissionSummary,
}

impl ToolPermissionPlanV2 {
    pub(crate) fn bind(
        tool_name: &str,
        canonical_args: &Value,
        workspace_root: &Path,
        mut draft: ToolPermissionPlanDraft,
    ) -> Result<Self> {
        draft.safe_summary.title = bounded_safe_text(
            &draft.safe_summary.title,
            MAX_TOOL_PERMISSION_SUMMARY_TITLE_BYTES,
        );
        draft.safe_summary.detail = bounded_safe_text(
            &draft.safe_summary.detail,
            MAX_TOOL_PERMISSION_SUMMARY_DETAIL_BYTES,
        );
        validate_draft(tool_name, &draft)?;
        let workspace_binding = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf())
            .to_string_lossy()
            .replace('\\', "/");
        let hash_payload = serde_json::json!({
            "schema_version": TOOL_PERMISSION_PLAN_SCHEMA_VERSION,
            "tool_name": tool_name,
            "canonical_args": canonical_args,
            "workspace_binding": workspace_binding,
            "draft": draft,
        });
        let plan_hash = canonical_json_content_hash(&hash_payload)?;
        Ok(Self {
            schema_version: TOOL_PERMISSION_PLAN_SCHEMA_VERSION,
            tool_name: tool_name.to_owned(),
            access: draft.access,
            operation: draft.operation,
            effects: draft.effects,
            subjects: draft.subjects,
            analysis: draft.analysis,
            containment: draft.containment,
            semantic_scope: draft.semantic_scope,
            tool_default_mode: draft.tool_default_mode,
            analysis_bindings: draft.analysis_bindings,
            plan_hash,
            safe_summary: draft.safe_summary,
        })
    }

    #[must_use]
    pub fn network_effect(&self) -> Option<NetworkEffect> {
        if self.effects.contains(&ToolPermissionEffect::NetworkUnknown) {
            Some(NetworkEffect::Unknown)
        } else if self.effects.contains(&ToolPermissionEffect::NetworkMutate) {
            Some(NetworkEffect::Mutate)
        } else if self.effects.contains(&ToolPermissionEffect::NetworkRead) {
            Some(NetworkEffect::Read)
        } else {
            None
        }
    }

    /// Returns the exact execution binding required by a reusable session grant.
    ///
    /// The binding records the actual backend/profile/environment state even when containment is
    /// insufficient for automatic approval. Exact interactive session authority can then be
    /// revalidated against that unchanged state without pretending the containment was proven.
    #[must_use]
    pub fn session_grant_containment_binding(&self) -> Option<ExecutionContainmentBindingV2> {
        let backend_identity = self.analysis_bindings.get("execution_backend")?.trim();
        let backend_profile = self.analysis_bindings.get("execution_profile")?.trim();
        let environment_binding = self.analysis_bindings.get("environment_binding")?.trim();
        if backend_identity.is_empty()
            || backend_profile.is_empty()
            || environment_binding.is_empty()
        {
            return None;
        }
        Some(ExecutionContainmentBindingV2 {
            requested: self.containment.clone(),
            backend_identity_hash: crate::stable_event_hash(backend_identity.as_bytes()),
            backend_profile_hash: crate::stable_event_hash(backend_profile.as_bytes()),
            environment_binding_hash: crate::stable_event_hash(environment_binding.as_bytes()),
        })
    }

    /// Returns whether this plan contains effects that must never be widened to session scope.
    #[must_use]
    pub fn has_non_grantable_effect(&self) -> bool {
        !tool_permission_effects_session_grantable(&self.effects)
    }
}

/// Returns whether every declared effect is eligible for bounded session authority.
///
/// This is intentionally plan-shape only. Callers must additionally require complete analysis,
/// a semantic scope, an exact execution binding, and a grantable policy decision.
#[must_use]
pub fn tool_permission_effects_session_grantable(effects: &BTreeSet<ToolPermissionEffect>) -> bool {
    !effects.iter().any(|effect| {
        matches!(
            effect,
            ToolPermissionEffect::FileDelete
                | ToolPermissionEffect::ExecuteDynamicCode
                | ToolPermissionEffect::NetworkMutate
                | ToolPermissionEffect::NetworkUnknown
                | ToolPermissionEffect::PrivilegeEscalation
                | ToolPermissionEffect::PersistenceChange
                | ToolPermissionEffect::RemoteMutation
                | ToolPermissionEffect::CredentialAccess
                | ToolPermissionEffect::ExternalApplicationControl
                | ToolPermissionEffect::Unknown
        )
    })
}

fn validate_draft(tool_name: &str, draft: &ToolPermissionPlanDraft) -> Result<()> {
    if tool_name.trim().is_empty() {
        bail!("permission plan tool name must not be empty");
    }
    if draft.effects.is_empty() {
        bail!("permission plan must declare at least one effect");
    }
    if draft.safe_summary.title.trim().is_empty() {
        bail!("permission plan safe summary title must not be empty");
    }
    if draft.subjects.len() > MAX_TOOL_PERMISSION_SUBJECTS {
        bail!(
            "permission plan subjects exceed maximum count of {}",
            MAX_TOOL_PERMISSION_SUBJECTS
        );
    }
    let mut total_subject_bytes = 0usize;
    for subject in &draft.subjects {
        for value in [&subject.original, &subject.normalized] {
            if value.len() > MAX_TOOL_PERMISSION_SUBJECT_BYTES {
                bail!(
                    "permission plan subject exceeds maximum of {} bytes",
                    MAX_TOOL_PERMISSION_SUBJECT_BYTES
                );
            }
            total_subject_bytes = total_subject_bytes.saturating_add(value.len());
        }
        if let Some(path) = &subject.canonical_path {
            total_subject_bytes =
                total_subject_bytes.saturating_add(path.as_os_str().to_string_lossy().len());
        }
    }
    if total_subject_bytes > MAX_TOOL_PERMISSION_SUBJECT_TOTAL_BYTES {
        bail!(
            "permission plan subjects exceed aggregate maximum of {} bytes",
            MAX_TOOL_PERMISSION_SUBJECT_TOTAL_BYTES
        );
    }
    validate_analysis_status(&draft.analysis)?;
    if let Some(scope) = &draft.semantic_scope {
        validate_semantic_scope(scope)?;
    }
    if draft.analysis_bindings.len() > MAX_TOOL_ANALYSIS_BINDINGS {
        bail!(
            "permission plan analysis bindings exceed maximum count of {}",
            MAX_TOOL_ANALYSIS_BINDINGS
        );
    }
    for (key, value) in &draft.analysis_bindings {
        validate_policy_safe_label(
            "permission plan analysis binding key",
            key,
            MAX_TOOL_ANALYSIS_BINDING_KEY_BYTES,
            false,
        )?;
        if value.len() > MAX_TOOL_ANALYSIS_BINDING_VALUE_BYTES {
            bail!(
                "permission plan analysis binding value exceeds maximum of {} bytes",
                MAX_TOOL_ANALYSIS_BINDING_VALUE_BYTES
            );
        }
    }
    Ok(())
}

pub(crate) fn validate_analysis_status(status: &ToolAnalysisStatus) -> Result<()> {
    let reasons = match status {
        ToolAnalysisStatus::Complete => return Ok(()),
        ToolAnalysisStatus::Conservative { reasons } => reasons.as_slice(),
        ToolAnalysisStatus::Unsupported { reason } | ToolAnalysisStatus::Invalid { reason } => {
            std::slice::from_ref(reason)
        }
    };
    if reasons.len() > MAX_TOOL_ANALYSIS_REASONS {
        bail!(
            "permission plan analysis reasons exceed maximum count of {}",
            MAX_TOOL_ANALYSIS_REASONS
        );
    }
    for reason in reasons {
        if let Some(detail) = &reason.detail {
            if detail.len() > MAX_TOOL_ANALYSIS_REASON_DETAIL_BYTES {
                bail!(
                    "permission plan analysis reason exceeds maximum of {} bytes",
                    MAX_TOOL_ANALYSIS_REASON_DETAIL_BYTES
                );
            }
            if crate::safe_persistence_text(detail) != *detail {
                bail!("permission plan analysis reason is not safe for persistence");
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_semantic_scope(scope: &ToolSemanticScope) -> Result<()> {
    if scope.version == 0 {
        bail!("permission plan semantic scope version must be non-zero");
    }
    validate_policy_safe_label(
        "permission plan semantic family",
        &scope.family,
        MAX_TOOL_SEMANTIC_FAMILY_BYTES,
        false,
    )?;
    if scope.qualifiers.len() > MAX_TOOL_SEMANTIC_QUALIFIERS {
        bail!(
            "permission plan semantic qualifiers exceed maximum count of {}",
            MAX_TOOL_SEMANTIC_QUALIFIERS
        );
    }
    for (key, value) in &scope.qualifiers {
        validate_policy_safe_label(
            "permission plan semantic qualifier key",
            key,
            MAX_TOOL_SEMANTIC_QUALIFIER_KEY_BYTES,
            false,
        )?;
        validate_policy_safe_label(
            "permission plan semantic qualifier value",
            value,
            MAX_TOOL_SEMANTIC_QUALIFIER_VALUE_BYTES,
            true,
        )?;
    }
    Ok(())
}

fn validate_policy_safe_label(
    label: &str,
    value: &str,
    max_bytes: usize,
    allow_relative_path_separator: bool,
) -> Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > max_bytes {
        bail!("{label} must contain 1..={max_bytes} bytes");
    }
    if crate::safe_persistence_text(trimmed) != trimmed
        || trimmed.starts_with('/')
        || trimmed.contains('\\')
        || trimmed.split('/').any(|segment| segment == "..")
        || !trimmed.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '_' | '-' | '.' | ':' | '=' | '@')
                || (allow_relative_path_separator && character == '/')
        })
    {
        bail!("{label} is not a policy-safe stable label");
    }
    Ok(())
}

fn bounded_safe_text(value: &str, max_bytes: usize) -> String {
    let safe = redact_url_tokens(&crate::safe_persistence_text(value));
    if safe.len() <= max_bytes {
        return safe;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !safe.is_char_boundary(boundary) {
        boundary -= 1;
    }
    safe[..boundary].to_owned()
}

fn redact_url_tokens(value: &str) -> String {
    value
        .split_whitespace()
        .map(|token| {
            let lower = token.to_ascii_lowercase();
            if lower.contains("http://") || lower.contains("https://") {
                "[redacted-url]"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[path = "tests/permission_plan_tests.rs"]
mod tests;
