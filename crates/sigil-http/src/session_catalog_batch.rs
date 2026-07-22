use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use sha2::{Digest, Sha256};
use sigil_runtime::{
    LocalSessionCatalogState, LocalSessionMutationError, SessionCatalogProjectionEntry,
    SessionCatalogProjectionService,
};
use thiserror::Error as ThisError;

use crate::{
    HttpRegistryError, HttpSessionRunRegistry,
    dto::{
        HttpSessionCatalogBatchAction, HttpSessionCatalogBatchExecuteRequest,
        HttpSessionCatalogBatchItem, HttpSessionCatalogBatchOutcome, HttpSessionCatalogBatchPlan,
        HttpSessionCatalogBatchPlanItem, HttpSessionCatalogBatchPlanRequest,
        HttpSessionCatalogBatchPlanStatus, HttpSessionCatalogBatchReceipt,
        HttpSessionCatalogBatchReceiptItem,
    },
};

const MAX_BATCH_ITEMS: usize = 100;
const MAX_BATCH_REFERENCE_BYTES: usize = 512;
const MAX_BATCH_PLAN_ID_BYTES: usize = 128;

#[derive(Debug, ThisError)]
pub(crate) enum SessionCatalogBatchError {
    #[error("invalid session catalog batch request: {0}")]
    InvalidRequest(String),
    #[error("session catalog batch plan is stale")]
    StalePlan,
    #[error("session catalog is unavailable")]
    Unavailable,
}

