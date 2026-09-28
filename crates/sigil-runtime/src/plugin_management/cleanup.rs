//! Exact trust-event cleanup projection and the existing owners' post-review settlement.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use sigil_kernel::{
    ControlEntry, DurableEventType, ExtensionProcessLifecycleAudit,
    ExtensionProcessLifecycleStatus, PluginCleanupStatus, PluginReviewCompletedV1,
    PluginTrustDecision, Session, SessionLogEntry, SessionStreamRecord,
};

use super::ApplicationPluginDecisionReceipt;
use crate::PreparedPluginMcpRetirement;

type Publication = Result<(ApplicationPluginDecisionReceipt, Vec<ControlEntry>)>;

/// Joins every captured owner after publication, including an ambiguous durable append failure.
/// A known preflight rejection releases capture fences without stopping otherwise valid work.
/// The returned controls are already durable; adapters may replay but must not append them.
///
/// # Errors
/// Returns publication/read/append errors only after required physical cleanup was attempted.
/// A confirmed trust decision with failed cleanup returns its explicit `Unconfirmed` result.
pub async fn settle_application_plugin_decision(
    session: &mut Session,
    publication: Publication,
    retirements: Vec<PreparedPluginMcpRetirement>,
) -> Result<(
    ApplicationPluginDecisionReceipt,
    Vec<ControlEntry>,
    Option<String>,
)> {
    if publication.as_ref().is_err_and(|error| {
        !error.is::<crate::application_operation_owner::ApplicationPublicationError>()
    }) {
        return publication.map(|(receipt, controls)| (receipt, controls, None));
    }
    let mut failures = Vec::new();
    for retirement in retirements {
        if let Err(error) = retirement.settle().await {
            failures.push(format!("{error:#}"));
        }
    }
    let cleanup_error = (!failures.is_empty()).then(|| failures.join("; "));
    let (mut receipt, mut controls) = publication.map_err(|error| {
        if let Some(cleanup) = &cleanup_error {
            error.context(format!("plugin process cleanup also failed: {cleanup}"))
        } else {
            error
        }
    })?;
    let records = session
        .read_durable_event_records()
        .map_err(crate::application_operation_owner::ApplicationPublicationError)?;
    receipt.process_cleanup = if receipt.decision == PluginTrustDecision::Disabled {
        Some(if cleanup_error.is_some() {
            PluginCleanupStatus::Unconfirmed
        } else {
            disabled_cleanup_status(&records, &receipt)
                .map_err(crate::application_operation_owner::ApplicationPublicationError)?
        })
    } else {
        // Enabling is not evidence that an older unconfirmed generation stopped.
        application_plugin_cleanup_projection(&records)
            .map_err(crate::application_operation_owner::ApplicationPublicationError)?
            .get(&receipt.plugin_id)
            .copied()
            .filter(|status| *status != PluginCleanupStatus::Confirmed)
    };
    let completed = ControlEntry::PluginReviewCompletedV1(PluginReviewCompletedV1 {
        plugin_id: receipt.plugin_id.clone(),
        manifest_hash: receipt.manifest_hash.clone(),
        capability_digest: receipt.capability_digest.clone(),
        decision: receipt.decision,
        trust_event_id: receipt.trust_event_id.clone(),
        process_cleanup: receipt.process_cleanup,
    });
    session
        .ensure_application_operation_target(&completed)
        .map_err(crate::application_operation_owner::ApplicationPublicationError)?;
    session
        .append_control(completed.clone())
        .map_err(crate::application_operation_owner::ApplicationPublicationError)?;
    controls.push(completed);
    Ok((receipt, controls, cleanup_error))
}

/// Projects only results referring to the exact trust event. A later enable cannot erase an
/// unresolved disable, and a late older result cannot overwrite a newer disable's status.
///
/// # Errors
/// Returns an error when the durable control stream cannot be decoded.
pub fn application_plugin_cleanup_projection(
    records: &[SessionStreamRecord],
) -> Result<BTreeMap<String, PluginCleanupStatus>> {
    let mut latest_disable = BTreeMap::<String, String>::new();
    let mut status = BTreeMap::new();
    for record in records {
        match record.session_log_entry()? {
            Some(SessionLogEntry::Control(ControlEntry::PluginTrustDecision(trust)))
                if trust.decision == PluginTrustDecision::Disabled =>
            {
                latest_disable.insert(trust.plugin_id.clone(), record.event_id().to_owned());
                status.insert(trust.plugin_id, PluginCleanupStatus::Unknown);
            }
            Some(SessionLogEntry::Control(ControlEntry::PluginReviewCompletedV1(result)))
                if result.decision == PluginTrustDecision::Disabled
                    && latest_disable.get(&result.plugin_id) == Some(&result.trust_event_id) =>
            {
                if let Some(cleanup) = result.process_cleanup {
                    status.insert(result.plugin_id, cleanup);
                }
            }
            _ => {}
        }
    }
    Ok(status)
}

