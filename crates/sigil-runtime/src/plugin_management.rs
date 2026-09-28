//! Session-bound plugin review through an existing application writer capability.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sigil_kernel::{
    ControlEntry, PluginManifestSnapshot, PluginStateProjection, PluginTrustDecision,
    PluginTrustEntry, Session, plugin_manifest_digests_match,
};

use crate::{
    interactive_session_attachment::InteractiveSessionAttachmentLease,
    plugins::{PluginDiscoveryWarning, discover_workspace_plugins},
};

/// Host-only discovery material. Adapters must project bounded/redacted display fields; raw
/// manifest capabilities and diagnostic paths are not renderer DTOs.
#[derive(Debug)]
pub struct ApplicationPluginCatalog {
    pub manifests: Vec<PluginManifestSnapshot>,
    pub warnings: Vec<PluginDiscoveryWarning>,
    pub process_cleanup: BTreeMap<String, sigil_kernel::PluginCleanupStatus>,
}

/// An explicit user decision for the exact manifest and capability set they reviewed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationPluginDecisionRequest {
    pub plugin_id: String,
    pub expected_manifest_hash: String,
    pub expected_capability_digest: String,
    pub decision: PluginTrustDecision,
}

/// Durable trust publication. This is not tool approval or proof of process cleanup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplicationPluginDecisionReceipt {
    pub plugin_id: String,
    pub manifest_hash: String,
    pub capability_digest: String,
    pub decision: PluginTrustDecision,
    pub reviewed_at_ms: u64,
    pub trust_event_id: String,
    pub process_cleanup: Option<sigil_kernel::PluginCleanupStatus>,
}

/// Queries configured workspace plugins without starting an extension or acquiring a new writer.
/// `workspace_root` must be the host-resolved workspace for the attached application session.
///
/// # Errors
/// Rejects missing controller ownership, foreign session scope, unreadable session or discovery.
/// Call from a blocking boundary; this performs bounded filesystem discovery.
pub fn application_plugin_catalog(
    attachment: &InteractiveSessionAttachmentLease,
    expected_session_scope_id: &str,
    workspace_root: &Path,
) -> Result<ApplicationPluginCatalog> {
    let session = observe_attached_session(attachment, expected_session_scope_id)?;
    catalog_for_session(&session, workspace_root)
}

/// Publishes exact reviewed trust using the session's existing application writer capability.
/// An active run is not an admission restriction: current hook/MCP guards read durable trust.
/// The caller retains its normal application command receipt/serialization, and separately
/// retires any live generation through its existing registry owner after a disable decision.
///
/// # Errors
/// Rejects foreign scope, missing/changed manifest or capabilities, and durable append failure.
/// Call from a blocking boundary; this performs filesystem discovery and a durable append.
pub fn apply_application_plugin_decision(
    attachment: &InteractiveSessionAttachmentLease,
    expected_session_scope_id: &str,
    workspace_root: &Path,
    request: &ApplicationPluginDecisionRequest,
) -> Result<ApplicationPluginDecisionReceipt> {
    let owner = attachment
        .application_operation_owner()
        .context("plugin review requires the attached application writer")?;
    apply_application_plugin_decision_with_owner(
        &owner,
        expected_session_scope_id,
        workspace_root,
        request,
    )
    .map(|(receipt, _)| receipt)
}

/// Uses an already-held session writer; returns the controls actually appended for local replay.
/// Adapters retain their normal command journal and must not append the returned controls again.
///
/// # Errors
/// Rejects foreign session scope, stale review bindings, or failed durable publication.
pub fn apply_application_plugin_decision_with_owner(
    owner: &sigil_kernel::SessionApplicationOperationOwner,
    expected_session_scope_id: &str,
    workspace_root: &Path,
    request: &ApplicationPluginDecisionRequest,
) -> Result<(ApplicationPluginDecisionReceipt, Vec<ControlEntry>)> {
    let mut session = owner.attach_for_control()?;
    apply_application_plugin_decision_to_session(
        &mut session,
        expected_session_scope_id,
        workspace_root,
        request,
    )
}

