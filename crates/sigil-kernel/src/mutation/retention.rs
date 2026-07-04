use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{DurableEventType, JsonlSessionStore, StoredEvent, verification::WorkspaceId};

use super::{
    MutationArtifactGroup, MutationArtifactId, MutationArtifactLifecycleRecorded,
    MutationArtifactLifecycleStatus, MutationEventRecorder, MutationPrepared, OperationId,
    SnapshotCoverage, locate_mutation_artifacts, remove_file_if_exists,
    scan_mutation_artifact_groups, short_hash, sync_existing_dir, unix_time_ms,
};

/// Durable payload recorded when a user or maintenance flow explicitly starts artifact cleanup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct MutationArtifactCleanupRequested {
    pub target: MutationArtifactCleanupTarget,
    pub policy: MutationArtifactRetentionPolicy,
    pub scanned_artifacts: usize,
    pub scanned_bytes: u64,
    pub candidate_artifacts: usize,
    pub candidate_bytes: u64,
}

/// Retention and quota limits for mutation artifacts.
///
/// The scanner only removes artifact content through audited lifecycle events. It does not rewrite
/// historical mutation events that reference the artifact id.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct MutationArtifactRetentionPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_artifacts: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expire_older_than_ms: Option<u64>,
}

/// Coarse cleanup target selected by product surfaces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "target", content = "workspace_id")]
pub enum MutationArtifactCleanupTarget {
    /// Use the configured retention policy: expired/quota-selected artifacts and unavailable blobs.
    Recommended,
    /// Clean only artifacts selected by age/count/byte retention limits.
    Expired,
    /// Clean only artifacts whose metadata exists but blob content is missing or corrupt.
    Unavailable,
    /// Clean artifact blobs that are not referenced by the current session event stream.
    Unreferenced,
    /// Clean all artifact blobs captured for the provided workspace id.
    Workspace(WorkspaceId),
}

/// Summary produced by one mutation artifact retention scan.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MutationArtifactRetentionReport {
    pub scanned_artifacts: usize,
    pub scanned_bytes: u64,
    pub expired_artifacts: usize,
    pub expired_bytes: u64,
    pub deleted_artifacts: usize,
    pub deleted_bytes: u64,
    pub unavailable_artifacts: usize,
    pub lifecycle_events: Vec<StoredEvent>,
}

impl MutationArtifactRetentionReport {
    /// Number of artifacts selected by the recommended cleanup preview.
    #[must_use]
    pub fn cleanup_candidate_artifacts(&self) -> usize {
        self.expired_artifacts
            .saturating_add(self.deleted_artifacts)
            .saturating_add(self.unavailable_artifacts)
    }

    /// Bytes selected by the recommended cleanup preview.
    #[must_use]
    pub fn cleanup_candidate_bytes(&self) -> u64 {
        self.expired_bytes.saturating_add(self.deleted_bytes)
    }

    /// Whether a product surface should show a cleanup recommendation.
    #[must_use]
    pub fn has_cleanup_candidates(&self) -> bool {
        self.cleanup_candidate_artifacts() > 0
    }
}

/// Read-only metadata for mutation artifact inventory views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationArtifactInventoryItem {
    pub artifact_id: MutationArtifactId,
    pub size: u64,
    pub created_at_ms: Option<u64>,
    pub blob_available: bool,
    pub operation_ids: Vec<OperationId>,
    pub source_paths: Vec<PathBuf>,
}

#[derive(Debug)]
struct MutationArtifactRetentionSelection {
    groups: Vec<MutationArtifactGroup>,
    selected: Vec<MutationArtifactCleanupSelection>,
}

#[derive(Debug, Clone)]
struct MutationArtifactCleanupSelection {
    artifact_id: MutationArtifactId,
    requested_status: MutationArtifactLifecycleStatus,
    reason: &'static str,
}

