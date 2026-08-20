use std::collections::{BTreeMap, BTreeSet};

use super::*;

pub(super) fn check_workspace(report: &mut DoctorReport, workspace_root: &Path) -> Option<PathBuf> {
    match fs::canonicalize(workspace_root) {
        Ok(canonical) if canonical.is_dir() => {
            report.push(
                DoctorStatus::Ok,
                "workspace",
                canonical.display().to_string(),
            );
            Some(canonical)
        }
        Ok(canonical) => {
            report.push_with_remediation(
                DoctorStatus::Error,
                "workspace",
                format!("workspace root is not a directory: {}", canonical.display()),
                Some(
                    "set [workspace].root to an existing directory, or launch Sigil from the intended workspace",
                ),
            );
            None
        }
        Err(error) => {
            report.push_with_remediation(
                DoctorStatus::Error,
                "workspace",
                format!(
                    "failed to resolve workspace root {}: {error}",
                    workspace_root.display()
                ),
                Some(
                    "create the workspace directory, update [workspace].root, or launch Sigil from the intended repository",
                ),
            );
            None
        }
    }
}

pub(super) fn check_storage_paths(report: &mut DoctorReport, paths: &crate::SigilPaths) {
    report.push(
        DoctorStatus::Ok,
        "storage:state_root",
        paths.state_root.display().to_string(),
    );
    report.push(
        DoctorStatus::Ok,
        "storage:cache_root",
        paths.cache_root.display().to_string(),
    );
    report.push(
        DoctorStatus::Ok,
        "storage:workspace_state",
        paths.workspace_state_root.display().to_string(),
    );
    report.push(
        DoctorStatus::Ok,
        "storage:project_assets",
        paths.project_assets_root.display().to_string(),
    );
    check_session_log_dir(report, &paths.session_log_dir);
    check_private_storage_file(
        report,
        "session:lifecycle_permissions",
        &paths.session_lifecycle_journal,
    );
}

pub(super) fn check_session_log_dir(report: &mut DoctorReport, session_dir: &Path) {
    if session_dir.is_dir() {
        report.push(
            DoctorStatus::Ok,
            "session:log_dir",
            session_dir.display().to_string(),
        );
        return;
    }
    let Some(parent) = session_dir.parent() else {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "session:log_dir",
            format!("cannot determine parent for {}", session_dir.display()),
            Some("set [session].log_dir to a normal directory path"),
        );
        return;
    };
    if parent.exists() {
        report.push(
            DoctorStatus::Ok,
            "session:log_dir",
            format!("will create {}", session_dir.display()),
        );
    } else {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "session:log_dir",
            format!("parent does not exist for {}", session_dir.display()),
            Some("create the parent directory, or use the default user state directory"),
        );
    }
}

pub(super) fn check_session_streams(report: &mut DoctorReport, session_dir: &Path) {
    let Ok(metadata) = fs::metadata(session_dir) else {
        return;
    };
    if !metadata.is_dir() {
        return;
    }

    let mut session_paths = match session_log_paths(session_dir) {
        Ok(paths) => paths,
        Err(error) => {
            report.push_with_remediation(
                DoctorStatus::Warn,
                "session:stream",
                format!(
                    "failed to inspect session log dir {}: {error}",
                    session_dir.display()
                ),
                Some("check permissions on the session log directory"),
            );
            return;
        }
    };
    let total_streams = session_paths.len();
    if total_streams == 0 {
        report.push(DoctorStatus::Ok, "session:stream", "no session logs yet");
        return;
    }
    session_paths.truncate(MAX_SESSION_STREAMS_DOCTOR_SCAN);
    check_session_stream_permissions(report, &session_paths, total_streams);

    let mut summary = SessionStreamDoctorSummary::default();
    let mut oversized_skipped = 0usize;
    for path in &session_paths {
        if session_stream_too_large_for_doctor(path) {
            oversized_skipped += 1;
            continue;
        }
        match JsonlSessionStore::read_event_records(path) {
            Ok(records) => summary.add_records(records),
            Err(error) => {
                report.push_with_remediation(
                    DoctorStatus::Error,
                    "session:stream",
                    format!("{} failed RFC-0001 stream validation: {error:#}", path.display()),
                    Some(
                        "open the session in writer mode to recover tail corruption, or inspect checksum/sequence mismatch before continuing",
                    ),
                );
                return;
            }
        }
    }

    let skipped = total_streams.saturating_sub(session_paths.len());
    let checked_streams = session_paths.len().saturating_sub(oversized_skipped);
    let mut message = format!(
        "{checked_streams} V2 streams checked, {} records, last_sequence={}",
        summary.records, summary.last_sequence
    );
    if summary.tail_recovery_events > 0 {
        message.push_str(&format!(
            ", tail_recovered={}",
            summary.tail_recovery_events
        ));
    }
    if skipped > 0 {
        message.push_str(&format!(", skipped {skipped} older streams"));
    }
    if oversized_skipped > 0 {
        message.push_str(&format!(
            ", skipped {oversized_skipped} oversized streams over {MAX_SESSION_STREAM_DOCTOR_BYTES} bytes"
        ));
    }
    if oversized_skipped > 0 {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "session:stream",
            message,
            Some(
                "open a focused session audit for oversized streams instead of loading them during startup diagnostics",
            ),
        );
    } else {
        report.push(DoctorStatus::Ok, "session:stream", message);
    }
}

