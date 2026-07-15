use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) struct TailRecoveryIntent {
    pub(super) original_size: u64,
    pub(super) recovered_size: u64,
    pub(super) discarded_bytes: u64,
    pub(super) quarantine_path: PathBuf,
    pub(super) original_hash: String,
    pub(super) event_id: String,
    pub(super) session_id: String,
}

pub(super) struct RecoveredSessionStream {
    pub(super) records: Vec<SessionStreamRecord>,
    pub(super) content: Vec<u8>,
}

pub(super) fn recover_tail_if_needed_locked(
    file: &mut File,
    path: &Path,
) -> Result<RecoveredSessionStream> {
    if let Some(intent) = read_tail_recovery_intent(path)? {
        match read_stream_records_from_file(file, path) {
            Ok(records) => {
                if records.iter().any(|record| {
                    matches!(
                        record,
                        SessionStreamRecord::Stored(event)
                            if event.event_type == DurableEventType::LogTailRecovered.as_str()
                                && event.event_id == intent.event_id
                    )
                }) {
                    clear_tail_recovery_intent(path)?;
                    return recovered_stream_from_records(file, path, records);
                }
                validate_pending_tail_recovery_prefix(file, path, &intent, &records)?;
            }
            Err(read_error) => {
                if is_unsupported_legacy_session_error(&read_error) {
                    return Err(read_error);
                }
                recover_from_pending_tail_intent(file, path, &intent)
                    .with_context(|| read_error.to_string())?;
                let records = read_stream_records_from_file(file, path)?;
                validate_pending_tail_recovery_prefix(file, path, &intent, &records)?;
            }
        }
        append_tail_recovery_event_locked(file, path, &intent)?;
        clear_tail_recovery_intent(path)?;
        return read_recovered_stream_from_file(file, path);
    }

    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let Some(corruption) = tail_corruption(path, &content)? else {
        return Ok(RecoveredSessionStream {
            records: read_stream_records_from_str(path, &content)?,
            content: content.into_bytes(),
        });
    };

    let original_hash = stable_event_hash(content.as_bytes());
    let recovered_content = &content[..corruption.recovered_size as usize];
    let recovered_records = read_stream_records_from_str(path, recovered_content)?;
    let session_id = stream_session_id(&recovered_records).unwrap_or_else(|| {
        stable_event_uuid("sigil-session-path", &path.as_os_str().to_string_lossy())
    });
    let event_id = stable_event_uuid(
        "sigil-tail-recovery",
        &format!(
            "{original_hash}:{}:{}",
            corruption.recovered_size, corruption.discarded_bytes
        ),
    );
    let quarantine_path = quarantine_tail_copy(path, &content, &original_hash)?;
    let intent = TailRecoveryIntent {
        original_size: content.len() as u64,
        recovered_size: corruption.recovered_size,
        discarded_bytes: corruption.discarded_bytes,
        quarantine_path,
        original_hash,
        event_id,
        session_id,
    };
    write_tail_recovery_intent(path, &intent)?;
    file.set_len(intent.recovered_size)
        .with_context(|| format!("failed to truncate {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync truncated {}", path.display()))?;
    append_tail_recovery_event_locked(file, path, &intent)?;
    clear_tail_recovery_intent(path)?;
    read_recovered_stream_from_file(file, path)
}

fn read_recovered_stream_from_file(file: &mut File, path: &Path) -> Result<RecoveredSessionStream> {
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(RecoveredSessionStream {
        records: read_stream_records_from_str(path, &content)?,
        content: content.into_bytes(),
    })
}

fn recovered_stream_from_records(
    file: &mut File,
    path: &Path,
    records: Vec<SessionStreamRecord>,
) -> Result<RecoveredSessionStream> {
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let mut content = Vec::new();
    file.read_to_end(&mut content)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(RecoveredSessionStream { records, content })
}

fn validate_pending_tail_recovery_prefix(
    file: &mut File,
    path: &Path,
    intent: &TailRecoveryIntent,
    records: &[SessionStreamRecord],
) -> Result<()> {
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let mut current = Vec::new();
    file.read_to_end(&mut current)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if current.len() as u64 != intent.recovered_size {
        bail!(
            "tail recovery intent recovered prefix length changed: expected {}, got {}",
            intent.recovered_size,
            current.len()
        );
    }

    let quarantine = fs::read(&intent.quarantine_path).with_context(|| {
        format!(
            "failed to read tail recovery quarantine {}",
            intent.quarantine_path.display()
        )
    })?;
    if stable_event_hash(&quarantine) != intent.original_hash {
        bail!("tail recovery quarantine hash does not match recorded original hash");
    }
    let recovered_size = usize::try_from(intent.recovered_size)
        .context("tail recovery intent recovered_size does not fit usize")?;
    let expected_prefix = quarantine
        .get(..recovered_size)
        .context("tail recovery quarantine is shorter than the recovered prefix")?;
    if current != expected_prefix {
        bail!("tail recovery intent current stream does not match quarantined recovered prefix");
    }
    if let Some(session_id) = stream_session_id(records)
        && session_id != intent.session_id
    {
        bail!("tail recovery intent session_id does not match recovered stream");
    }
    Ok(())
}

pub(super) fn recover_from_pending_tail_intent(
    file: &mut File,
    path: &Path,
    intent: &TailRecoveryIntent,
) -> Result<()> {
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let current_hash = stable_event_hash(content.as_bytes());
    if current_hash != intent.original_hash {
        bail!(
            "tail recovery intent exists but current log hash does not match recorded original hash"
        );
    }
    if content.len() < intent.recovered_size as usize {
        bail!("tail recovery intent recovered_size is past current log length");
    }
    read_stream_records_from_str(path, &content[..intent.recovered_size as usize])
        .context("tail recovery intent points to invalid recovered prefix")?;
    file.set_len(intent.recovered_size)
        .with_context(|| format!("failed to truncate {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync truncated {}", path.display()))
}