impl MutationEventRecorder {
    /// Deletes mutation artifact content because the user explicitly requested cleanup.
    ///
    /// The session history remains append-only: cleanup appends a lifecycle event instead of
    /// rewriting historical mutation events that referenced the artifact.
    pub fn delete_mutation_artifact(
        &self,
        artifact_id: impl Into<MutationArtifactId>,
        reason: impl Into<String>,
    ) -> Result<StoredEvent> {
        self.remove_mutation_artifact(
            artifact_id.into(),
            MutationArtifactLifecycleStatus::Deleted,
            reason.into(),
        )
    }

    /// Expires mutation artifact content due to retention or quota policy.
    ///
    /// Callers may invoke this directly for explicit maintenance, or through
    /// `enforce_artifact_retention` for policy-driven cleanup.
    pub fn expire_mutation_artifact(
        &self,
        artifact_id: impl Into<MutationArtifactId>,
        reason: impl Into<String>,
    ) -> Result<StoredEvent> {
        self.remove_mutation_artifact(
            artifact_id.into(),
            MutationArtifactLifecycleStatus::Expired,
            reason.into(),
        )
    }

    /// Applies artifact retention and quota policy to the recorder artifact root.
    ///
    /// Missing or corrupt artifact content is treated as unavailable and emits a lifecycle event.
    /// Age and quota expiration emit `Expired` lifecycle events. The session log remains
    /// append-only; historical mutation evidence is not rewritten.
    pub fn enforce_artifact_retention(
        &self,
        policy: &MutationArtifactRetentionPolicy,
    ) -> Result<MutationArtifactRetentionReport> {
        self.enforce_artifact_cleanup_at(
            &MutationArtifactCleanupTarget::Recommended,
            policy,
            unix_time_ms(),
        )
    }

    /// Previews artifact retention and quota impact without removing content or appending events.
    pub fn preview_artifact_retention(
        &self,
        policy: &MutationArtifactRetentionPolicy,
    ) -> Result<MutationArtifactRetentionReport> {
        self.preview_artifact_cleanup_at(
            &MutationArtifactCleanupTarget::Recommended,
            policy,
            unix_time_ms(),
        )
    }

    /// Previews artifact retention using an explicit clock value.
    ///
    /// This is a read-only scan: missing or corrupt content is counted as unavailable, but no
    /// lifecycle event is appended and artifact content remains untouched.
    pub fn preview_artifact_retention_at(
        &self,
        policy: &MutationArtifactRetentionPolicy,
        now_ms: u64,
    ) -> Result<MutationArtifactRetentionReport> {
        self.preview_artifact_cleanup_at(
            &MutationArtifactCleanupTarget::Recommended,
            policy,
            now_ms,
        )
    }

    /// Previews a coarse artifact cleanup target without removing content or appending events.
    pub fn preview_artifact_cleanup(
        &self,
        target: &MutationArtifactCleanupTarget,
        policy: &MutationArtifactRetentionPolicy,
    ) -> Result<MutationArtifactRetentionReport> {
        self.preview_artifact_cleanup_at(target, policy, unix_time_ms())
    }

    /// Previews a coarse artifact cleanup target using an explicit clock value.
    pub fn preview_artifact_cleanup_at(
        &self,
        target: &MutationArtifactCleanupTarget,
        policy: &MutationArtifactRetentionPolicy,
        now_ms: u64,
    ) -> Result<MutationArtifactRetentionReport> {
        let selection = select_artifacts_for_cleanup(
            &self.artifact_root,
            self.store.path(),
            target,
            policy,
            now_ms,
        )?;
        Ok(retention_report_from_selection(&selection))
    }

    /// Lists mutation artifact metadata without reading artifact content or modifying storage.
    pub fn list_mutation_artifacts(&self) -> Result<Vec<MutationArtifactInventoryItem>> {
        let mut groups = scan_mutation_artifact_groups(&self.artifact_root)?;
        groups.sort_by(|left, right| {
            left.created_at_ms
                .cmp(&right.created_at_ms)
                .then_with(|| left.artifact_id.cmp(&right.artifact_id))
        });
        Ok(groups
            .into_iter()
            .map(|artifact| MutationArtifactInventoryItem {
                artifact_id: artifact.artifact_id,
                size: artifact.size,
                created_at_ms: artifact.created_at_ms,
                blob_available: artifact.blob_available,
                operation_ids: artifact.operation_ids,
                source_paths: artifact.source_paths,
            })
            .collect())
    }