/// Audits the runtime contracts that preserve provider-request and tool-result closure.
pub(super) fn check_cache_runtime_invariants(report: &mut DoctorReport, session_dir: &Path) {
    let Ok(metadata) = fs::metadata(session_dir) else {
        return;
    };
    if !metadata.is_dir() {
        return;
    }
    let Ok(mut paths) = session_log_paths(session_dir) else {
        return;
    };
    paths.truncate(MAX_SESSION_STREAMS_DOCTOR_SCAN);

    let mut provider_attempts = 0usize;
    let mut current_envelopes = 0usize;
    let mut durable_frontiers_proven = 0usize;
    let mut process_local_overlay_required = 0usize;
    let mut legacy_without_envelope = 0usize;
    let mut unfinished_attempts = 0usize;
    let mut open_tool_executions = 0usize;
    let mut unclosed_tool_calls = 0usize;
    let mut orphan_tool_results = 0usize;
    let mut duplicate_tool_call_ids = 0usize;
    let mut duplicate_tool_result_ids = 0usize;
    let mut mismatched_tool_result_names = 0usize;
    for path in paths {
        if session_stream_too_large_for_doctor(&path) {
            continue;
        }
        let records = match JsonlSessionStore::read_event_records(&path) {
            Ok(records) => records,
            Err(_) => continue,
        };
        let projection = match sigil_kernel::ProviderPhysicalAttemptProjection::from_records(
            &records,
        ) {
            Ok(projection) => projection,
            Err(error) => {
                report.push_with_remediation(
                    DoctorStatus::Error,
                    "session:runtime_invariants",
                    format!("provider request envelope or attempt lifecycle is invalid: {error:#}"),
                    Some(
                        "preserve the affected session and inspect its provider-attempt audit before resuming",
                    ),
                );
                return;
            }
        };
        let durable_bytes = fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        for attempt in projection.attempts() {
            provider_attempts += 1;
            if let Some(envelope) = &attempt.entry.request_envelope {
                current_envelopes += 1;
                if envelope.source_frontier.as_ref().is_some_and(|frontier| {
                    frontier.session_id != attempt.session_id()
                        || frontier.durable_end_offset > durable_bytes
                }) {
                    report.push_with_remediation(
                        DoctorStatus::Error,
                        "session:runtime_invariants",
                        "provider request source frontier is outside its owning durable session",
                        Some(
                            "preserve the affected session and inspect the request envelope frontier before resuming",
                        ),
                    );
                    return;
                }
                match envelope.reconstruction_disposition {
                    sigil_kernel::ProviderRequestReconstructionDispositionV1::DurableFrontierAndRuntimeInputs => {
                        let Some(frontier) = envelope.source_frontier.as_ref() else {
                            unreachable!("validated reconstructable envelope has a frontier")
                        };
                        if let Err(error) =
                            JsonlSessionStore::read_event_records_through_provider_frontier(
                                &path, frontier,
                            )
                        {
                            report.push_with_remediation(
                                DoctorStatus::Error,
                                "session:runtime_invariants",
                                format!(
                                    "provider request source frontier failed exact durable-prefix proof: {error:#}"
                                ),
                                Some(
                                    "preserve the affected session and inspect the exact request-envelope byte frontier before resuming",
                                ),
                            );
                            return;
                        }
                        durable_frontiers_proven += 1;
                    }
                    sigil_kernel::ProviderRequestReconstructionDispositionV1::ProcessLocalOverlayRequired => {
                        process_local_overlay_required += 1;
                    }
                    sigil_kernel::ProviderRequestReconstructionDispositionV1::InMemoryOnly => {}
                }
            } else {
                legacy_without_envelope += 1;
            }
            unfinished_attempts += usize::from(attempt.terminal.is_none());
        }

        let mut open = BTreeSet::new();
        let mut open_result_pairs = BTreeMap::<String, String>::new();
        let mut declared_call_ids = BTreeSet::new();
        let mut recorded_result_ids = BTreeSet::new();
        for record in &records {
            let Ok(Some(entry)) = record.session_log_entry() else {
                continue;
            };
            match entry {
                sigil_kernel::SessionLogEntry::Assistant(message) => {
                    for call in message.tool_calls {
                        if !declared_call_ids.insert(call.id.clone()) {
                            duplicate_tool_call_ids += 1;
                        }
                        open_result_pairs.insert(call.id, call.name);
                    }
                }
                sigil_kernel::SessionLogEntry::ToolResultV3(result) => {
                    if !recorded_result_ids.insert(result.call_id.clone()) {
                        duplicate_tool_result_ids += 1;
                    }
                    match open_result_pairs.remove(&result.call_id) {
                        Some(tool_name) if tool_name != result.tool_name => {
                            mismatched_tool_result_names += 1;
                        }
                        Some(_) => {}
                        None => orphan_tool_results += 1,
                    }
                }
                sigil_kernel::SessionLogEntry::Control(
                    sigil_kernel::ControlEntry::ToolExecution(execution),
                ) => match execution.status {
                    sigil_kernel::ToolExecutionStatus::Started => {
                        open.insert(execution.call_id.clone());
                    }
                    sigil_kernel::ToolExecutionStatus::Completed
                    | sigil_kernel::ToolExecutionStatus::Failed
                    | sigil_kernel::ToolExecutionStatus::Cancelled
                    | sigil_kernel::ToolExecutionStatus::Interrupted => {
                        open.remove(&execution.call_id);
                    }
                },
                sigil_kernel::SessionLogEntry::User(_)
                | sigil_kernel::SessionLogEntry::RuntimeContextSnapshotV2(_)
                | sigil_kernel::SessionLogEntry::Control(_) => {}
            }
        }
        open_tool_executions += open.len();
        unclosed_tool_calls += open_result_pairs.len();
    }

    let message = format!(
        "provider_attempts={provider_attempts}, current_envelopes={current_envelopes}, durable_frontiers_proven={durable_frontiers_proven}, process_local_overlay_required={process_local_overlay_required}, legacy_without_envelope={legacy_without_envelope}, unfinished_attempts={unfinished_attempts}, open_tool_executions={open_tool_executions}, unclosed_tool_calls={unclosed_tool_calls}, orphan_tool_results={orphan_tool_results}, duplicate_tool_call_ids={duplicate_tool_call_ids}, duplicate_tool_result_ids={duplicate_tool_result_ids}, mismatched_tool_result_names={mismatched_tool_result_names}"
    );
    if unfinished_attempts > 0
        || open_tool_executions > 0
        || unclosed_tool_calls > 0
        || orphan_tool_results > 0
        || duplicate_tool_call_ids > 0
        || duplicate_tool_result_ids > 0
        || mismatched_tool_result_names > 0
    {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "session:runtime_invariants",
            message,
            Some(
                "open the affected session in writer mode so interrupted provider/tool lifecycles can be reconciled; preserve malformed call/result evidence for audit",
            ),
        );
    } else if legacy_without_envelope > 0 {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "session:runtime_invariants",
            message,
            Some(
                "legacy attempts remain readable but cannot provide ProviderRequestEnvelope V1 reconstruction evidence",
            ),
        );
    } else {
        report.push(DoctorStatus::Ok, "session:runtime_invariants", message);
    }
}

