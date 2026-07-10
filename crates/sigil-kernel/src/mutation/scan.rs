use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::{
    DurableEventType, JsonlSessionStore, StoredEvent, stable_event_uuid,
    verification::{
        DEFAULT_TASK_VERIFICATION_SCOPE_HASH, FileType, SnapshotEntryState, ToolEffect,
        VerificationScope, VerificationScopeHash, WorkspaceId, WorkspaceRevision,
        WorkspaceSnapshotBuild, WorkspaceSnapshotEntry, WorkspaceSnapshotId,
        WorkspaceSnapshotManifestV1, build_workspace_snapshot, stable_workspace_id,
    },
};

use super::{
    ExecutionMutationProfile, MutationCommitted, MutationEventRecorder, MutationObservedState,
    MutationPrepared, MutationReconciled, MutationResolution, MutationSubject, OperationId,
    PreparedFileMutation, ToolCallId, WorkspaceMutationDetected, WorkspaceMutationDetectionReason,
    WorkspaceMutationScan, directory_state_hash, file_content_hash,
};

impl MutationEventRecorder {
    /// Reconciles one live prepared file mutation against its current on-disk state.
    ///
    /// Applied or conflicting states advance the workspace revision and carry a content-bound
    /// snapshot. An unreadable state is closed as unknown dirty without claiming a snapshot.
    pub(super) fn reconcile_prepared_file_from_disk_under_lease(
        &self,
        prepared: &PreparedFileMutation,
    ) -> Result<StoredEvent> {
        let observed_hash = file_content_hash(&prepared.absolute_path).ok();
        let (observed_state, resolution, workspace_revision, workspace_snapshot_id) =
            match observed_hash {
                Some(observed_hash) if observed_hash == prepared.before_hash => (
                    MutationObservedState::NotApplied,
                    MutationResolution::MarkNotApplied,
                    None,
                    None,
                ),
                Some(observed_hash) => {
                    let revision = latest_workspace_revision(&self.store, &prepared.workspace_id)?
                        .max(prepared.base_workspace_revision)
                        .saturating_add(1);
                    let snapshot_id = single_subject_snapshot_id(
                        &prepared.workspace_id,
                        &prepared.relative_path,
                        FileType::File,
                        observed_hash.clone(),
                    )?;
                    if observed_hash == prepared.intended_after_hash {
                        (
                            MutationObservedState::AppliedAsIntended,
                            MutationResolution::MarkCommitted,
                            Some(revision),
                            Some(snapshot_id),
                        )
                    } else {
                        (
                            MutationObservedState::AppliedDifferently,
                            MutationResolution::MarkConflict,
                            Some(revision),
                            Some(snapshot_id),
                        )
                    }
                }
                None => (
                    MutationObservedState::Unknown,
                    MutationResolution::MarkUnknownDirty,
                    None,
                    None,
                ),
            };
        self.append_reconciled(&MutationReconciled {
            operation_id: prepared.operation_id.clone(),
            batch_id: prepared.batch_id.clone(),
            observed_state,
            resolution,
            workspace_revision,
            workspace_snapshot_id,
        })
    }

