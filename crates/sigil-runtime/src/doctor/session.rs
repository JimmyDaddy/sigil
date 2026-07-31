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