pub(super) fn check_plan_review_compatibility(report: &mut DoctorReport, session_dir: &Path) {
    let Ok(mut paths) = session_log_paths(session_dir) else {
        return;
    };
    paths.truncate(MAX_SESSION_STREAMS_DOCTOR_SCAN);
    let mut current = 0usize;
    let mut legacy_recovered = 0usize;
    let mut unsupported_legacy = 0usize;
    for path in paths {
        if session_stream_too_large_for_doctor(&path) {
            continue;
        }
        let Ok(records) = JsonlSessionStore::read_event_records(&path) else {
            continue;
        };
        let entries = records
            .iter()
            .filter_map(|record| {
                sigil_kernel::conversation_transcript_entry_from_record(record)
                    .ok()
                    .flatten()
            })
            .collect::<Vec<_>>();
        match crate::conversation_display::plan_review_compatibility_from_entries(&entries) {
            crate::conversation_display::PlanReviewCompatibilityStatusV1::NoPlanReview => {}
            crate::conversation_display::PlanReviewCompatibilityStatusV1::Current => current += 1,
            crate::conversation_display::PlanReviewCompatibilityStatusV1::LegacyRecovered => {
                legacy_recovered += 1;
            }
            crate::conversation_display::PlanReviewCompatibilityStatusV1::UnsupportedLegacy => {
                unsupported_legacy += 1;
            }
        }
    }
    let message = format!(
        "current={current}, legacy_read_only_recovered={legacy_recovered}, unsupported_legacy={unsupported_legacy}"
    );
    if unsupported_legacy > 0 {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "session:plan_review_compatibility",
            message,
            Some(
                "preserve the affected session for audit and start a new plan review; Sigil will not guess ambiguous legacy revision lineage",
            ),
        );
    } else {
        report.push(
            DoctorStatus::Ok,
            "session:plan_review_compatibility",
            message,
        );
    }
}

