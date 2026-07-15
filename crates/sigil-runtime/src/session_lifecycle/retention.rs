use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use super::{
    LocalSessionCatalogEntry, LocalSessionCatalogState, LocalSessionLifecycleEvent,
    LocalSessionLifecycleService, LocalSessionRetentionJournalBinding, SessionDeleteOutput,
    SessionDeletePreview, digest_serializable, session_writer_is_inactive,
};

/// Runtime retention policy converted from the user-visible session configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionRetentionPolicy {
    pub max_sessions: Option<usize>,
    pub max_bytes: Option<u64>,
    pub expire_older_than_ms: Option<u64>,
}

impl From<&sigil_kernel::SessionRetentionConfig> for SessionRetentionPolicy {
    fn from(config: &sigil_kernel::SessionRetentionConfig) -> Self {
        Self {
            max_sessions: config.max_sessions,
            max_bytes: config.max_bytes,
            expire_older_than_ms: config.expire_older_than_ms,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SessionRetentionReason {
    Age,
    Count,
    Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionRetentionCandidate {
    pub delete_preview: SessionDeletePreview,
    pub reasons: Vec<SessionRetentionReason>,
}

/// Read-only deterministic retention selection and exact batch binding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionRetentionPreview {
    pub policy: SessionRetentionPolicy,
    pub generated_at_unix_ms: u64,
    pub total_ready_sessions: usize,
    pub total_ready_bytes: u64,
    pub protected_sessions: usize,
    pub pinned_sessions: usize,
    pub ineligible_sessions: usize,
    pub selected_bytes: u64,
    pub constraints_satisfied: bool,
    pub candidates: Vec<SessionRetentionCandidate>,
    pub preview_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionRetentionOutput {
    pub operation_id: String,
    pub deleted_sessions: usize,
    pub deleted_bytes: u64,
    pub delete_outputs: Vec<SessionDeleteOutput>,
    pub journal_sequence: u64,
}

impl LocalSessionLifecycleService {
    /// Selects inactive, unpinned sessions without mutating files or the lifecycle journal.
    ///
    /// # Errors
    ///
    /// Returns an error when catalog/journal validation fails or any selected source drifts while
    /// its exact delete preview is built.
    pub fn preview_retention(
        &self,
        policy: SessionRetentionPolicy,
        protected_paths: &[PathBuf],
        generated_at_unix_ms: u64,
    ) -> Result<SessionRetentionPreview> {
        let catalog = self.catalog()?;
        let ready = catalog
            .entries
            .iter()
            .filter(|entry| entry.state == LocalSessionCatalogState::Ready)
            .collect::<Vec<_>>();
        let total_ready_sessions = ready.len();
        let total_ready_bytes = ready
            .iter()
            .fold(0_u64, |total, entry| total.saturating_add(entry.bytes));
        let ineligible_sessions = catalog
            .entries
            .len()
            .saturating_sub(total_ready_sessions)
            .saturating_add(catalog.truncated_entry_count);
        let mut protected_sessions = 0usize;
        let mut pinned_sessions = 0usize;
        let mut eligible = Vec::new();
        for entry in &ready {
            if entry.pinned {
                pinned_sessions = pinned_sessions.saturating_add(1);
                continue;
            }
            if is_protected(&entry.path, protected_paths) {
                protected_sessions = protected_sessions.saturating_add(1);
                continue;
            }
            if !session_writer_is_inactive(&entry.path)? {
                protected_sessions = protected_sessions.saturating_add(1);
                continue;
            }
            eligible.push(*entry);
        }
        eligible.sort_by(|left, right| {
            left.modified_at_unix_ms
                .cmp(&right.modified_at_unix_ms)
                .then_with(|| left.path.cmp(&right.path))
        });

        let mut selected = BTreeMap::<sigil_kernel::SessionRef, Vec<SessionRetentionReason>>::new();
        if let Some(expire_older_than_ms) = policy.expire_older_than_ms {
            for entry in &eligible {
                if generated_at_unix_ms.saturating_sub(entry.modified_at_unix_ms)
                    > expire_older_than_ms
                {
                    selected
                        .entry(entry.session_ref.clone())
                        .or_default()
                        .push(SessionRetentionReason::Age);
                }
            }
        }
        if let Some(max_sessions) = policy.max_sessions {
            for entry in &eligible {
                let remaining = total_ready_sessions.saturating_sub(selected.len());
                if remaining <= max_sessions {
                    break;
                }
                let reasons = selected.entry(entry.session_ref.clone()).or_default();
                if !reasons.contains(&SessionRetentionReason::Count) {
                    reasons.push(SessionRetentionReason::Count);
                }
            }
        }
        if let Some(max_bytes) = policy.max_bytes {
            let mut remaining_bytes =
                total_ready_bytes.saturating_sub(selected_bytes(&eligible, &selected));
            for entry in &eligible {
                if remaining_bytes <= max_bytes {
                    break;
                }
                let reasons = selected.entry(entry.session_ref.clone()).or_default();
                if !reasons.contains(&SessionRetentionReason::Bytes) {
                    if reasons.is_empty() {
                        remaining_bytes = remaining_bytes.saturating_sub(entry.bytes);
                    }
                    reasons.push(SessionRetentionReason::Bytes);
                }
            }
        }

        let mut candidates = Vec::new();
        for entry in &eligible {
            let Some(reasons) = selected.get(&entry.session_ref) else {
                continue;
            };
            let mut reasons = reasons.clone();
            reasons.sort_unstable();
            reasons.dedup();
            candidates.push(SessionRetentionCandidate {
                delete_preview: self.preview_delete_entry(entry, protected_paths)?,
                reasons,
            });
        }
        let selected_bytes = candidates.iter().fold(0_u64, |total, candidate| {
            total.saturating_add(candidate.delete_preview.source_bytes)
        });
        let remaining_sessions = total_ready_sessions.saturating_sub(candidates.len());
        let remaining_bytes = total_ready_bytes.saturating_sub(selected_bytes);
        let has_expired_remaining = policy.expire_older_than_ms.is_some_and(|max_age| {
            ready.iter().any(|entry| {
                generated_at_unix_ms.saturating_sub(entry.modified_at_unix_ms) > max_age
                    && !selected.contains_key(&entry.session_ref)
            })
        });
        let constraints_satisfied = policy
            .max_sessions
            .is_none_or(|max| remaining_sessions <= max)
            && policy.max_bytes.is_none_or(|max| remaining_bytes <= max)
            && !has_expired_remaining
            && ineligible_sessions == 0;
        let preview_digest = retention_preview_digest(
            &policy,
            generated_at_unix_ms,
            total_ready_sessions,
            total_ready_bytes,
            protected_sessions,
            pinned_sessions,
            ineligible_sessions,
            selected_bytes,
            constraints_satisfied,
            &candidates,
        )?;
        Ok(SessionRetentionPreview {
            policy,
            generated_at_unix_ms,
            total_ready_sessions,
            total_ready_bytes,
            protected_sessions,
            pinned_sessions,
            ineligible_sessions,
            selected_bytes,
            constraints_satisfied,
            candidates,
            preview_digest,
        })
    }

    /// Applies an exact retention preview after full-batch pin/protection/lease/hash preflight.
    ///
    /// # Errors
    ///
    /// Returns before the first delete when any candidate or preview binding has drifted. An I/O
    /// failure after the batch plan is durable may leave a truthful partial batch and uncertain
    /// batch recovery state; completed per-session deletes remain individually audited.
    pub fn apply_retention(
        &self,
        preview: &SessionRetentionPreview,
        protected_paths: &[PathBuf],
        applied_at_unix_ms: u64,
    ) -> Result<SessionRetentionOutput> {
        validate_retention_preview(preview)?;
        if preview.candidates.is_empty() {
            bail!("retention preview has no deletion candidates");
        }
        let _maintenance = self.acquire_maintenance_lease()?;
        for candidate in &preview.candidates {
            if self.is_session_pinned(
                &candidate.delete_preview.source_session_ref,
                &candidate.delete_preview.source_session_id,
            )? {
                bail!("retention candidate became pinned after preview");
            }
        }
        let mut leases = Vec::with_capacity(preview.candidates.len());
        for candidate in &preview.candidates {
            leases.push(self.preflight_delete(&candidate.delete_preview, protected_paths)?);
        }
        let binding = LocalSessionRetentionJournalBinding {
            preview_digest: preview.preview_digest.clone(),
            candidate_count: preview.candidates.len(),
            candidate_bytes: preview.selected_bytes,
        };
        let operation_id = format!("session-retention:{}", uuid::Uuid::new_v4());
        self.lifecycle_journal().append(
            &operation_id,
            applied_at_unix_ms,
            LocalSessionLifecycleEvent::RetentionBatchPlanned(binding.clone()),
        )?;
        let mut delete_outputs = Vec::with_capacity(preview.candidates.len());
        for (candidate, lease) in preview.candidates.iter().zip(leases) {
            delete_outputs.push(self.apply_delete_after_preflight(
                &candidate.delete_preview,
                lease,
                applied_at_unix_ms,
            )?);
        }
        let completed = self.lifecycle_journal().append(
            &operation_id,
            applied_at_unix_ms,
            LocalSessionLifecycleEvent::RetentionBatchCompleted(binding),
        )?;
        Ok(SessionRetentionOutput {
            operation_id,
            deleted_sessions: delete_outputs.len(),
            deleted_bytes: preview.selected_bytes,
            delete_outputs,
            journal_sequence: completed.sequence,
        })
    }
}

fn selected_bytes(
    entries: &[&LocalSessionCatalogEntry],
    selected: &BTreeMap<sigil_kernel::SessionRef, Vec<SessionRetentionReason>>,
) -> u64 {
    entries
        .iter()
        .filter(|entry| selected.contains_key(&entry.session_ref))
        .fold(0_u64, |total, entry| total.saturating_add(entry.bytes))
}

fn is_protected(path: &std::path::Path, protected_paths: &[PathBuf]) -> bool {
    let Ok(path) = std::fs::canonicalize(path) else {
        return true;
    };
    protected_paths
        .iter()
        .filter_map(|protected| std::fs::canonicalize(protected).ok())
        .any(|protected| protected == path)
}

#[allow(clippy::too_many_arguments)]
fn retention_preview_digest(
    policy: &SessionRetentionPolicy,
    generated_at_unix_ms: u64,
    total_ready_sessions: usize,
    total_ready_bytes: u64,
    protected_sessions: usize,
    pinned_sessions: usize,
    ineligible_sessions: usize,
    selected_bytes: u64,
    constraints_satisfied: bool,
    candidates: &[SessionRetentionCandidate],
) -> Result<String> {
    digest_serializable(&(
        policy,
        generated_at_unix_ms,
        total_ready_sessions,
        total_ready_bytes,
        protected_sessions,
        pinned_sessions,
        ineligible_sessions,
        selected_bytes,
        constraints_satisfied,
        candidates,
    ))
}

fn validate_retention_preview(preview: &SessionRetentionPreview) -> Result<()> {
    let expected = retention_preview_digest(
        &preview.policy,
        preview.generated_at_unix_ms,
        preview.total_ready_sessions,
        preview.total_ready_bytes,
        preview.protected_sessions,
        preview.pinned_sessions,
        preview.ineligible_sessions,
        preview.selected_bytes,
        preview.constraints_satisfied,
        &preview.candidates,
    )?;
    if expected != preview.preview_digest {
        return Err(anyhow!("session retention preview digest does not match"));
    }
    Ok(())
}