    /// Applies artifact retention using an explicit clock value.
    ///
    /// This is primarily useful for deterministic tests and offline maintenance jobs.
    pub fn enforce_artifact_retention_at(
        &self,
        policy: &MutationArtifactRetentionPolicy,
        now_ms: u64,
    ) -> Result<MutationArtifactRetentionReport> {
        self.enforce_artifact_cleanup_at(
            &MutationArtifactCleanupTarget::Recommended,
            policy,
            now_ms,
        )
    }

    /// Applies a coarse artifact cleanup target.
    ///
    /// Cleanup appends lifecycle records for every removed artifact. It never rewrites historical
    /// mutation events that may still reference cleaned artifact ids.
    pub fn enforce_artifact_cleanup(
        &self,
        target: &MutationArtifactCleanupTarget,
        policy: &MutationArtifactRetentionPolicy,
    ) -> Result<MutationArtifactRetentionReport> {
        self.enforce_artifact_cleanup_at(target, policy, unix_time_ms())
    }

    /// Applies a coarse artifact cleanup target using an explicit clock value.
    pub fn enforce_artifact_cleanup_at(
        &self,
        target: &MutationArtifactCleanupTarget,
        policy: &MutationArtifactRetentionPolicy,
        now_ms: u64,
    ) -> Result<MutationArtifactRetentionReport> {
        let selection = select_artifacts_for_cleanup(
            &self.artifact_root,
            self.store.path(),
            target,
            policy,
            now_ms,
        )?;
        let preview_report = retention_report_from_selection(&selection);
        self.append_artifact_cleanup_requested(&MutationArtifactCleanupRequested {
            target: target.clone(),
            policy: policy.clone(),
            scanned_artifacts: preview_report.scanned_artifacts,
            scanned_bytes: preview_report.scanned_bytes,
            candidate_artifacts: preview_report.cleanup_candidate_artifacts(),
            candidate_bytes: preview_report.cleanup_candidate_bytes(),
        })?;
        let artifact_sizes = selection
            .groups
            .iter()
            .map(|artifact| (artifact.artifact_id.clone(), artifact.size))
            .collect::<BTreeMap<_, _>>();
        let mut report = MutationArtifactRetentionReport {
            scanned_artifacts: selection.groups.len(),
            scanned_bytes: selection
                .groups
                .iter()
                .fold(0_u64, |total, artifact| total.saturating_add(artifact.size)),
            ..MutationArtifactRetentionReport::default()
        };
        for selection in selection.selected {
            let artifact_id = selection.artifact_id;
            let event = self.remove_mutation_artifact(
                artifact_id.clone(),
                selection.requested_status,
                selection.reason.to_owned(),
            )?;
            let payload =
                serde_json::from_value::<MutationArtifactLifecycleRecorded>(event.payload.clone())
                    .context("failed to decode mutation artifact lifecycle event")?;
            update_artifact_cleanup_report_counts(
                &mut report,
                payload.status,
                *artifact_sizes.get(&artifact_id).unwrap_or(&0),
            );
            report.lifecycle_events.push(event);
        }

        Ok(report)
    }

