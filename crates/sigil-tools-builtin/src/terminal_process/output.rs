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
    let bytes = fs::read(path)
        .await
        .with_context(|| format!("failed to read {}", path.display()))?;
    let sha256 = sha256_hex(&bytes);
    let limited = limit_output_bytes(&bytes, limit_bytes.max(1));
    Ok(LogSummary {
        preview: limited.content,
        sha256,
        truncated: limited.truncated,
    })
}

#[derive(Debug, Clone)]
pub(super) struct LogSummary {
    pub(super) preview: String,
    pub(super) sha256: String,
    pub(super) truncated: bool,
}

#[derive(Debug, Clone)]
pub(super) struct LimitedOutput {
    pub(super) content: String,
    pub(super) truncated: bool,
}

pub(super) fn limit_output_bytes(bytes: &[u8], limit_bytes: usize) -> LimitedOutput {
    if bytes.len() <= limit_bytes {
        return LimitedOutput {
            content: String::from_utf8_lossy(bytes).to_string(),
            truncated: false,
        };
    }

    let head_len = limit_bytes / 2;
    let tail_len = limit_bytes.saturating_sub(head_len);
    let omitted = bytes.len().saturating_sub(head_len + tail_len);
    let mut content = String::new();
    content.push_str(&String::from_utf8_lossy(&bytes[..head_len]));
    content.push_str(&format!("\n... truncated {omitted} bytes ...\n"));
    content.push_str(&String::from_utf8_lossy(&bytes[bytes.len() - tail_len..]));
    LimitedOutput {
        content,
        truncated: true,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub(super) fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}