/// Appends through the caller's existing Session, retaining its exact prepared operation binding.
/// Returned controls have already been persisted; adapters may replay them but must not append them.
///
/// # Errors
/// Rejects foreign scope, changed declaration/capability binding, or durable publication failure.
pub fn apply_application_plugin_decision_to_session(
    session: &mut Session,
    expected_session_scope_id: &str,
    workspace_root: &Path,
    request: &ApplicationPluginDecisionRequest,
) -> Result<(ApplicationPluginDecisionReceipt, Vec<ControlEntry>)> {
    ensure!(
        !expected_session_scope_id.is_empty()
            && session.session_scope_id() == expected_session_scope_id,
        "plugin review session scope mismatch"
    );
    let catalog = catalog_for_session(session, workspace_root)?;
    let mut candidates = catalog
        .manifests
        .into_iter()
        .filter(|manifest| manifest.plugin_id == request.plugin_id);
    let snapshot = candidates
        .next()
        .context("reviewed plugin is no longer available")?;
    ensure!(
        candidates.next().is_none(),
        "reviewed plugin identity is ambiguous"
    );
    ensure!(
        plugin_manifest_digests_match(&snapshot.manifest_hash, &request.expected_manifest_hash),
        "plugin manifest changed; review the current declaration"
    );
    let capability_digest = snapshot.capability_digest()?;
    ensure!(
        capability_digest == request.expected_capability_digest,
        "plugin capabilities changed; review the current declaration"
    );
    let trust =
        PluginTrustEntry::for_snapshot(&snapshot, request.decision, crate::current_unix_time_ms())?;
    let mut receipt = ApplicationPluginDecisionReceipt {
        plugin_id: trust.plugin_id.clone(),
        manifest_hash: trust.manifest_hash.clone(),
        capability_digest,
        decision: trust.decision,
        reviewed_at_ms: trust.reviewed_at_ms,
        trust_event_id: String::new(),
        process_cleanup: None,
    };
    let decision = ControlEntry::PluginTrustDecision(trust);
    session.ensure_application_operation_binding_target(
        &sigil_kernel::ApplicationOperationTargetV1::ReviewPlugin {
            plugin_id: request.plugin_id.clone(),
            manifest_hash: request.expected_manifest_hash.clone(),
            capability_digest: request.expected_capability_digest.clone(),
            decision: request.decision,
        },
    )?;
    let controls = vec![ControlEntry::PluginManifestCaptured(snapshot), decision];
    let events = session
        .append_controls_with_events(controls.clone())
        .map_err(crate::application_operation_owner::ApplicationPublicationError)?;
    receipt.trust_event_id = events
        .last()
        .context("plugin trust publication has no durable event")
        .map_err(crate::application_operation_owner::ApplicationPublicationError)?
        .event_id
        .clone();
    Ok((receipt, controls))
}

fn observe_attached_session(
    attachment: &InteractiveSessionAttachmentLease,
    expected_scope: &str,
) -> Result<Session> {
    ensure!(
        !expected_scope.is_empty(),
        "plugin review requires an exact session scope"
    );
    let owner = attachment
        .application_operation_owner()
        .context("plugin review requires the attached application writer")?;
    observe_owned_session(&owner, expected_scope)
}

fn observe_owned_session(
    owner: &sigil_kernel::SessionApplicationOperationOwner,
    expected_scope: &str,
) -> Result<Session> {
    ensure!(
        !expected_scope.is_empty(),
        "plugin review requires an exact session scope"
    );
    let session = owner.attach_for_observation()?;
    ensure!(
        session.session_scope_id() == expected_scope,
        "plugin review session scope mismatch"
    );
    Ok(session)
}

fn catalog_for_session(
    session: &Session,
    workspace_root: &Path,
) -> Result<ApplicationPluginCatalog> {
    let trust = PluginStateProjection::from_entries(session.entries())
        .trust_entries
        .into_values()
        .collect::<Vec<_>>();
    let report = discover_workspace_plugins(workspace_root, &trust)?;
    Ok(ApplicationPluginCatalog {
        manifests: report.manifests,
        warnings: report.warnings,
        process_cleanup: cleanup::application_plugin_cleanup_projection(
            &session.read_durable_event_records()?,
        )?,
    })
}

mod cleanup;
pub use cleanup::{application_plugin_cleanup_projection, settle_application_plugin_decision};

#[cfg(test)]
#[path = "tests/plugin_management_tests.rs"]
mod tests;