    fn remove_mutation_artifact(
        &self,
        artifact_id: MutationArtifactId,
        requested_status: MutationArtifactLifecycleStatus,
        reason: String,
    ) -> Result<StoredEvent> {
        let located = locate_mutation_artifacts(&self.artifact_root, &artifact_id)?;
        if located.is_empty() {
            let payload = MutationArtifactLifecycleRecorded {
                artifact_id,
                status: MutationArtifactLifecycleStatus::Unavailable,
                reason,
                content_hash: None,
                size: None,
                operation_ids: Vec::new(),
                source_paths: Vec::new(),
            };
            return self.append_artifact_lifecycle_recorded(&payload);
        }

        let content_hash = located
            .first()
            .map(|artifact| artifact.metadata.content_hash.clone());
        let size = located.first().map(|artifact| artifact.metadata.size);
        let mut operation_ids = located
            .iter()
            .map(|artifact| artifact.metadata.operation_id.clone())
            .collect::<Vec<_>>();
        operation_ids.sort();
        operation_ids.dedup();
        let mut source_paths = located
            .iter()
            .map(|artifact| artifact.metadata.source_path.clone())
            .collect::<Vec<_>>();
        source_paths.sort();
        source_paths.dedup();
        let any_blob_available = located.iter().any(|artifact| artifact.blob_available);
        let status = if any_blob_available {
            requested_status
        } else {
            MutationArtifactLifecycleStatus::Unavailable
        };
        let mut synced_parents = Vec::<PathBuf>::new();
        for artifact in &located {
            remove_file_if_exists(&artifact.blob_path)?;
            remove_file_if_exists(&artifact.metadata_path)?;
            if let Some(parent) = artifact.blob_path.parent() {
                synced_parents.push(parent.to_path_buf());
            }
            if let Some(parent) = artifact.metadata_path.parent() {
                synced_parents.push(parent.to_path_buf());
            }
        }
        synced_parents.sort();
        synced_parents.dedup();
        for parent in synced_parents {
            sync_existing_dir(&parent)?;
        }
        if self.artifact_root.exists() {
            sync_existing_dir(&self.artifact_root)?;
        }

        let payload = MutationArtifactLifecycleRecorded {
            artifact_id,
            status,
            reason,
            content_hash,
            size,
            operation_ids,
            source_paths,
        };
        self.append_artifact_lifecycle_recorded(&payload)
    }
}

fn select_artifacts_for_cleanup(
    artifact_root: &Path,
    session_log_path: &Path,
    target: &MutationArtifactCleanupTarget,
    policy: &MutationArtifactRetentionPolicy,
    now_ms: u64,
) -> Result<MutationArtifactRetentionSelection> {
    let mut groups = scan_mutation_artifact_groups(artifact_root)?;
    groups.sort_by(|left, right| {
        left.created_at_ms
            .cmp(&right.created_at_ms)
            .then_with(|| left.artifact_id.cmp(&right.artifact_id))
    });

    match target {
        MutationArtifactCleanupTarget::Recommended => {
            Ok(select_recommended_artifacts(groups, policy, now_ms))
        }
        MutationArtifactCleanupTarget::Expired => {
            Ok(select_expired_artifacts(groups, policy, now_ms))
        }
        MutationArtifactCleanupTarget::Unavailable => Ok(select_unavailable_artifacts(groups)),
        MutationArtifactCleanupTarget::Unreferenced => {
            select_unreferenced_artifacts(groups, session_log_path)
        }
        MutationArtifactCleanupTarget::Workspace(workspace_id) => {
            Ok(select_workspace_artifacts(groups, workspace_id))
        }
    }
}

fn select_recommended_artifacts(
    groups: Vec<MutationArtifactGroup>,
    policy: &MutationArtifactRetentionPolicy,
    now_ms: u64,
) -> MutationArtifactRetentionSelection {
    let mut selected = Vec::<MutationArtifactCleanupSelection>::new();
    let mut selected_ids = BTreeSet::<MutationArtifactId>::new();
    for artifact in &groups {
        if !artifact.blob_available {
            if selected_ids.insert(artifact.artifact_id.clone()) {
                selected.push(MutationArtifactCleanupSelection {
                    artifact_id: artifact.artifact_id.clone(),
                    requested_status: MutationArtifactLifecycleStatus::Expired,
                    reason: "retention scan found unavailable content",
                });
            }
            continue;
        }
        if policy.expire_older_than_ms.is_some_and(|limit| {
            artifact
                .created_at_ms
                .is_some_and(|created_at| now_ms.saturating_sub(created_at) >= limit)
        }) && selected_ids.insert(artifact.artifact_id.clone())
        {
            selected.push(MutationArtifactCleanupSelection {
                artifact_id: artifact.artifact_id.clone(),
                requested_status: MutationArtifactLifecycleStatus::Expired,
                reason: "retention age limit",
            });
        }
    }

    let mut remaining_count = groups
        .iter()
        .filter(|artifact| !selected_ids.contains(&artifact.artifact_id))
        .count();
    let mut remaining_bytes = groups
        .iter()
        .filter(|artifact| !selected_ids.contains(&artifact.artifact_id))
        .fold(0_u64, |total, artifact| total.saturating_add(artifact.size));

    for artifact in &groups {
        if selected_ids.contains(&artifact.artifact_id) {
            continue;
        }
        let exceeds_count = policy
            .max_artifacts
            .is_some_and(|max_artifacts| remaining_count > max_artifacts);
        let exceeds_bytes = policy
            .max_bytes
            .is_some_and(|max_bytes| remaining_bytes > max_bytes);
        if !exceeds_count && !exceeds_bytes {
            continue;
        }
        selected_ids.insert(artifact.artifact_id.clone());
        selected.push(MutationArtifactCleanupSelection {
            artifact_id: artifact.artifact_id.clone(),
            requested_status: MutationArtifactLifecycleStatus::Expired,
            reason: "retention quota limit",
        });
        remaining_count = remaining_count.saturating_sub(1);
        remaining_bytes = remaining_bytes.saturating_sub(artifact.size);
    }

    MutationArtifactRetentionSelection { groups, selected }
}