fn disabled_cleanup_status(
    records: &[SessionStreamRecord],
    receipt: &ApplicationPluginDecisionReceipt,
) -> Result<PluginCleanupStatus> {
    let boundary = records
        .iter()
        .position(|record| record.event_id() == receipt.trust_event_id)
        .context("plugin cleanup lost its exact trust event")?;
    let unscoped_gap = has_unscoped_cleanup_gap(&records[..boundary], &receipt.plugin_id)?;
    let mut generations = BTreeMap::new();
    for (index, record) in records.iter().enumerate() {
        let event = record.stored_event();
        if event.event_type != DurableEventType::ExtensionProcessLifecycleRecorded.as_str() {
            continue;
        }
        let audit: ExtensionProcessLifecycleAudit = serde_json::from_value(event.payload.clone())?;
        if audit.process_kind != "mcp_stdio"
            || audit
                .safe_metadata
                .get("mcp_config_origin")
                .map(String::as_str)
                != Some("plugin")
            || audit.safe_metadata.get("mcp_config_origin_id") != Some(&receipt.plugin_id)
        {
            continue;
        }
        let Some(generation) = audit.safe_metadata.get("process_generation") else {
            continue;
        };
        let key = (audit.subject, generation.clone());
        // Later re-enabled generations are outside this disable's obligation. Already captured
        // initializing owners that cross this boundary are independently joined above.
        if index < boundary || generations.contains_key(&key) {
            generations.insert(key, audit.status);
        }
    }
    if generations
        .values()
        .any(|state| *state == ExtensionProcessLifecycleStatus::StopUnconfirmed)
    {
        return Ok(PluginCleanupStatus::Unconfirmed);
    }
    if generations.values().any(|state| {
        matches!(
            state,
            ExtensionProcessLifecycleStatus::Starting | ExtensionProcessLifecycleStatus::Running
        )
    }) {
        return Ok(PluginCleanupStatus::Unknown);
    }
    if unscoped_gap {
        // In particular, process restart + empty weak registry does not prove a lost startup
        // owner was joined. Keep the absence of evidence visible on an explicit retry.
        return Ok(PluginCleanupStatus::Unknown);
    }
    Ok(PluginCleanupStatus::Confirmed)
}

// Retain an older disable whose unknown owner never had an exact durable generation. A later
// unrelated stopped generation cannot supply that missing range of evidence.
fn has_unscoped_cleanup_gap(records: &[SessionStreamRecord], plugin_id: &str) -> Result<bool> {
    let mut active = BTreeMap::new();
    let mut disables = BTreeMap::new();
    let mut results = BTreeMap::new();
    for record in records {
        let event = record.stored_event();
        if event.event_type == DurableEventType::ExtensionProcessLifecycleRecorded.as_str() {
            let audit: ExtensionProcessLifecycleAudit =
                serde_json::from_value(event.payload.clone())?;
            if audit.process_kind == "mcp_stdio"
                && audit
                    .safe_metadata
                    .get("mcp_config_origin")
                    .map(String::as_str)
                    == Some("plugin")
                && audit
                    .safe_metadata
                    .get("mcp_config_origin_id")
                    .map(String::as_str)
                    == Some(plugin_id)
                && let Some(generation) = audit.safe_metadata.get("process_generation")
            {
                let key = (audit.subject, generation.clone());
                match audit.status {
                    ExtensionProcessLifecycleStatus::Starting
                    | ExtensionProcessLifecycleStatus::Running
                    | ExtensionProcessLifecycleStatus::StopUnconfirmed => {
                        active.insert(key, ());
                    }
                    ExtensionProcessLifecycleStatus::Stopped => {
                        active.remove(&key);
                    }
                    _ => {}
                }
            }
        }
        match record.session_log_entry()? {
            Some(SessionLogEntry::Control(ControlEntry::PluginTrustDecision(trust)))
                if trust.plugin_id == plugin_id
                    && trust.decision == PluginTrustDecision::Disabled =>
            {
                disables.insert(record.event_id().to_owned(), active.is_empty());
            }
            Some(SessionLogEntry::Control(ControlEntry::PluginReviewCompletedV1(result)))
                if result.plugin_id == plugin_id
                    && result.decision == PluginTrustDecision::Disabled =>
            {
                results.insert(result.trust_event_id, result.process_cleanup);
            }
            _ => {}
        }
    }
    Ok(disables.iter().any(|(event, unscoped)| {
        *unscoped && results.get(event).copied().flatten() != Some(PluginCleanupStatus::Confirmed)
    }))
}
