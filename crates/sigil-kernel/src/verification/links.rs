use super::*;

pub(crate) fn verification_receipt_link_from_records(
    receipt: &VerificationReceipt,
    appended_receipt_event: Option<&StoredEvent>,
    records: &[crate::SessionStreamRecord],
) -> Result<VerificationReceiptLinkRecorded> {
    receipt.receipt.validate_source_identity()?;
    let workspace_snapshot_id = receipt
        .receipt
        .workspace_snapshot_id
        .clone()
        .ok_or_else(|| anyhow!("verification receipt is missing workspace_snapshot_id"))?;
    if workspace_snapshot_id != receipt.binding.workspace_snapshot_id {
        bail!("verification receipt snapshot does not match its binding");
    }

    let source_event_present = records.iter().any(|record| {
        matches!(
            record,
            crate::SessionStreamRecord::Stored(event)
                if event.event_id == receipt.receipt.source_event_id
                    && event.event_type == DurableEventType::CheckFinished.as_str()
        )
    });
    if !records.is_empty() && !source_event_present {
        bail!("verification receipt source event is missing from the durable stream");
    }

    let receipt_event = appended_receipt_event
        .and_then(|event| receipt_event_identity(event, &receipt.receipt.receipt_id))
        .or_else(|| {
            records.iter().find_map(|record| match record {
                crate::SessionStreamRecord::Stored(event) => {
                    receipt_event_identity(event, &receipt.receipt.receipt_id)
                }
                crate::SessionStreamRecord::Legacy { .. } => None,
            })
        });
    if (appended_receipt_event.is_some() || !records.is_empty()) && receipt_event.is_none() {
        bail!("verification receipt event is missing from the durable stream");
    }
    let (receipt_event_id, receipt_sequence) = receipt_event.unwrap_or_else(|| {
        (
            receipt.receipt.source_event_id.clone(),
            receipt.receipt.recorded_at_stream_sequence,
        )
    });

    let changeset_link = receipt
        .receipt
        .changeset_id
        .as_deref()
        .and_then(|changeset_id| {
            proven_changeset_apply_event_id(
                records,
                changeset_id,
                &workspace_snapshot_id,
                receipt_sequence,
            )
        });

    let changeset_id = changeset_link
        .as_ref()
        .and(receipt.receipt.changeset_id.clone());
    Ok(VerificationReceiptLinkRecorded {
        receipt_id: receipt.receipt.receipt_id.clone(),
        receipt_event_id,
        scope: receipt.receipt.scope.clone(),
        workspace_snapshot_id,
        changeset_id,
        changeset_apply_event_id: changeset_link,
    })
}

pub(crate) fn verification_failure_locator_from_records(
    check_run: &VerificationCheckRunEntry,
    receipt: Option<&VerificationReceipt>,
    known_command_event_id: Option<&str>,
    records: &[crate::SessionStreamRecord],
) -> Result<Option<VerificationFailureLocatorRecorded>> {
    if !matches!(
        check_run.status,
        VerificationCheckRunStatus::Failed
            | VerificationCheckRunStatus::Inconclusive
            | VerificationCheckRunStatus::Errored
    ) {
        return Ok(None);
    }

    let command_event_id = known_command_event_id.map(str::to_owned).or_else(|| {
        receipt.and_then(|receipt| {
            records.iter().find_map(|record| match record {
                crate::SessionStreamRecord::Stored(event)
                    if event.event_id == receipt.receipt.source_event_id
                        && event.event_type == DurableEventType::CheckFinished.as_str() =>
                {
                    event
                        .payload
                        .get("command_event_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                }
                crate::SessionStreamRecord::Legacy { .. }
                | crate::SessionStreamRecord::Stored(_) => None,
            })
        })
    });
    let summary = check_run
        .reason
        .as_deref()
        .or_else(|| receipt.and_then(|receipt| receipt.failure_reason.as_deref()))
        .unwrap_or(match check_run.status {
            VerificationCheckRunStatus::Failed => "verification check failed",
            VerificationCheckRunStatus::Inconclusive => "verification check was inconclusive",
            VerificationCheckRunStatus::Errored => "verification check errored",
            VerificationCheckRunStatus::Queued
            | VerificationCheckRunStatus::Running
            | VerificationCheckRunStatus::Succeeded
            | VerificationCheckRunStatus::Skipped => unreachable!("filtered above"),
        });

    Ok(Some(VerificationFailureLocatorRecorded {
        check_run_id: check_run.run_id.clone(),
        receipt_id: receipt.map(|receipt| receipt.receipt.receipt_id.clone()),
        command_event_id,
        output_artifact_id: receipt
            .and_then(|receipt| receipt.receipt.artifact_refs.first().cloned()),
        summary: crate::safe_persistence_text(summary),
    }))
}

fn proven_changeset_apply_event_id(
    records: &[crate::SessionStreamRecord],
    changeset_id: &str,
    workspace_snapshot_id: &str,
    receipt_sequence: u64,
) -> Option<EventId> {
    let lineage_sequence = records.iter().rev().find_map(|record| match record {
        crate::SessionStreamRecord::Stored(event)
            if event.stream_sequence < receipt_sequence
                && event.event_type == DurableEventType::ChildChangesetMerged.as_str()
                && event
                    .payload
                    .get("changeset_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(changeset_id)
                && event
                    .payload
                    .get("parent_workspace_snapshot_after_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(workspace_snapshot_id) =>
        {
            Some(event.stream_sequence)
        }
        crate::SessionStreamRecord::Legacy { .. } | crate::SessionStreamRecord::Stored(_) => None,
    })?;

    records.iter().rev().find_map(|record| match record {
        crate::SessionStreamRecord::Stored(event) if event.stream_sequence < lineage_sequence => {
            let entry = control_entry_from_stored_event(event)?;
            let ControlEntry::ChangeSetApplied(result) = entry else {
                return None;
            };
            (result.id.as_str() == changeset_id
                && result.status == crate::ChangeSetResultStatus::Applied)
                .then(|| event.event_id.clone())
        }
        crate::SessionStreamRecord::Legacy { .. } | crate::SessionStreamRecord::Stored(_) => None,
    })
}

fn control_entry_from_stored_event(event: &StoredEvent) -> Option<ControlEntry> {
    let value = event.payload.get("session_log_entry")?.clone();
    let SessionLogEntry::Control(control) = serde_json::from_value(value).ok()? else {
        return None;
    };
    Some(control)
}

fn receipt_event_identity(event: &StoredEvent, receipt_id: &str) -> Option<(EventId, u64)> {
    if event.event_type != DurableEventType::VerificationRecorded.as_str() {
        return None;
    }
    let ControlEntry::VerificationRecorded(recorded) = control_entry_from_stored_event(event)?
    else {
        return None;
    };
    (recorded.receipt.receipt.receipt_id == receipt_id)
        .then(|| (event.event_id.clone(), event.stream_sequence))
}