fn select_expired_artifacts(
    groups: Vec<MutationArtifactGroup>,
    policy: &MutationArtifactRetentionPolicy,
    now_ms: u64,
) -> MutationArtifactRetentionSelection {
    let mut selected = Vec::<MutationArtifactCleanupSelection>::new();
    let mut selected_ids = BTreeSet::<MutationArtifactId>::new();
    for artifact in &groups {
        if artifact.blob_available
            && policy.expire_older_than_ms.is_some_and(|limit| {
                artifact
                    .created_at_ms
                    .is_some_and(|created_at| now_ms.saturating_sub(created_at) >= limit)
            })
            && selected_ids.insert(artifact.artifact_id.clone())
        {
            selected.push(MutationArtifactCleanupSelection {
                artifact_id: artifact.artifact_id.clone(),
                requested_status: MutationArtifactLifecycleStatus::Expired,
                reason: "retention age limit",
            });
        }
    }

    let mut remaining_count = groups
        .iter()
        .filter(|artifact| !selected_ids.contains(&artifact.artifact_id))
        .count();
    let mut remaining_bytes = groups
        .iter()
        .filter(|artifact| !selected_ids.contains(&artifact.artifact_id))
        .fold(0_u64, |total, artifact| total.saturating_add(artifact.size));

    for artifact in &groups {
        if selected_ids.contains(&artifact.artifact_id) {
            continue;
        }
        let exceeds_count = policy
            .max_artifacts
            .is_some_and(|max_artifacts| remaining_count > max_artifacts);
        let exceeds_bytes = policy
            .max_bytes
            .is_some_and(|max_bytes| remaining_bytes > max_bytes);
        if !artifact.blob_available || (!exceeds_count && !exceeds_bytes) {
            continue;
        }
        selected_ids.insert(artifact.artifact_id.clone());
        selected.push(MutationArtifactCleanupSelection {
            artifact_id: artifact.artifact_id.clone(),
            requested_status: MutationArtifactLifecycleStatus::Expired,
            reason: "retention quota limit",
        });
        remaining_count = remaining_count.saturating_sub(1);
        remaining_bytes = remaining_bytes.saturating_sub(artifact.size);
    }

    MutationArtifactRetentionSelection { groups, selected }
}

fn select_unavailable_artifacts(
    groups: Vec<MutationArtifactGroup>,
) -> MutationArtifactRetentionSelection {
    let selected = groups
        .iter()
        .filter(|artifact| !artifact.blob_available)
        .map(|artifact| MutationArtifactCleanupSelection {
            artifact_id: artifact.artifact_id.clone(),
            requested_status: MutationArtifactLifecycleStatus::Unavailable,
            reason: "artifact content unavailable",
        })
        .collect();
    MutationArtifactRetentionSelection { groups, selected }
}

