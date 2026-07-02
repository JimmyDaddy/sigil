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
}

pub(super) fn check_legacy_workspace_state(
    report: &mut DoctorReport,
    config_path: &Path,
    paths: &crate::SigilPaths,
) {
    let workspace_config = paths.workspace_root.join(WORKSPACE_CONFIG_FILE);
    if workspace_config.exists() && !same_path(&workspace_config, config_path) {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "config:legacy_workspace",
            format!(
                "workspace {} is no longer loaded by default",
                workspace_config.display()
            ),
            Some(format!(
                "move local config to {}, pass --config explicitly, or delete the workspace copy",
                config_path.display()
            )),
        );
    }

    let legacy_sessions = paths
        .workspace_root
        .join(LEGACY_WORKSPACE_STATE_DIR)
        .join(LEGACY_SESSIONS_DIR);
    if legacy_sessions.exists() {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "storage:legacy_sessions",
            format!(
                "legacy workspace sessions remain at {}",
                legacy_sessions.display()
            ),
            Some(format!(
                "migrate sessions to {} and remove the workspace copy",
                paths.session_log_dir.display()
            )),
        );
    }

    let legacy_input_history = paths
        .workspace_root
        .join(LEGACY_WORKSPACE_STATE_DIR)
        .join(LEGACY_INPUT_HISTORY_FILE);
    if legacy_input_history.exists() {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "storage:legacy_input_history",
            format!(
                "legacy workspace input history remains at {}",
                legacy_input_history.display()
            ),
            Some(format!(
                "migrate input history to {} and remove the workspace copy",
                paths.input_history_file.display()
            )),
        );
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
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
    let mut message = format!(
        "{} streams checked, {} records, last_sequence={}, legacy={}, stored={}",
        session_paths.len().saturating_sub(oversized_skipped),
        summary.records,
        summary.last_sequence,
        summary.legacy_records,
        summary.stored_records
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
        report.push_with_remediation(
            DoctorStatus::Warn,
            "session:stream",
            message,
            Some("open a focused session audit for oversized streams instead of loading them during startup diagnostics"),
        );
    } else {
        report.push(DoctorStatus::Ok, "session:stream", message);
    }
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
    legacy_records: usize,
    stored_records: usize,
    last_sequence: u64,
    tail_recovery_events: usize,
}

impl SessionStreamDoctorSummary {
    fn add_records(&mut self, records: Vec<SessionStreamRecord>) {
        for record in records {
            self.records += 1;
            self.last_sequence = self.last_sequence.max(record.stream_sequence());
            match record {
                SessionStreamRecord::Legacy { .. } => {
                    self.legacy_records += 1;
                }
                SessionStreamRecord::Stored(event) => {
                    if event.event_type == DurableEventType::LogTailRecovered.as_str() {
                        self.tail_recovery_events += 1;
                    }
                    self.stored_records += 1;
                }
            }
        }
    }
}