#[derive(Debug, Clone, Copy)]
pub(super) struct TailCorruption {
    pub(super) recovered_size: u64,
    pub(super) discarded_bytes: u64,
}

pub(super) fn tail_corruption(path: &Path, content: &str) -> Result<Option<TailCorruption>> {
    let mut line_start = 0usize;
    let mut physical_line = 1usize;
    let mut non_empty_lines = Vec::new();
    for segment in content.split_inclusive('\n') {
        let line_end = line_start + segment.len();
        let line = segment.trim_end_matches(['\n', '\r']);
        if !line.trim().is_empty() {
            non_empty_lines.push((physical_line, line_start, line_end, line.to_owned()));
        }
        line_start = line_end;
        physical_line += 1;
    }
    for (index, (physical_line, start, _end, line)) in non_empty_lines.iter().enumerate() {
        if record_line_is_valid_or_fail_closed(*physical_line, line, path)? {
            continue;
        }
        if index + 1 == non_empty_lines.len() {
            return Ok(Some(TailCorruption {
                recovered_size: *start as u64,
                discarded_bytes: (content.len() - *start) as u64,
            }));
        }
        bail!("middle corruption in session log {}", path.display());
    }
    Ok(None)
}

pub(super) fn record_line_is_valid_or_fail_closed(
    physical_line: usize,
    line: &str,
    path: &Path,
) -> Result<bool> {
    Ok(classify_session_stream_line(line, path, physical_line)?.is_some())
}

pub(super) fn append_tail_recovery_event_locked(
    file: &mut File,
    _path: &Path,
    intent: &TailRecoveryIntent,
) -> Result<()> {
    let records = read_stream_records_from_file(file, _path)?;
    let next_sequence = records
        .iter()
        .map(SessionStreamRecord::stream_sequence)
        .max()
        .unwrap_or(0)
        + 1;
    let event = StoredEvent::new(
        DurableEventType::LogTailRecovered,
        EventClass::Critical,
        intent.event_id.clone(),
        intent.session_id.clone(),
        next_sequence,
        serde_json::json!({
            "original_size": intent.original_size,
            "recovered_size": intent.recovered_size,
            "discarded_bytes": intent.discarded_bytes,
            "quarantine_path": intent.quarantine_path,
            "original_hash": intent.original_hash,
        }),
    )?;
    append_stored_event_to_locked_file(file, &event)
}

pub(super) fn quarantine_tail_copy(
    path: &Path,
    content: &str,
    original_hash: &str,
) -> Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let dir = parent.join(".sigil-recovery");
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    sync_parent_dir(&dir)?;
    let short_hash = original_hash
        .trim_start_matches("sha256:")
        .chars()
        .take(16)
        .collect::<String>();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("session.jsonl");
    let quarantine_path = dir.join(format!("{file_name}.corrupt.{short_hash}"));
    fs::write(&quarantine_path, content)
        .with_context(|| format!("failed to write {}", quarantine_path.display()))?;
    let quarantine_file = File::open(&quarantine_path)
        .with_context(|| format!("failed to open {}", quarantine_path.display()))?;
    quarantine_file
        .sync_all()
        .with_context(|| format!("failed to sync {}", quarantine_path.display()))?;
    sync_parent_dir(&quarantine_path)?;
    Ok(quarantine_path)
}

pub(super) fn tail_recovery_intent_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("session.jsonl");
    path.with_file_name(format!("{file_name}.tail-recovery-intent"))
}

pub(super) fn read_tail_recovery_intent(path: &Path) -> Result<Option<TailRecoveryIntent>> {
    let intent_path = tail_recovery_intent_path(path);
    if !intent_path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&intent_path)
        .with_context(|| format!("failed to read {}", intent_path.display()))?;
    let intent = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", intent_path.display()))?;
    Ok(Some(intent))
}

pub(super) fn write_tail_recovery_intent(path: &Path, intent: &TailRecoveryIntent) -> Result<()> {
    let intent_path = tail_recovery_intent_path(path);
    let content = serde_json::to_vec(intent).context("failed to serialize tail recovery intent")?;
    fs::write(&intent_path, content)
        .with_context(|| format!("failed to write {}", intent_path.display()))?;
    let file = File::open(&intent_path)
        .with_context(|| format!("failed to open {}", intent_path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", intent_path.display()))?;
    sync_parent_dir(&intent_path)
}

pub(super) fn clear_tail_recovery_intent(path: &Path) -> Result<()> {
    let intent_path = tail_recovery_intent_path(path);
    if intent_path.exists() {
        fs::remove_file(&intent_path)
            .with_context(|| format!("failed to remove {}", intent_path.display()))?;
        sync_parent_dir(&intent_path)?;
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn sync_parent_dir(path: &Path) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let dir = File::open(parent).with_context(|| format!("failed to open {}", parent.display()))?;
    dir.sync_all()
        .with_context(|| format!("failed to sync {}", parent.display()))
}

#[cfg(not(unix))]
pub(super) fn sync_parent_dir(_path: &Path) -> Result<()> {
    // Rust's standard library cannot open and fsync directory handles on Windows. The data file
    // and recovery-intent files are still synced before this boundary; directory-entry flushing
    // remains an explicit platform limitation instead of making every durable append fail.
    Ok(())
}