fn select_unreferenced_artifacts(
    groups: Vec<MutationArtifactGroup>,
    session_log_path: &Path,
) -> Result<MutationArtifactRetentionSelection> {
    let referenced_artifacts = referenced_mutation_artifact_ids(session_log_path)?;
    let selected = groups
        .iter()
        .filter(|artifact| !referenced_artifacts.contains(&artifact.artifact_id))
        .map(|artifact| MutationArtifactCleanupSelection {
            artifact_id: artifact.artifact_id.clone(),
            requested_status: MutationArtifactLifecycleStatus::Deleted,
            reason: "artifact metadata is not referenced by session events",
        })
        .collect();
    Ok(MutationArtifactRetentionSelection { groups, selected })
}

fn select_workspace_artifacts(
    groups: Vec<MutationArtifactGroup>,
    workspace_id: &WorkspaceId,
) -> MutationArtifactRetentionSelection {
    let workspace_id_hash = short_hash(workspace_id);
    let selected = groups
        .iter()
        .filter(|artifact| artifact.workspace_id_hashes.contains(&workspace_id_hash))
        .map(|artifact| MutationArtifactCleanupSelection {
            artifact_id: artifact.artifact_id.clone(),
            requested_status: MutationArtifactLifecycleStatus::Deleted,
            reason: "user requested workspace artifact cleanup",
        })
        .collect();
    MutationArtifactRetentionSelection { groups, selected }
}

fn retention_report_from_selection(
    selection: &MutationArtifactRetentionSelection,
) -> MutationArtifactRetentionReport {
    let artifact_groups = selection
        .groups
        .iter()
        .map(|artifact| (artifact.artifact_id.clone(), artifact))
        .collect::<BTreeMap<_, _>>();
    let mut report = MutationArtifactRetentionReport {
        scanned_artifacts: selection.groups.len(),
        scanned_bytes: selection
            .groups
            .iter()
            .fold(0_u64, |total, artifact| total.saturating_add(artifact.size)),
        ..MutationArtifactRetentionReport::default()
    };
    for selection in &selection.selected {
        let Some(group) = artifact_groups.get(&selection.artifact_id) else {
            continue;
        };
        let status = effective_artifact_lifecycle_status(selection.requested_status, group);
        update_artifact_cleanup_report_counts(&mut report, status, group.size);
    }
    report
}

fn effective_artifact_lifecycle_status(
    requested_status: MutationArtifactLifecycleStatus,
    artifact: &MutationArtifactGroup,
) -> MutationArtifactLifecycleStatus {
    if artifact.blob_available {
        requested_status
    } else {
        MutationArtifactLifecycleStatus::Unavailable
    }
}

fn update_artifact_cleanup_report_counts(
    report: &mut MutationArtifactRetentionReport,
    status: MutationArtifactLifecycleStatus,
    size: u64,
) {
    match status {
        MutationArtifactLifecycleStatus::Deleted => {
            report.deleted_artifacts += 1;
            report.deleted_bytes = report.deleted_bytes.saturating_add(size);
        }
        MutationArtifactLifecycleStatus::Expired => {
            report.expired_artifacts += 1;
            report.expired_bytes = report.expired_bytes.saturating_add(size);
        }
        MutationArtifactLifecycleStatus::Unavailable => {
            report.unavailable_artifacts += 1;
        }
    }
}

fn referenced_mutation_artifact_ids(
    session_log_path: &Path,
) -> Result<BTreeSet<MutationArtifactId>> {
    let mut artifact_ids = BTreeSet::new();
    for record in JsonlSessionStore::read_event_records(session_log_path)? {
        let crate::SessionStreamRecord::Stored(event) = record;
        if event.event_type != DurableEventType::MutationPrepared.as_str() {
            continue;
        }
        let payload =
            serde_json::from_value::<MutationPrepared>(event.payload).with_context(|| {
                format!(
                    "failed to decode {}",
                    DurableEventType::MutationPrepared.as_str()
                )
            })?;
        if let SnapshotCoverage::Captured(artifact_id) = payload.snapshot_coverage {
            artifact_ids.insert(artifact_id);
        }
    }
    Ok(artifact_ids)
}