pub(super) fn check_session_route_compatibility(
    report: &mut DoctorReport,
    session_dir: &Path,
    root_config: &RootConfig,
) {
    let Ok(mut paths) = session_log_paths(session_dir) else {
        return;
    };
    paths.truncate(MAX_SESSION_STREAMS_DOCTOR_SCAN);
    let snapshot =
        crate::provider_connections::ResolvedRouteConfigSnapshot::from_root_config(root_config);
    let mut exact = 0usize;
    let mut rebindable = 0usize;
    let mut confirmation = 0usize;
    let mut missing = 0usize;
    let mut invalid = 0usize;
    for path in paths {
        if session_stream_too_large_for_doctor(&path) {
            continue;
        }
        let Ok(records) = JsonlSessionStore::read_event_records(&path) else {
            invalid += 1;
            continue;
        };
        let entries = records
            .iter()
            .filter_map(|record| {
                sigil_kernel::conversation_transcript_entry_from_record(record)
                    .ok()
                    .flatten()
            })
            .collect::<Vec<_>>();
        let Some(route) = doctor_session_route(&entries) else {
            invalid += 1;
            continue;
        };
        let plan = crate::provider_connections::plan_session_route_resume(
            &snapshot,
            &crate::provider_connections::SessionRouteResumeInput {
                route,
                egress_trust_binding: doctor_session_route_trust(&entries),
            },
        );
        match plan {
            crate::provider_connections::SessionRouteResumePlan::Exact { .. } => exact += 1,
            crate::provider_connections::SessionRouteResumePlan::RebindCurrentModel { .. } => {
                rebindable += 1;
            }
            crate::provider_connections::SessionRouteResumePlan::NeedsConfirmation { .. } => {
                confirmation += 1;
            }
            crate::provider_connections::SessionRouteResumePlan::NeedsReplacement { .. } => {
                missing += 1;
            }
            crate::provider_connections::SessionRouteResumePlan::NeedsSetup { .. } => invalid += 1,
        }
    }
    let message = format!(
        "exact={exact}, rebindable={rebindable}, confirmation_required={confirmation}, missing={missing}, invalid={invalid}"
    );
    if invalid > 0 {
        report.push_with_remediation(
            DoctorStatus::Error,
            "session:route_resume",
            message,
            Some("repair provider configuration or inspect the invalid session before resuming"),
        );
    } else if confirmation > 0 || missing > 0 {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "session:route_resume",
            message,
            Some("open the session and confirm or select a connection; its transcript remains readable"),
        );
    } else {
        report.push(DoctorStatus::Ok, "session:route_resume", message);
    }
}