pub(crate) fn plan_session_catalog_batch(
    catalog: &SessionCatalogProjectionService,
    registry: &HttpSessionRunRegistry,
    request: &HttpSessionCatalogBatchPlanRequest,
) -> Result<HttpSessionCatalogBatchPlan, SessionCatalogBatchError> {
    validate_request(request.action, &request.items)?;
    let (generation, entries) = stable_catalog_snapshot(catalog)?;
    let entries = entries
        .iter()
        .map(|entry| (entry.session_ref.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    let items = request
        .items
        .iter()
        .map(|item| {
            let reason = if !seen.insert(item.session_ref.as_str()) {
                Some("duplicate")
            } else {
                classify_item(
                    request.action,
                    item,
                    entries.get(item.session_ref.as_str()).copied(),
                    registry,
                )
            };
            HttpSessionCatalogBatchPlanItem {
                session_ref: item.session_ref.clone(),
                status: if reason.is_some() {
                    HttpSessionCatalogBatchPlanStatus::Blocked
                } else {
                    HttpSessionCatalogBatchPlanStatus::Executable
                },
                reason: reason.map(str::to_owned),
            }
        })
        .collect::<Vec<_>>();
    let executable = items
        .iter()
        .filter(|item| item.status == HttpSessionCatalogBatchPlanStatus::Executable)
        .count();
    let mut plan = HttpSessionCatalogBatchPlan {
        plan_id: String::new(),
        action: request.action,
        generation,
        total: items.len(),
        executable,
        blocked: items.len().saturating_sub(executable),
        items,
    };
    plan.plan_id = plan_digest(request, &plan)?;
    Ok(plan)
}

fn stable_catalog_snapshot(
    catalog: &SessionCatalogProjectionService,
) -> Result<(u64, Vec<SessionCatalogProjectionEntry>), SessionCatalogBatchError> {
    // Planning may inspect entries outside the projection lock, so prove that the
    // generation did not move while the snapshot was copied. Execution plans again
    // and compares the digest, which closes the remaining post-plan race.
    for _ in 0..3 {
        let before = catalog
            .reconcile()
            .map_err(|_| SessionCatalogBatchError::Unavailable)?
            .generation;
        let entries = catalog
            .list_workspace_entries()
            .map_err(|_| SessionCatalogBatchError::Unavailable)?;
        let after = catalog
            .reconcile()
            .map_err(|_| SessionCatalogBatchError::Unavailable)?
            .generation;
        if before == after {
            return Ok((after, entries));
        }
    }
    Err(SessionCatalogBatchError::Unavailable)
}

pub(crate) fn execute_session_catalog_batch(
    catalog: &SessionCatalogProjectionService,
    registry: &HttpSessionRunRegistry,
    request: &HttpSessionCatalogBatchExecuteRequest,
) -> Result<HttpSessionCatalogBatchReceipt, SessionCatalogBatchError> {
    if request.plan_id.is_empty() || request.plan_id.len() > MAX_BATCH_PLAN_ID_BYTES {
        return Err(SessionCatalogBatchError::InvalidRequest(
            "plan id is missing or too long".to_owned(),
        ));
    }
    let plan_request = HttpSessionCatalogBatchPlanRequest {
        action: request.action,
        items: request.items.clone(),
    };
    let plan = plan_session_catalog_batch(catalog, registry, &plan_request)?;
    if plan.plan_id != request.plan_id {
        return Err(SessionCatalogBatchError::StalePlan);
    }

    let mut receipts = Vec::with_capacity(request.items.len());
    for (item, planned) in request.items.iter().zip(&plan.items) {
        if planned.status == HttpSessionCatalogBatchPlanStatus::Blocked {
            receipts.push(HttpSessionCatalogBatchReceiptItem {
                session_ref: item.session_ref.clone(),
                outcome: HttpSessionCatalogBatchOutcome::Skipped,
                reason: planned.reason.clone(),
                operation_id: None,
                quarantine_name: None,
                projection_generation: None,
            });
            continue;
        }
        receipts.push(execute_item(catalog, registry, request.action, item));
    }
    let completed = count_outcome(&receipts, HttpSessionCatalogBatchOutcome::Completed);
    let failed = count_outcome(&receipts, HttpSessionCatalogBatchOutcome::Failed);
    let skipped = count_outcome(&receipts, HttpSessionCatalogBatchOutcome::Skipped);
    Ok(HttpSessionCatalogBatchReceipt {
        plan_id: request.plan_id.clone(),
        action: request.action,
        total: receipts.len(),
        completed,
        failed,
        skipped,
        items: receipts,
    })
}

fn validate_request(
    action: HttpSessionCatalogBatchAction,
    items: &[HttpSessionCatalogBatchItem],
) -> Result<(), SessionCatalogBatchError> {
    if items.is_empty() || items.len() > MAX_BATCH_ITEMS {
        return Err(SessionCatalogBatchError::InvalidRequest(format!(
            "batch must contain between 1 and {MAX_BATCH_ITEMS} items"
        )));
    }
    for item in items {
        if item.session_ref.is_empty()
            || item.session_ref.len() > MAX_BATCH_REFERENCE_BYTES
            || item.session_ref.trim() != item.session_ref
        {
            return Err(SessionCatalogBatchError::InvalidRequest(
                "session reference is invalid".to_owned(),
            ));
        }
        let valid_shape = match action {
            HttpSessionCatalogBatchAction::DeleteSessions => {
                item.session_id.as_ref().is_some_and(|value| {
                    !value.is_empty() && value.len() <= MAX_BATCH_REFERENCE_BYTES
                }) && item.source_bytes.is_none()
                    && item.source_modified_at_unix_ms.is_none()
            }
            HttpSessionCatalogBatchAction::QuarantineInvalidSources
            | HttpSessionCatalogBatchAction::DeleteInvalidSources => {
                item.session_id.is_none()
                    && item.source_bytes.is_some()
                    && item.source_modified_at_unix_ms.is_some()
            }
        };
        if !valid_shape {
            return Err(SessionCatalogBatchError::InvalidRequest(
                "batch item does not match the selected action".to_owned(),
            ));
        }
    }
    Ok(())
}

fn classify_item(
    action: HttpSessionCatalogBatchAction,
    item: &HttpSessionCatalogBatchItem,
    entry: Option<&SessionCatalogProjectionEntry>,
    registry: &HttpSessionRunRegistry,
) -> Option<&'static str> {
    let entry = match entry {
        Some(entry) => entry,
        None => return Some("not_found"),
    };
    match action {
        HttpSessionCatalogBatchAction::DeleteSessions => {
            if entry.source_state != LocalSessionCatalogState::Ready {
                return Some("not_ready");
            }
            let expected_id = match item.session_id.as_deref() {
                Some(session_id) => session_id,
                None => return Some("invalid_request"),
            };
            if entry.session_id.as_deref() != Some(expected_id) {
                return Some("identity_changed");
            }
            if entry.pinned {
                return Some("pinned");
            }
            registry
                .durable_session_mutation_is_blocked(expected_id)
                .then_some("active")
        }
        HttpSessionCatalogBatchAction::QuarantineInvalidSources
        | HttpSessionCatalogBatchAction::DeleteInvalidSources => {
            if entry.source_state != LocalSessionCatalogState::Invalid || entry.session_id.is_some()
            {
                return Some("not_ready");
            }
            (entry.source_bytes != item.source_bytes.unwrap_or_default()
                || entry.source_modified_at_unix_ms
                    != item.source_modified_at_unix_ms.unwrap_or_default())
            .then_some("identity_changed")
        }
    }
}

fn execute_item(
    catalog: &SessionCatalogProjectionService,
    registry: &HttpSessionRunRegistry,
    action: HttpSessionCatalogBatchAction,
    item: &HttpSessionCatalogBatchItem,
) -> HttpSessionCatalogBatchReceiptItem {
    match action {
        HttpSessionCatalogBatchAction::DeleteSessions => {
            let session_id = item.session_id.as_deref().unwrap_or_default();
            let guard = match registry.reserve_durable_session_mutation(session_id) {
                Ok(guard) => guard,
                Err(error) => return failed_receipt(item, registry_error_code(&error)),
            };
            match catalog.delete_session(&item.session_ref, session_id) {
                Ok(receipt) => {
                    guard.finish(true);
                    completed_receipt(
                        item,
                        receipt.operation_id,
                        None,
                        receipt.projection_generation,
                    )
                }
                Err(error) => failed_receipt(item, mutation_error_code(&error)),
            }
        }
        HttpSessionCatalogBatchAction::QuarantineInvalidSources => match catalog
            .quarantine_invalid_source(
                &item.session_ref,
                item.source_bytes.unwrap_or_default(),
                item.source_modified_at_unix_ms.unwrap_or_default(),
            ) {
            Ok(receipt) => completed_receipt(
                item,
                receipt.operation_id,
                Some(receipt.quarantine_name),
                receipt.projection_generation,
            ),
            Err(error) => failed_receipt(item, mutation_error_code(&error)),
        },
        HttpSessionCatalogBatchAction::DeleteInvalidSources => match catalog.delete_invalid_source(
            &item.session_ref,
            item.source_bytes.unwrap_or_default(),
            item.source_modified_at_unix_ms.unwrap_or_default(),
        ) {
            Ok(receipt) => completed_receipt(
                item,
                receipt.operation_id,
                None,
                receipt.projection_generation,
            ),
            Err(error) => failed_receipt(item, mutation_error_code(&error)),
        },
    }
}

fn completed_receipt(
    item: &HttpSessionCatalogBatchItem,
    operation_id: String,
    quarantine_name: Option<String>,
    projection_generation: Option<u64>,
) -> HttpSessionCatalogBatchReceiptItem {
    HttpSessionCatalogBatchReceiptItem {
        session_ref: item.session_ref.clone(),
        outcome: HttpSessionCatalogBatchOutcome::Completed,
        reason: None,
        operation_id: Some(operation_id),
        quarantine_name,
        projection_generation,
    }
}

fn failed_receipt(
    item: &HttpSessionCatalogBatchItem,
    reason: &'static str,
) -> HttpSessionCatalogBatchReceiptItem {
    HttpSessionCatalogBatchReceiptItem {
        session_ref: item.session_ref.clone(),
        outcome: HttpSessionCatalogBatchOutcome::Failed,
        reason: Some(reason.to_owned()),
        operation_id: None,
        quarantine_name: None,
        projection_generation: None,
    }
}

fn count_outcome(
    receipts: &[HttpSessionCatalogBatchReceiptItem],
    outcome: HttpSessionCatalogBatchOutcome,
) -> usize {
    receipts
        .iter()
        .filter(|item| item.outcome == outcome)
        .count()
}

fn mutation_error_code(error: &LocalSessionMutationError) -> &'static str {
    match error {
        LocalSessionMutationError::InvalidRequest => "invalid_request",
        LocalSessionMutationError::NotFound => "not_found",
        LocalSessionMutationError::NotReady => "not_ready",
        LocalSessionMutationError::IdentityChanged => "identity_changed",
        LocalSessionMutationError::Pinned => "pinned",
        LocalSessionMutationError::Unavailable { .. } => "unavailable",
    }
}

fn registry_error_code(error: &HttpRegistryError) -> &'static str {
    match error {
        HttpRegistryError::SessionForegroundRunActive { .. }
        | HttpRegistryError::SessionVerificationActive { .. }
        | HttpRegistryError::DurableSessionMutationActive => "active",
        HttpRegistryError::ServerShuttingDown => "unavailable",
        _ => "unavailable",
    }
}

fn plan_digest(
    request: &HttpSessionCatalogBatchPlanRequest,
    plan: &HttpSessionCatalogBatchPlan,
) -> Result<String, SessionCatalogBatchError> {
    #[derive(Serialize)]
    struct PlanBinding<'a> {
        schema_version: u16,
        request: &'a HttpSessionCatalogBatchPlanRequest,
        generation: u64,
        items: &'a [HttpSessionCatalogBatchPlanItem],
    }
    let encoded = serde_json::to_vec(&PlanBinding {
        schema_version: 1,
        request,
        generation: plan.generation,
        items: &plan.items,
    })
    .map_err(|_| SessionCatalogBatchError::Unavailable)?;
    Ok(format!("sha256:{:x}", Sha256::digest(encoded)))
}
