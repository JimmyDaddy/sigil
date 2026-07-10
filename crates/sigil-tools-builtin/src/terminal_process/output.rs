use super::*;

pub(super) async fn read_terminal_output_log(
    task_id: TerminalTaskId,
    path: &Path,
    offset: u64,
    limit_bytes: usize,
) -> Result<TerminalReadResult> {
    let mut file = File::open(path)
        .await
        .with_context(|| format!("failed to open {}", path.display()))?;
    let total_bytes = file
        .metadata()
        .await
        .with_context(|| format!("failed to inspect {}", path.display()))?
        .len();
    let start = offset.min(total_bytes);
    file.seek(SeekFrom::Start(start))
        .await
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let max_len = (total_bytes - start).min(limit_bytes as u64) as usize;
    let mut buffer = vec![0u8; max_len];
    if max_len > 0 {
        file.read_exact(&mut buffer)
            .await
            .with_context(|| format!("failed to read {}", path.display()))?;
    }
    let returned_bytes = buffer.len() as u64;
    let next_offset = start + returned_bytes;
    Ok(TerminalReadResult {
        task_id,
        offset: start,
        next_offset: (next_offset < total_bytes).then_some(next_offset),
        latest_entry: None,
        content: String::from_utf8_lossy(&buffer).to_string(),
        returned_bytes,
        total_bytes,
        truncated: next_offset < total_bytes,
    })
}

pub(super) async fn summarize_log(path: &Path, limit_bytes: usize) -> Result<LogSummary> {
    let mut file = File::open(path)
        .await
        .with_context(|| format!("failed to open {}", path.display()))?;
    let limit_bytes = limit_bytes.max(1);
    let head_limit = limit_bytes / 2;
    let tail_limit = limit_bytes.saturating_sub(head_limit);
    let mut head = Vec::with_capacity(head_limit);
    let mut tail = Vec::with_capacity(tail_limit);
    let mut hasher = Sha256::new();
    let mut total_bytes = 0u64;
    let mut buffer = [0u8; 8192];

    loop {
        let read = file
            .read(&mut buffer)
            .await
            .with_context(|| format!("failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        hasher.update(chunk);
        total_bytes = total_bytes.saturating_add(read as u64);

        let head_remaining = head_limit.saturating_sub(head.len());
        let head_bytes = head_remaining.min(chunk.len());
        head.extend_from_slice(&chunk[..head_bytes]);
        push_bounded_tail(&mut tail, tail_limit, &chunk[head_bytes..]);
    }

    let raw_truncated = total_bytes > limit_bytes as u64;
    let (preview, display_truncated) =
        bounded_log_preview(&head, &tail, total_bytes, limit_bytes, raw_truncated);
    Ok(LogSummary {
        preview,
        sha256: format!("{:x}", hasher.finalize()),
        truncated: raw_truncated || display_truncated,
        total_bytes,
    })
}

fn bounded_log_preview(
    head: &[u8],
    tail: &[u8],
    total_bytes: u64,
    limit_bytes: usize,
    force_notice: bool,
) -> (String, bool) {
    let head = String::from_utf8_lossy(head);
    let tail = String::from_utf8_lossy(tail);
    let combined_len = head.len().saturating_add(tail.len());
    if !force_notice && combined_len <= limit_bytes {
        return (format!("{head}{tail}"), false);
    }

    let notice = format!("[sigil: terminal output truncated; total {total_bytes} bytes]");
    if limit_bytes <= notice.len() {
        let end = crate::support::floor_char_boundary(&notice, limit_bytes);
        return (notice[..end].to_owned(), true);
    }
    let raw_budget = limit_bytes.saturating_sub(notice.len() + 2);
    let head_budget = raw_budget / 2;
    let tail_budget = raw_budget.saturating_sub(head_budget);
    let head_end = crate::support::floor_char_boundary(&head, head_budget.min(head.len()));
    let tail_start =
        crate::support::ceil_char_boundary(&tail, tail.len().saturating_sub(tail_budget));
    (
        format!("{}\n{notice}\n{}", &head[..head_end], &tail[tail_start..]),
        true,
    )
}

fn push_bounded_tail(tail: &mut Vec<u8>, limit_bytes: usize, bytes: &[u8]) {
    if limit_bytes == 0 || bytes.is_empty() {
        return;
    }
    if bytes.len() >= limit_bytes {
        tail.clear();
        tail.extend_from_slice(&bytes[bytes.len() - limit_bytes..]);
        return;
    }
    let overflow = tail
        .len()
        .saturating_add(bytes.len())
        .saturating_sub(limit_bytes);
    if overflow > 0 {
        tail.drain(..overflow);
    }
    tail.extend_from_slice(bytes);
}

#[derive(Debug, Clone)]
pub(super) struct LogSummary {
    pub(super) preview: String,
    pub(super) sha256: String,
    pub(super) truncated: bool,
    pub(super) total_bytes: u64,
}

pub(super) fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}