    /// Reconciles prepared mutations that were persisted without a terminal commit.
    ///
    /// This is intentionally conservative: it never replays a mutation. It only records what the
    /// workspace currently looks like so downstream verification can treat the affected workspace
    /// snapshot as stale or conflicted.
    pub fn reconcile_prepared_mutations(
        &self,
        workspace_root: impl AsRef<Path>,
    ) -> Result<Vec<StoredEvent>> {
        let workspace_root = fs::canonicalize(workspace_root.as_ref()).with_context(|| {
            format!(
                "failed to canonicalize {}",
                workspace_root.as_ref().display()
            )
        })?;
        let _workspace_lease = self.acquire_workspace_mutation_lease(&workspace_root)?;
        let workspace_id = stable_workspace_id(&workspace_root)?;
        let mut prepared = Vec::new();
        let mut committed = Vec::new();
        let mut reconciled = Vec::new();

        for record in JsonlSessionStore::read_event_records(self.store.path())? {
            let crate::SessionStreamRecord::Stored(event) = record else {
                continue;
            };
            match DurableEventType::from_event_type(&event.event_type) {
                Some(DurableEventType::MutationPrepared) => {
                    let payload = serde_json::from_value::<MutationPrepared>(event.payload.clone())
                        .with_context(|| {
                            format!(
                                "failed to decode {}",
                                DurableEventType::MutationPrepared.as_str()
                            )
                        })?;
                    if payload.workspace_id == workspace_id {
                        prepared.push(payload);
                    }
                }
                Some(DurableEventType::MutationCommitted) => {
                    let payload =
                        serde_json::from_value::<MutationCommitted>(event.payload.clone())
                            .with_context(|| {
                                format!(
                                    "failed to decode {}",
                                    DurableEventType::MutationCommitted.as_str()
                                )
                            })?;
                    committed.push(payload);
                }
                Some(DurableEventType::MutationReconciled) => {
                    let payload =
                        serde_json::from_value::<MutationReconciled>(event.payload.clone())
                            .with_context(|| {
                                format!(
                                    "failed to decode {}",
                                    DurableEventType::MutationReconciled.as_str()
                                )
                            })?;
                    reconciled.push(payload);
                }
                _ => {}
            }
        }

        let target_operation_ids = prepared
            .iter()
            .map(|payload| payload.operation_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let mut terminal_operation_ids = std::collections::BTreeSet::new();
        let mut latest_revision = prepared
            .iter()
            .map(|payload| payload.base_workspace_revision)
            .max()
            .unwrap_or_default();
        for payload in committed {
            if payload.workspace_id.as_deref() == Some(workspace_id.as_str())
                || (payload.workspace_id.is_none()
                    && target_operation_ids.contains(payload.operation_id.as_str()))
            {
                latest_revision = latest_revision.max(payload.workspace_revision);
                terminal_operation_ids.insert(payload.operation_id);
            }
        }
        for payload in reconciled {
            if target_operation_ids.contains(payload.operation_id.as_str()) {
                if let Some(revision) = payload.workspace_revision {
                    latest_revision = latest_revision.max(revision);
                }
                terminal_operation_ids.insert(payload.operation_id);
            }
        }

        let mut events = Vec::new();
        for payload in prepared {
            if terminal_operation_ids.contains(&payload.operation_id) {
                continue;
            }
            let (relative_path, observed_hash, subject_kind) = match &payload.subject {
                MutationSubject::File { path, .. } => (
                    path,
                    file_content_hash(&workspace_root.join(path))?,
                    FileType::File,
                ),
                MutationSubject::Directory { path } => (
                    path,
                    directory_state_hash(&workspace_root.join(path))?,
                    FileType::Directory,
                ),
                _ => {
                    let event = self.append_reconciled(&MutationReconciled {
                        operation_id: payload.operation_id,
                        batch_id: payload.batch_id,
                        observed_state: MutationObservedState::Unknown,
                        resolution: MutationResolution::MarkUnknownDirty,
                        workspace_revision: None,
                        workspace_snapshot_id: None,
                    })?;
                    events.push(event);
                    continue;
                }
            };
            let (observed_state, resolution, workspace_revision, workspace_snapshot_id) =
                if observed_hash == payload.before_hash {
                    (
                        MutationObservedState::NotApplied,
                        MutationResolution::MarkNotApplied,
                        None,
                        None,
                    )
                } else if observed_hash == payload.intended_after_hash {
                    let revision = latest_revision.saturating_add(1);
                    latest_revision = revision;
                    let snapshot_id = single_subject_snapshot_id(
                        &payload.workspace_id,
                        relative_path,
                        subject_kind,
                        observed_hash,
                    )?;
                    (
                        MutationObservedState::AppliedAsIntended,
                        MutationResolution::MarkCommitted,
                        Some(revision),
                        Some(snapshot_id),
                    )
                } else {
                    let revision = latest_revision.saturating_add(1);
                    latest_revision = revision;
                    let snapshot_id = single_subject_snapshot_id(
                        &payload.workspace_id,
                        relative_path,
                        subject_kind,
                        observed_hash,
                    )?;
                    (
                        MutationObservedState::AppliedDifferently,
                        MutationResolution::MarkConflict,
                        Some(revision),
                        Some(snapshot_id),
                    )
                };

            let event = self.append_reconciled(&MutationReconciled {
                operation_id: payload.operation_id,
                batch_id: payload.batch_id,
                observed_state,
                resolution,
                workspace_revision,
                workspace_snapshot_id,
            })?;
            events.push(event);
        }
        Ok(events)
    }

    /// Captures the verification-scope workspace state before or after an unknown-effect tool.
    ///
    /// The snapshot id is content-bound when every entry is complete. Incomplete coverage is
    /// preserved as `WorkspaceKnowledge::UnknownDirty` so verification can fail closed.
    pub fn capture_workspace_scan(
        &self,
        workspace_root: impl AsRef<Path>,
        scope: &VerificationScope,
    ) -> Result<WorkspaceMutationScan> {
        let workspace_root = workspace_root.as_ref();
        let workspace_id = stable_workspace_id(workspace_root)?;
        let workspace_revision = latest_workspace_revision(&self.store, &workspace_id)?;
        let snapshot = build_workspace_snapshot(
            workspace_root,
            workspace_id.clone(),
            scope,
            workspace_revision,
        )?;
        Ok(workspace_scan_from_snapshot(
            workspace_id,
            scope.clone(),
            workspace_revision,
            snapshot,
        ))
    }

    /// Records a workspace mutation if the after-snapshot differs from the before-snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if the after snapshot cannot be captured or if the durable mutation event
    /// cannot be appended.
    pub fn record_workspace_mutation_if_changed(
        &self,
        before: &WorkspaceMutationScan,
        workspace_root: impl AsRef<Path>,
        tool_call_id: impl Into<ToolCallId>,
        tool_name: impl Into<String>,
        tool_effect: ToolEffect,
    ) -> Result<Option<StoredEvent>> {
        let after = self.capture_workspace_scan(workspace_root, &before.scope)?;
        self.record_workspace_mutation_scan_result(
            before,
            &after,
            tool_call_id,
            tool_name,
            tool_effect,
        )
    }

    pub fn record_workspace_mutation_scan_result(
        &self,
        before: &WorkspaceMutationScan,
        after: &WorkspaceMutationScan,
        tool_call_id: impl Into<ToolCallId>,
        tool_name: impl Into<String>,
        tool_effect: ToolEffect,
    ) -> Result<Option<StoredEvent>> {
        let reason = workspace_mutation_detection_reason(before, after);
        let Some(reason) = reason else {
            return Ok(None);
        };
        let tool_call_id = tool_call_id.into();
        let tool_name = tool_name.into();
        let workspace_revision = latest_workspace_revision(&self.store, &before.workspace_id)?
            .max(before.workspace_revision)
            .saturating_add(1);
        let payload = WorkspaceMutationDetected {
            operation_id: workspace_detection_operation_id(
                &before.workspace_id,
                &tool_call_id,
                before.workspace_snapshot_id.as_deref(),
                after.workspace_snapshot_id.as_deref(),
                reason,
            ),
            tool_call_id: Some(tool_call_id),
            tool_name,
            tool_effect,
            workspace_id: before.workspace_id.clone(),
            scope_hash: before.scope_hash.clone(),
            from_workspace_snapshot_id: before.workspace_snapshot_id.clone(),
            to_workspace_snapshot_id: after.workspace_snapshot_id.clone(),
            base_workspace_revision: before.workspace_revision,
            workspace_revision,
            reason,
            unknown_dirty: before.workspace_knowledge.is_unknown_dirty()
                || after.workspace_knowledge.is_unknown_dirty(),
            metadata: BTreeMap::new(),
        };
        self.append_workspace_mutation_detected(&payload).map(Some)
    }

    /// Records a non-tool external process mutation if its after-snapshot differs.
    ///
    /// This is used for process lifecycle paths such as MCP server startup, where the runtime
    /// has no model-visible tool call id but can still compare workspace snapshots before and
    /// after the process boundary.
    pub fn record_external_process_mutation_scan_result(
        &self,
        before: &WorkspaceMutationScan,
        after: &WorkspaceMutationScan,
        process_name: impl Into<String>,
        tool_effect: ToolEffect,
        metadata: BTreeMap<String, String>,
    ) -> Result<Option<StoredEvent>> {
        let reason = workspace_mutation_detection_reason(before, after);
        let Some(reason) = reason else {
            return Ok(None);
        };
        let process_name = process_name.into();
        let workspace_revision = latest_workspace_revision(&self.store, &before.workspace_id)?
            .max(before.workspace_revision)
            .saturating_add(1);
        let payload = WorkspaceMutationDetected {
            operation_id: workspace_detection_operation_id(
                &before.workspace_id,
                &process_name,
                before.workspace_snapshot_id.as_deref(),
                after.workspace_snapshot_id.as_deref(),
                reason,
            ),
            tool_call_id: None,
            tool_name: process_name,
            tool_effect,
            workspace_id: before.workspace_id.clone(),
            scope_hash: before.scope_hash.clone(),
            from_workspace_snapshot_id: before.workspace_snapshot_id.clone(),
            to_workspace_snapshot_id: after.workspace_snapshot_id.clone(),
            base_workspace_revision: before.workspace_revision,
            workspace_revision,
            reason,
            unknown_dirty: before.workspace_knowledge.is_unknown_dirty()
                || after.workspace_knowledge.is_unknown_dirty(),
            metadata,
        };
        self.append_workspace_mutation_detected(&payload).map(Some)
    }

    /// Records an unknown-dirty mutation when scan coverage is unavailable.
    pub fn record_workspace_scan_unavailable(
        &self,
        workspace_root: impl AsRef<Path>,
        tool_call_id: impl Into<ToolCallId>,
        tool_name: impl Into<String>,
        tool_effect: ToolEffect,
    ) -> Result<StoredEvent> {
        let workspace_root = workspace_root.as_ref();
        let workspace_id = stable_workspace_id(workspace_root)?;
        let workspace_revision = latest_workspace_revision(&self.store, &workspace_id)?;
        let tool_call_id = tool_call_id.into();
        let tool_name = tool_name.into();
        let payload = WorkspaceMutationDetected {
            operation_id: workspace_detection_operation_id(
                &workspace_id,
                &tool_call_id,
                None,
                None,
                WorkspaceMutationDetectionReason::ScanUnavailable,
            ),
            tool_call_id: Some(tool_call_id),
            tool_name,
            tool_effect,
            workspace_id,
            scope_hash: DEFAULT_TASK_VERIFICATION_SCOPE_HASH.to_owned(),
            from_workspace_snapshot_id: None,
            to_workspace_snapshot_id: None,
            base_workspace_revision: workspace_revision,
            workspace_revision: workspace_revision.saturating_add(1),
            reason: WorkspaceMutationDetectionReason::ScanUnavailable,
            unknown_dirty: true,
            metadata: BTreeMap::new(),
        };
        self.append_workspace_mutation_detected(&payload)
    }

    /// Records an unknown-dirty non-tool external process mutation after a failed after-scan.
    pub fn record_external_process_scan_unavailable_after(
        &self,
        before: &WorkspaceMutationScan,
        process_name: impl Into<String>,
        tool_effect: ToolEffect,
        metadata: BTreeMap<String, String>,
    ) -> Result<StoredEvent> {
        let process_name = process_name.into();
        let workspace_revision = latest_workspace_revision(&self.store, &before.workspace_id)?
            .max(before.workspace_revision)
            .saturating_add(1);
        let payload = WorkspaceMutationDetected {
            operation_id: workspace_detection_operation_id(
                &before.workspace_id,
                &process_name,
                before.workspace_snapshot_id.as_deref(),
                None,
                WorkspaceMutationDetectionReason::ScanUnavailable,
            ),
            tool_call_id: None,
            tool_name: process_name,
            tool_effect,
            workspace_id: before.workspace_id.clone(),
            scope_hash: before.scope_hash.clone(),
            from_workspace_snapshot_id: before.workspace_snapshot_id.clone(),
            to_workspace_snapshot_id: None,
            base_workspace_revision: before.workspace_revision,
            workspace_revision,
            reason: WorkspaceMutationDetectionReason::ScanUnavailable,
            unknown_dirty: true,
            metadata,
        };
        self.append_workspace_mutation_detected(&payload)
    }

    /// Records a conservative unknown-dirty mutation for a non-tool external process lifecycle.
    ///
    /// This is used for processes such as TUI-triggered MCP server activation where there is no
    /// model tool call id, but the process may continue mutating the workspace after startup.
    pub fn record_external_process_unknown_dirty(
        &self,
        workspace_root: impl AsRef<Path>,
        process_name: impl Into<String>,
        tool_effect: ToolEffect,
    ) -> Result<StoredEvent> {
        self.record_external_process_unknown_dirty_with_metadata(
            workspace_root,
            process_name,
            tool_effect,
            BTreeMap::new(),
        )
    }

    /// Records a conservative unknown-dirty external process lifecycle with audit metadata.
    ///
    /// Metadata is not model-visible evidence. It exists to make durable session audit and product
    /// surfaces explicit about the external process boundary that caused the unknown-dirty state.
    pub fn record_external_process_unknown_dirty_with_metadata(
        &self,
        workspace_root: impl AsRef<Path>,
        process_name: impl Into<String>,
        tool_effect: ToolEffect,
        metadata: BTreeMap<String, String>,
    ) -> Result<StoredEvent> {
        let workspace_root = workspace_root.as_ref();
        let workspace_id = stable_workspace_id(workspace_root)?;
        let workspace_revision = latest_workspace_revision(&self.store, &workspace_id)?;
        let process_name = process_name.into();
        let operation_seed = format!("{process_name}:{workspace_revision}");
        let payload = WorkspaceMutationDetected {
            operation_id: workspace_detection_operation_id(
                &workspace_id,
                &operation_seed,
                None,
                None,
                WorkspaceMutationDetectionReason::DeclaredWriteEffect,
            ),
            tool_call_id: None,
            tool_name: process_name,
            tool_effect,
            workspace_id,
            scope_hash: DEFAULT_TASK_VERIFICATION_SCOPE_HASH.to_owned(),
            from_workspace_snapshot_id: None,
            to_workspace_snapshot_id: None,
            base_workspace_revision: workspace_revision,
            workspace_revision: workspace_revision.saturating_add(1),
            reason: WorkspaceMutationDetectionReason::DeclaredWriteEffect,
            unknown_dirty: true,
            metadata,
        };
        self.append_workspace_mutation_detected(&payload)
    }

    pub fn record_workspace_scan_unavailable_after(
        &self,
        before: &WorkspaceMutationScan,
        tool_call_id: impl Into<ToolCallId>,
        tool_name: impl Into<String>,
        tool_effect: ToolEffect,
    ) -> Result<StoredEvent> {
        let tool_call_id = tool_call_id.into();
        let tool_name = tool_name.into();
        let workspace_revision = latest_workspace_revision(&self.store, &before.workspace_id)?
            .max(before.workspace_revision)
            .saturating_add(1);
        let payload = WorkspaceMutationDetected {
            operation_id: workspace_detection_operation_id(
                &before.workspace_id,
                &tool_call_id,
                before.workspace_snapshot_id.as_deref(),
                None,
                WorkspaceMutationDetectionReason::ScanUnavailable,
            ),
            tool_call_id: Some(tool_call_id),
            tool_name,
            tool_effect,
            workspace_id: before.workspace_id.clone(),
            scope_hash: before.scope_hash.clone(),
            from_workspace_snapshot_id: before.workspace_snapshot_id.clone(),
            to_workspace_snapshot_id: None,
            base_workspace_revision: before.workspace_revision,
            workspace_revision,
            reason: WorkspaceMutationDetectionReason::ScanUnavailable,
            unknown_dirty: true,
            metadata: BTreeMap::new(),
        };
        self.append_workspace_mutation_detected(&payload)
    }

    pub fn execution_mutation_profile(
        &self,
        workspace_root: impl AsRef<Path>,
        scope: &VerificationScope,
        tool_call_id: impl Into<ToolCallId>,
        tool_name: impl Into<String>,
        effect: ToolEffect,
    ) -> Result<ExecutionMutationProfile> {
        let scan = self.capture_workspace_scan(workspace_root, scope)?;
        let profile = ExecutionMutationProfile {
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            effect,
            workspace_id: scan.workspace_id,
            scan_scope_hash: scan.scope_hash,
            pre_execution_snapshot_id: scan.workspace_snapshot_id,
            pre_execution_workspace_revision: scan.workspace_revision,
            workspace_knowledge: scan.workspace_knowledge,
        };
        Ok(profile)
    }

    pub fn reconcile_execution_mutation_profile(
        &self,
        workspace_root: impl AsRef<Path>,
        profile: &ExecutionMutationProfile,
    ) -> Result<Option<StoredEvent>> {
        let latest_detection =
            self.latest_workspace_detection_for_tool_call(&profile.tool_call_id)?;
        let scope = VerificationScope::all_tracked(profile.scan_scope_hash.clone());
        let before = WorkspaceMutationScan {
            workspace_id: profile.workspace_id.clone(),
            scope_hash: profile.scan_scope_hash.clone(),
            scope: scope.clone(),
            workspace_revision: profile.pre_execution_workspace_revision,
            workspace_snapshot_id: profile.pre_execution_snapshot_id.clone(),
            workspace_knowledge: profile.workspace_knowledge.clone(),
        };
        let workspace_root = workspace_root.as_ref();
        if let Ok(after) = self.capture_workspace_scan(workspace_root, &scope) {
            if latest_detection
                .as_ref()
                .is_some_and(|detection| detection_already_covers_after_scan(detection, &after))
            {
                return Ok(None);
            }
            return self.record_workspace_mutation_scan_result(
                &before,
                &after,
                profile.tool_call_id.clone(),
                profile.tool_name.clone(),
                profile.effect,
            );
        }
        if latest_detection.as_ref().is_some_and(|detection| {
            detection.unknown_dirty
                && detection.reason == WorkspaceMutationDetectionReason::ScanUnavailable
        }) {
            return Ok(None);
        }
        let call_id = profile.tool_call_id.clone();
        let tool_name = profile.tool_name.clone();
        let effect = profile.effect;
        let event =
            self.record_workspace_scan_unavailable(workspace_root, call_id, tool_name, effect)?;
        Ok(Some(event))
    }

    fn latest_workspace_detection_for_tool_call(
        &self,
        tool_call_id: &str,
    ) -> Result<Option<WorkspaceMutationDetected>> {
        let mut latest = None;
        for record in JsonlSessionStore::read_event_records(self.store.path())? {
            let crate::SessionStreamRecord::Stored(event) = record else {
                continue;
            };
            if DurableEventType::from_event_type(&event.event_type)
                != Some(DurableEventType::WorkspaceMutationDetected)
            {
                continue;
            }
            let payload = serde_json::from_value::<WorkspaceMutationDetected>(event.payload)
                .context("failed to decode workspace mutation detected payload")?;
            if payload.tool_call_id.as_deref() == Some(tool_call_id) {
                latest = Some(payload);
            }
        }
        Ok(latest)
    }
}

pub(super) fn latest_workspace_revision(
    store: &JsonlSessionStore,
    workspace_id: &str,
) -> Result<WorkspaceRevision> {
    let mut latest = 0;
    for record in JsonlSessionStore::read_event_records(store.path())? {
        let crate::SessionStreamRecord::Stored(event) = record else {
            continue;
        };
        if event.event_type == DurableEventType::MutationCommitted.as_str() {
            if let Ok(payload) = serde_json::from_value::<MutationCommitted>(event.payload.clone())
                && payload.workspace_id.as_deref().unwrap_or(workspace_id) == workspace_id
            {
                latest = latest.max(payload.workspace_revision);
            }
        } else if event.event_type == DurableEventType::MutationReconciled.as_str()
            && let Ok(payload) = serde_json::from_value::<MutationReconciled>(event.payload.clone())
            && payload.workspace_revision.is_some()
        {
            latest = latest.max(payload.workspace_revision.unwrap_or_default());
        } else if event.event_type == DurableEventType::MutationPrepared.as_str()
            && let Ok(payload) = serde_json::from_value::<MutationPrepared>(event.payload.clone())
            && payload.workspace_id == workspace_id
        {
            latest = latest.max(payload.base_workspace_revision);
        } else if event.event_type == DurableEventType::WorkspaceMutationDetected.as_str()
            && let Ok(payload) =
                serde_json::from_value::<WorkspaceMutationDetected>(event.payload.clone())
            && payload.workspace_id == workspace_id
        {
            latest = latest.max(payload.workspace_revision);
        }
    }
    Ok(latest)
}

pub(super) fn single_subject_snapshot_id(
    workspace_id: &str,
    relative_path: &Path,
    file_type: FileType,
    observed_hash: Option<String>,
) -> Result<WorkspaceSnapshotId> {
    let state = match file_type {
        FileType::File if observed_hash.is_some() => SnapshotEntryState::Present,
        FileType::File => SnapshotEntryState::Missing,
        FileType::Directory if observed_hash.is_some() => SnapshotEntryState::Present,
        FileType::Directory => SnapshotEntryState::Missing,
        FileType::Symlink | FileType::Other => SnapshotEntryState::Unsupported,
    };
    let content_hash = if file_type == FileType::File {
        observed_hash
    } else {
        None
    };
    let manifest = WorkspaceSnapshotManifestV1 {
        workspace_id: workspace_id.to_owned(),
        scope_hash: mutation_scope_hash(relative_path),
        entries: vec![WorkspaceSnapshotEntry {
            normalized_path: relative_path.to_path_buf(),
            file_type,
            content_hash,
            mode: None,
            file_metadata: None,
            symlink_target: None,
            state,
        }],
    };
    manifest.workspace_snapshot_id()
}

fn mutation_scope_hash(relative_path: &Path) -> VerificationScopeHash {
    let digest = Sha256::digest(relative_path.to_string_lossy().as_bytes());
    format!("mutation-scope:sha256:{digest:x}")
}

fn workspace_scan_from_snapshot(
    workspace_id: WorkspaceId,
    scope: VerificationScope,
    workspace_revision: WorkspaceRevision,
    snapshot: WorkspaceSnapshotBuild,
) -> WorkspaceMutationScan {
    WorkspaceMutationScan {
        workspace_id,
        scope_hash: scope.scope_hash.clone(),
        scope,
        workspace_revision,
        workspace_snapshot_id: snapshot.workspace_snapshot_id,
        workspace_knowledge: snapshot.workspace_knowledge,
    }
}

fn workspace_mutation_detection_reason(
    before: &WorkspaceMutationScan,
    after: &WorkspaceMutationScan,
) -> Option<WorkspaceMutationDetectionReason> {
    if before.workspace_knowledge.is_unknown_dirty() {
        return Some(WorkspaceMutationDetectionReason::SnapshotIncompleteBefore);
    }
    if after.workspace_knowledge.is_unknown_dirty() {
        return Some(WorkspaceMutationDetectionReason::SnapshotIncompleteAfter);
    }
    (before.workspace_snapshot_id != after.workspace_snapshot_id)
        .then_some(WorkspaceMutationDetectionReason::SnapshotChanged)
}

fn detection_already_covers_after_scan(
    detection: &WorkspaceMutationDetected,
    after: &WorkspaceMutationScan,
) -> bool {
    detection.to_workspace_snapshot_id == after.workspace_snapshot_id
        && detection.unknown_dirty == after.workspace_knowledge.is_unknown_dirty()
}

fn workspace_detection_operation_id(
    workspace_id: &str,
    tool_call_id: &str,
    before_snapshot_id: Option<&str>,
    after_snapshot_id: Option<&str>,
    reason: WorkspaceMutationDetectionReason,
) -> OperationId {
    stable_event_uuid(
        "sigil-workspace-mutation-detected",
        &format!(
            "{workspace_id}:{tool_call_id}:{before_snapshot_id:?}:{after_snapshot_id:?}:{reason:?}"
        ),
    )
}