fn doctor_session_route(
    entries: &[sigil_kernel::SessionLogEntry],
) -> Option<sigil_kernel::ResolvedModelRoute> {
    let mut route = None;
    for entry in entries {
        match entry {
            sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::SessionIdentity {
                    resolved_model_route,
                    ..
                },
            ) if route.is_none() => route = resolved_model_route.clone(),
            sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::SessionModelSelected {
                    resolved_model_route,
                    ..
                }
                | sigil_kernel::ControlEntry::SessionRouteRebound {
                    resolved_model_route,
                    ..
                },
            ) => route = Some(resolved_model_route.clone()),
            _ => {}
        }
    }
    route
}

fn doctor_session_route_trust(
    entries: &[sigil_kernel::SessionLogEntry],
) -> Option<sigil_kernel::RouteEgressTrustBinding> {
    let mut fingerprint = None::<String>;
    let mut binding = None;
    for entry in entries {
        match entry {
            sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::SessionIdentity {
                    resolved_model_route,
                    ..
                },
            ) => {
                fingerprint = resolved_model_route
                    .as_ref()
                    .map(|route| route.semantic_fingerprint.clone());
                binding = None;
            }
            sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::SessionModelSelected {
                    resolved_model_route,
                    ..
                }
                | sigil_kernel::ControlEntry::SessionRouteRebound {
                    resolved_model_route,
                    ..
                },
            ) => {
                fingerprint = Some(resolved_model_route.semantic_fingerprint.clone());
                binding = None;
            }
            sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::SessionRouteTrustBound {
                    route_semantic_fingerprint,
                    egress_trust_binding,
                },
            ) if fingerprint.as_deref() == Some(route_semantic_fingerprint.as_str()) => {
                binding = Some(egress_trust_binding.clone());
            }
            _ => {}
        }
    }
    binding
}

fn check_session_stream_permissions(
    report: &mut DoctorReport,
    session_paths: &[PathBuf],
    total_streams: usize,
) {
    let mut exposed = Vec::new();
    for path in session_paths {
        if matches!(
            private_path_permissions_are_restricted(path),
            Ok(false) | Err(_)
        ) {
            exposed.push(path.clone());
        }
        let lease_path = path.with_file_name(format!(
            "{}.writer-lock",
            path.file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("session.jsonl")
        ));
        if lease_path.exists()
            && matches!(
                private_path_permissions_are_restricted(&lease_path),
                Ok(false) | Err(_)
            )
        {
            exposed.push(lease_path);
        }
    }
    if let Some(first) = exposed.first() {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "session:permissions",
            format!(
                "{} owner-only violation(s) in {} recently scanned stream(s), first={}",
                exposed.len(),
                session_paths.len(),
                first.display()
            ),
            Some(
                "open the session in writer mode to repair data and lease files to owner-only permissions",
            ),
        );
    } else {
        let skipped = total_streams.saturating_sub(session_paths.len());
        report.push(
            DoctorStatus::Ok,
            "session:permissions",
            if skipped == 0 {
                format!("{} stream(s) and writer leases are owner-only", session_paths.len())
            } else {
                format!(
                    "{} recent stream(s) and writer leases are owner-only; skipped {skipped} older streams",
                    session_paths.len()
                )
            },
        );
    }
}

fn check_private_storage_file(report: &mut DoctorReport, name: &str, path: &Path) {
    if !path.exists() {
        return;
    }
    match private_path_permissions_are_restricted(path) {
        Ok(true) => report.push(
            DoctorStatus::Ok,
            name,
            format!("{} is owner-only", path.display()),
        ),
        Ok(false) => report.push_with_remediation(
            DoctorStatus::Warn,
            name,
            format!("{} allows access beyond the current user", path.display()),
            Some("run a session lifecycle operation to tighten the journal permissions"),
        ),
        Err(error) => report.push_with_remediation(
            DoctorStatus::Warn,
            name,
            format!("failed to inspect {}: {error:#}", path.display()),
            Some("check that the lifecycle journal is a regular file owned by the current user"),
        ),
    }
}

pub(super) fn check_orchestration_route_disablement(
    report: &mut DoctorReport,
    session_dir: &Path,
    guard: &crate::OrchestrationRouteGuard,
) {
    let Ok(metadata) = fs::metadata(session_dir) else {
        return;
    };
    if !metadata.is_dir() {
        return;
    }
    let Ok(mut session_paths) = session_log_paths(session_dir) else {
        return;
    };
    session_paths.truncate(MAX_SESSION_STREAMS_DOCTOR_SCAN);

    for path in session_paths {
        if session_stream_too_large_for_doctor(&path) {
            continue;
        }
        let Ok(records) = JsonlSessionStore::read_event_records(&path) else {
            continue;
        };
        match route_disablement_from_records(records, guard) {
            Ok(Some(disabled)) => {
                report.push_with_remediation(
                    DoctorStatus::Warn,
                    "orchestration:route",
                    format!(
                        "automatic orchestration disabled route={} build={} invariant={:?} report={}",
                        disabled.route_fingerprint,
                        disabled.sigil_build,
                        disabled.invariant,
                        disabled.report_handle
                    ),
                    Some(
                        "automatic handoff already falls back to manual; use explicit /task while investigating, then install a newly qualified build instead of editing durable history",
                    ),
                );
                return;
            }
            Ok(None) => {}
            Err(error) => {
                report.push_with_remediation(
                    DoctorStatus::Error,
                    "orchestration:route",
                    format!(
                        "{} contains an invalid orchestration route disablement: {error:#}",
                        path.display()
                    ),
                    Some(
                        "preserve the session and inspect the recovery-critical orchestration_route_disabled event before continuing",
                    ),
                );
                return;
            }
        }
    }
}

fn route_disablement_from_records(
    records: Vec<SessionStreamRecord>,
    guard: &crate::OrchestrationRouteGuard,
) -> anyhow::Result<Option<sigil_kernel::OrchestrationRouteDisabledEntry>> {
    for record in records {
        let event = record.stored_event();
        if event.event_type != DurableEventType::OrchestrationRouteDisabled.as_str() {
            continue;
        }
        let Some(value) = event.payload.get("session_log_entry") else {
            anyhow::bail!("event payload has no session_log_entry");
        };
        let entry = serde_json::from_value::<sigil_kernel::SessionLogEntry>(value.clone())?;
        let sigil_kernel::SessionLogEntry::Control(
            sigil_kernel::ControlEntry::OrchestrationRouteDisabled(disabled),
        ) = entry
        else {
            anyhow::bail!("event payload is not an orchestration route disablement");
        };
        disabled.validate()?;
        if disabled.route_fingerprint == guard.route_fingerprint()
            && disabled.sigil_build == guard.sigil_build()
        {
            return Ok(Some(disabled));
        }
    }
    Ok(None)
}

fn session_log_paths(session_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(session_dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension == "jsonl")
        })
        .collect::<Vec<_>>();
    paths.sort_by(|left, right| {
        session_modified_time(right)
            .cmp(&session_modified_time(left))
            .then_with(|| left.cmp(right))
    });
    Ok(paths)
}

fn session_stream_too_large_for_doctor(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.len() > MAX_SESSION_STREAM_DOCTOR_BYTES)
        .unwrap_or(false)
}

fn session_modified_time(path: &Path) -> std::time::SystemTime {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
}

#[derive(Debug, Default)]
struct SessionStreamDoctorSummary {
    records: usize,
    last_sequence: u64,
    tail_recovery_events: usize,
}

impl SessionStreamDoctorSummary {
    fn add_records(&mut self, records: Vec<SessionStreamRecord>) {
        for record in records {
            self.records += 1;
            self.last_sequence = self.last_sequence.max(record.stream_sequence());
            let event = record.stored_event();
            if event.event_type == DurableEventType::LogTailRecovered.as_str() {
                self.tail_recovery_events += 1;
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/doctor_orchestration_tests.rs"]
mod orchestration_tests;
